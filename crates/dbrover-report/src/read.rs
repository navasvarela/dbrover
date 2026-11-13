//! Reading a report file: TOML, checked as a whole. A report with a problem can't run, so
//! unlike `exports.toml` nothing is skipped: every problem found is listed, each a sentence
//! that names where it is (`[report]`, `source "balances"`, `output 2, column "Debit"`).

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use dbrover_export::{ColumnType, CustomFormatId, ExportFormat, IfInvalid, Preset, SchemaColumn};

use dbrover_chart::ChartSpec;
use dbrover_driver_duckdb::DuckDbType;

use crate::partition::read_partition;
use crate::problem::{Place, ReportProblem, Table, messages, syntax_problems};
use crate::{
    ByteSize, Chart, ChartPicture, Combine, CombineLimitsSet, CombineQuery, FileName, HintId,
    Lookup, Output, OutputColumns, OutputLimitsSet, OutputWriter, PART_FROM, PART_TO, Param,
    Partition, PlannedPartition, Report, Source, SourceColumn, SourceLimitsSet, SourcePartition,
    SourceRead, SourceRows, SourceSplit, grouped,
};

/// Keys a file may put in the wrong table, and where each goes, so it says so rather than
/// calling the key unknown.
const MISPLACED_REPORT_KEYS: [(&str, &str); 1] = [(
    "prql",
    "PRQL goes in [combine], as prql = \"from …\", in place of its query",
)];

/// Names DuckDB gives its own catalogs and schemas, which a source's table or attached
/// database can't take.
const RESERVED_IDS: [&str; 4] = ["main", "memory", "system", "temp"];

/// Reads a report file's text.
///
/// # Errors
///
/// Every problem found, each a sentence naming where it is.
pub fn read_report(text: &str) -> Result<Report, Vec<String>> {
    read_with_places(text).map_err(messages)
}

/// Reads a report file's text, as [`read_report`] does.
///
/// # Errors
///
/// Every problem found, each with its place in the file: every TOML syntax error, or else
/// what the file's tables have wrong.
pub fn read_with_places(text: &str) -> Result<Report, Vec<ReportProblem>> {
    let syntax = syntax_problems(text);
    if !syntax.is_empty() {
        return Err(syntax);
    }
    let table = toml::from_str::<toml::Table>(text).map_err(|error| {
        vec![ReportProblem::new(
            error.message().trim(),
            Place::Text(error.span().unwrap_or(0..0)),
        )]
    })?;
    let mut problems = Vec::new();
    let mut file = Fields::new(table);
    let header = file.table("report");
    let sources = file.tables("source");
    let combine = file.table("combine");
    let has_combine = matches!(combine, Ok(Some(_)));
    let outputs = file.tables("output");
    let charts = file.tables("chart");
    for (key, place) in MISPLACED_REPORT_KEYS {
        if file.take(key).is_some() {
            problems.push(at_file(Found::at(key, place)));
        }
    }
    problems.extend(
        file.unknown(
            "a report file",
            "[report], [[source]], [combine], [[output]] and [[chart]]",
        )
        .into_iter()
        .map(at_file),
    );

    let header = match header {
        Ok(Some(header)) => read_header(header, &mut problems),
        Ok(None) => {
            problems.push(at_file(Found::from(
                "the file has no [report] table, with the report's name",
            )));
            None
        }
        Err(problem) => {
            problems.push(at_file(problem));
            None
        }
    };
    let params: BTreeMap<&str, ColumnType> = header
        .as_ref()
        .map(|header| {
            header
                .params
                .iter()
                .map(|param| (param.name.as_str(), param.param_type))
                .collect()
        })
        .unwrap_or_default();

    let (sources, positions) = match sources {
        Ok(sources) => read_sources(sources, &mut problems),
        Err(problem) => {
            problems.push(at_file(problem));
            (Vec::new(), Vec::new())
        }
    };
    check_lookups(&sources, &positions, &params, &mut problems);
    check_partitions(&sources, &positions, &params, has_combine, &mut problems);
    let combine = match combine {
        Ok(Some(combine)) => read_combine(combine, &mut problems),
        Ok(None) => None,
        Err(problem) => {
            problems.push(at_file(problem));
            None
        }
    };
    let combine_problems = problems
        .iter()
        .any(|problem| in_table(problem, |table| *table == Table::Combine));
    if !combine_problems {
        check_shape(&sources, &positions, combine.is_some(), &mut problems);
    }
    let run_partition = header.as_ref().and_then(|header| header.partition.as_ref());
    check_run_partition(run_partition, &params, &sources, &positions, &mut problems);
    let file_params = with_range_params(run_partition, &params);
    let outputs = match outputs {
        Ok(outputs) => read_outputs(outputs, &file_params, &mut problems),
        Err(problem) => {
            problems.push(at_file(problem));
            Vec::new()
        }
    };

    let charts = match charts {
        Ok(charts) => {
            let read = ChartsRead {
                params: &params,
                outputs: &outputs,
                has_combine,
            };
            read_charts(charts, &read, &mut problems)
        }
        Err(problem) => {
            problems.push(at_file(problem));
            Vec::new()
        }
    };

    if run_partition.is_some() {
        for (index, chart) in charts.iter().enumerate() {
            if chart.query.is_some() {
                problems.push(ReportProblem::new(
                    format!(
                        "chart {}: query: in a partitioned run, the sources' tables hold one \
                         range at a time; leave query out to draw the result of every range",
                        index + 1
                    ),
                    Place::key(Table::Chart(index), "query"),
                ));
            }
        }
    }

    match header {
        Some(header) if problems.is_empty() => Ok(Report {
            name: header.name,
            description: header.description,
            params: header.params,
            timeout: header.timeout,
            max_disk: header.max_disk,
            stall_timeout: header.stall_timeout,
            resumable_above: header.resumable_above,
            quiet_hints: header.quiet_hints,
            partition: header.partition,
            sources,
            combine,
            outputs,
            charts,
        }),
        _ => Err(problems),
    }
}

/// A problem found in one of the file's tables, and the key it is about when one is known.
#[derive(Debug)]
struct Found {
    key: Option<String>,
    message: String,
}

impl Found {
    fn at(key: &str, message: impl Into<String>) -> Self {
        Self {
            key: Some(key.to_owned()),
            message: message.into(),
        }
    }
}

impl From<String> for Found {
    fn from(message: String) -> Self {
        Self { key: None, message }
    }
}

impl From<&str> for Found {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// `found` in `table`, each message after `label` (`source "lines": …`) when there is one.
fn placed(
    table: Table,
    label: Option<&str>,
    found: Vec<Found>,
) -> impl Iterator<Item = ReportProblem> + '_ {
    found.into_iter().map(move |found| {
        let message = match label {
            Some(label) => format!("{label}: {}", found.message),
            None => found.message,
        };
        let place = Place::Key {
            table: table.clone(),
            key: found.key,
        };
        ReportProblem::new(message, place)
    })
}

/// What `found` says, for the readers of lists inside a table, which name their item.
fn message(found: Found) -> String {
    found.message
}

/// A problem of the file as a whole.
fn at_file(found: Found) -> ReportProblem {
    let place = Place::Key {
        table: Table::File,
        key: found.key,
    };
    ReportProblem::new(found.message, place)
}

/// Whether `problem` is placed in a table that `which` picks.
fn in_table(problem: &ReportProblem, which: impl Fn(&Table) -> bool) -> bool {
    matches!(&problem.place, Place::Key { table, .. } if which(table))
}

struct Header {
    name: String,
    description: Option<String>,
    params: Vec<Param>,
    timeout: Option<Duration>,
    max_disk: Option<ByteSize>,
    stall_timeout: Option<Duration>,
    resumable_above: Option<ByteSize>,
    quiet_hints: Vec<HintId>,
    partition: Option<Partition>,
}

/// The keys `[report]` has, for the problem of one it hasn't.
const REPORT_KEYS: &str = "name, description, params, timeout, stall_timeout, max_disk, \
                           resumable_above, quiet_hints and partition";

fn read_header(table: toml::Table, problems: &mut Vec<ReportProblem>) -> Option<Header> {
    let mut fields = Fields::new(table);
    let label = "[report]";
    let mut found = Vec::new();
    let name = fields.string("name").and_then(|name| {
        name.filter(|name| !name.trim().is_empty())
            .ok_or_else(|| Found::at("name", "needs a name, as name = \"Trial balance\""))
    });
    let description = fields.string("description");
    let params = fields.table("params").map(Option::unwrap_or_default);
    let timeout = fields.duration("timeout");
    let max_disk = fields.byte_size("max_disk");
    let stall_timeout = fields.duration("stall_timeout");
    let resumable_above = fields.byte_size("resumable_above");
    let quiet_hints = read_quiet_hints(fields.take("quiet_hints"));
    let partition = fields.take("partition").map(read_partition).transpose();
    for (key, place) in MISPLACED_REPORT_KEYS {
        if fields.take(key).is_some() {
            found.push(Found::at(key, place));
        }
    }
    found.extend(fields.unknown("[report]", REPORT_KEYS));
    let params = params.map(|params| read_params(params, &mut found));
    let name = kept(name, &mut found);
    let description = kept(description, &mut found);
    let params = kept(params, &mut found);
    let timeout = kept(timeout, &mut found);
    let max_disk = kept(max_disk, &mut found);
    let stall_timeout = kept(stall_timeout, &mut found);
    let resumable_above = kept(resumable_above, &mut found);
    let partition = partition
        .map_err(|problems| {
            found.extend(
                problems
                    .into_iter()
                    .map(|problem| Found::at("partition", problem)),
            );
        })
        .ok();
    let quiet_hints = quiet_hints
        .map_err(|quiet| {
            found.extend(
                quiet
                    .into_iter()
                    .map(|problem| Found::at("quiet_hints", problem)),
            );
        })
        .ok();
    problems.extend(placed(Table::Report, Some(label), found));
    Some(Header {
        name: name?,
        description: description?.filter(|text| !text.trim().is_empty()),
        params: params?,
        timeout: timeout?,
        max_disk: max_disk?,
        stall_timeout: stall_timeout?,
        resumable_above: resumable_above?,
        quiet_hints: quiet_hints?,
        partition: partition?,
    })
}

/// `result`'s value, or `None` with its problem added to `found`.
fn kept<T>(result: Result<T, Found>, found: &mut Vec<Found>) -> Option<T> {
    result.map_err(|problem| found.push(problem)).ok()
}

/// `quiet_hints`, the ids of the hints the report's runs don't give, each a problem when it
/// names none.
fn read_quiet_hints(value: Option<toml::Value>) -> Result<Vec<HintId>, Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let toml::Value::Array(items) = value else {
        return Err(vec![
            "quiet_hints is a list of hints' ids, as quiet_hints = [\"source-reads-more\"]"
                .to_owned(),
        ]);
    };
    let mut ids = Vec::new();
    let mut problems = Vec::new();
    for item in items {
        match item.as_str().map(str::parse::<HintId>) {
            Some(Ok(id)) if !ids.contains(&id) => ids.push(id),
            Some(Ok(_)) => {}
            Some(Err(unknown)) => problems.push(format!("quiet_hints: {unknown}")),
            None => problems.push(format!(
                "quiet_hints: {item} isn't a hint's id, which is a string"
            )),
        }
    }
    if problems.is_empty() {
        Ok(ids)
    } else {
        Err(problems)
    }
}

fn read_params(table: toml::Table, problems: &mut Vec<Found>) -> Vec<Param> {
    let mut params = Vec::new();
    let mut problem = |message: String| problems.push(Found::at("params", message));
    for (name, value) in table {
        if !is_param_name(&name) {
            problem(format!(
                "params: {name:?} can't be a parameter's name, which is letters, digits and _, \
                 starting with a letter"
            ));
            continue;
        }
        let toml::Value::String(type_name) = value else {
            problem(format!(
                "params: {name}'s type is a string, as {name} = \"date\""
            ));
            continue;
        };
        match type_name.parse::<ColumnType>() {
            Ok(param_type) => params.push(Param { name, param_type }),
            Err(error) => problem(format!("params: {name}: {error}")),
        }
    }
    params
}

/// As `:name` parameters are written in queries, which `dbrover_sql::parameters` finds.
fn is_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Each source, with a problem for each id that can't name a table or that another source
/// already has. A source with a problem is left out, and its problems listed. Also returns
/// each source's position among the file's `[[source]]` tables.
fn read_sources(
    sources: Vec<toml::Table>,
    problems: &mut Vec<ReportProblem>,
) -> (Vec<Source>, Vec<usize>) {
    let mut read: Vec<Source> = Vec::new();
    let mut positions = Vec::new();
    for (position, table) in sources.into_iter().enumerate() {
        let Some(source) = read_source(table, position, problems) else {
            continue;
        };
        if read
            .iter()
            .any(|kept| kept.id.eq_ignore_ascii_case(&source.id))
        {
            problems.push(ReportProblem::new(
                format!(
                    "source {:?}: another source already has this id; each source's id names \
                     its table, so each is different",
                    source.id
                ),
                Place::key(Table::Source(position), "id"),
            ));
            continue;
        }
        read.push(source);
        positions.push(position);
    }
    (read, positions)
}

/// What only the whole list of sources shows: whether there is one, and whether the sources
/// need a combine step they don't have.
fn check_shape(
    sources: &[Source],
    positions: &[usize],
    combined: bool,
    problems: &mut Vec<ReportProblem>,
) {
    let source_problems = problems
        .iter()
        .any(|problem| in_table(problem, |table| matches!(table, Table::Source(_))));
    let problem = match sources {
        [] if !source_problems => Some(ReportProblem::new(
            "the file has no [[source]] table, with the query whose rows the report writes",
            Place::table(Table::File),
        )),
        [one] if !combined && one.read == SourceRead::Attach => Some(ReportProblem::new(
            format!(
                "source {:?}: an attached source is read in place by a [combine] query; add \
                 one, or give the source a query",
                one.id
            ),
            Place::key(
                Table::Source(positions.first().copied().unwrap_or(0)),
                "attach",
            ),
        )),
        [_, _, ..] if !combined => Some(ReportProblem::new(
            format!(
                "the file has {} [[source]] tables; several sources are combined by a \
                 [combine] query over their tables",
                sources.len()
            ),
            Place::key(Table::File, "source"),
        )),
        _ => None,
    };
    problems.extend(problem);
}

fn read_source(
    table: toml::Table,
    position: usize,
    problems: &mut Vec<ReportProblem>,
) -> Option<Source> {
    let mut fields = Fields::new(table);
    let id = fields.string("id");
    let label = match &id {
        Ok(Some(id)) => format!("source {id:?}"),
        _ => "[[source]]".to_owned(),
    };
    let mut found = Vec::new();
    let id = id.and_then(|id| {
        let id = id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| Found::at("id", "needs an id, as id = \"balances\""))?;
        check_id(&id)
            .map(|()| id)
            .map_err(|problem| Found::at("id", problem))
    });
    let data_source = fields.string("data_source").and_then(|name| {
        name.filter(|name| !name.trim().is_empty()).ok_or_else(|| {
            Found::at(
                "data_source",
                "needs the data source it reads, by name: data_source = \"ledger\"",
            )
        })
    });
    let attach = fields.boolean("attach");
    let query = fields.string("query");
    let max_rows = fields.count("max_rows");
    let max_bytes = fields.byte_size("max_bytes");
    let timeout = fields.duration("timeout");
    let split = read_split(&mut fields);
    let columns = fields.take("columns").map(read_source_columns).transpose();
    let columnar = fields.boolean("columnar");
    found.extend(fields.unknown(
        "a source",
        "id, data_source, query, lookup, partition, max_rows_per_part, columns, columnar, \
         attach, max_rows, max_bytes and timeout",
    ));
    let read = match (
        attach, query, max_rows, max_bytes, timeout, split, columns, columnar,
    ) {
        (Ok(Some(true)), query, max_rows, max_bytes, timeout, split, columns, columnar) => {
            if !matches!(query, Ok(None)) {
                found.push(Found::at(
                    "query",
                    "an attached source has no query: the [combine] query reads its tables in \
                     place",
                ));
            }
            let limits = [
                ("max_rows", max_rows.map(|value| value.is_some())),
                ("max_bytes", max_bytes.map(|value| value.is_some())),
                ("timeout", timeout.map(|value| value.is_some())),
            ];
            for (key, set) in limits {
                if !matches!(set, Ok(false)) {
                    found.push(Found::at(
                        key,
                        format!(
                            "{key} bounds what a source's query copies, and an attached source \
                             copies nothing; [combine] sets the limits of what reads it"
                        ),
                    ));
                }
            }
            let split_key = match &split {
                Ok(SourceSplit::Whole) => None,
                Ok(SourceSplit::Lookup(_)) => Some("lookup"),
                Ok(SourceSplit::Partition(_)) | Err(_) => Some("partition"),
            };
            let columns_key = (!matches!(columns, Ok(None))).then_some("columns");
            let columnar_key = (!matches!(columnar, Ok(None))).then_some("columnar");
            for key in [split_key, columns_key, columnar_key].into_iter().flatten() {
                found.push(Found::at(
                    key,
                    format!(
                        "{key} is for a source's query, and an attached source has none; \
                         [combine] reads its tables as they are"
                    ),
                ));
            }
            Ok(SourceRead::Attach)
        }
        (
            Ok(_),
            Ok(query),
            Ok(max_rows),
            Ok(max_bytes),
            Ok(timeout),
            Ok(split),
            Ok(columns),
            Ok(columnar),
        ) => query
            .filter(|query| !query.trim().is_empty())
            .map(|query| SourceRead::Query {
                query,
                limits: SourceLimitsSet {
                    max_rows,
                    max_bytes,
                    timeout,
                },
                split,
                columns,
                rows: match columnar {
                    Some(false) => SourceRows::Text,
                    Some(true) | None => SourceRows::Columnar,
                },
            })
            .ok_or_else(|| {
                vec![Found::at(
                    "query",
                    "needs a query, or attach = true to read a DuckDB file in place",
                )]
            }),
        (attach, query, max_rows, max_bytes, timeout, split, columns, columnar) => {
            let mut errors = Vec::new();
            errors.extend(columnar.err());
            errors.extend(attach.err());
            errors.extend(query.err());
            errors.extend(max_rows.err());
            errors.extend(max_bytes.err());
            errors.extend(timeout.err());
            errors.extend(split.err().into_iter().flatten());
            errors.extend(
                columns
                    .err()
                    .into_iter()
                    .flatten()
                    .map(|problem| Found::at("columns", problem)),
            );
            Err(errors)
        }
    };
    let source = match (id, data_source, read) {
        (Ok(id), Ok(data_source), Ok(read)) => Some(Source {
            id,
            data_source,
            read,
        }),
        (id, data_source, read) => {
            found.extend(id.err());
            found.extend(data_source.err());
            found.extend(read.err().into_iter().flatten());
            None
        }
    };
    let source = source.filter(|_| found.is_empty());
    problems.extend(placed(Table::Source(position), Some(&label), found));
    source
}

/// How a source's query is split: `lookup`, or `partition` and its `max_rows_per_part`, or
/// neither.
fn read_split(fields: &mut Fields) -> Result<SourceSplit, Vec<Found>> {
    let lookup = fields.take("lookup").map(read_lookup).transpose();
    let partition = fields.take("partition").map(read_partition).transpose();
    let max_rows_per_part = fields.count("max_rows_per_part");
    match (lookup, partition, max_rows_per_part) {
        (Ok(Some(_)), Ok(Some(_)), _) => Err(vec![Found::at(
            "partition",
            "a source reads by a lookup or in ranges, not both; partition the source the \
             lookup reads, or join in [combine]",
        )]),
        (Ok(Some(lookup)), Ok(None), Ok(None)) => Ok(SourceSplit::Lookup(lookup)),
        (Ok(None), Ok(Some(partition)), Ok(max_rows_per_part)) => {
            Ok(SourceSplit::Partition(SourcePartition {
                partition,
                max_rows_per_part,
            }))
        }
        (Ok(_), Ok(None), Ok(Some(_))) => Err(vec![Found::at(
            "max_rows_per_part",
            "max_rows_per_part bounds each range of a partitioned source; add a partition, \
             or use max_rows",
        )]),
        (Ok(None), Ok(None), Ok(None)) => Ok(SourceSplit::Whole),
        (lookup, partition, max_rows_per_part) => {
            let mut found: Vec<Found> = Vec::new();
            found.extend(
                lookup
                    .err()
                    .into_iter()
                    .flatten()
                    .map(|problem| Found::at("lookup", problem)),
            );
            found.extend(
                partition
                    .err()
                    .into_iter()
                    .flatten()
                    .map(|problem| Found::at("partition", problem)),
            );
            found.extend(max_rows_per_part.err());
            Err(found)
        }
    }
}

/// `lookup = { from = "tx", column = "asset_id", as = "asset_ids" }`.
fn read_lookup(value: toml::Value) -> Result<Lookup, Vec<String>> {
    let toml::Value::Table(table) = value else {
        return Err(vec![
            "lookup is a table, as lookup = { from = \"tx\", column = \"asset_id\", as = \
             \"asset_ids\" }"
                .to_owned(),
        ]);
    };
    let mut fields = Fields::new(table);
    let mut named = |key: &str, example: &str| {
        fields
            .string(key)
            .map_err(|found| found.message)
            .and_then(|text| {
                text.filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| format!("lookup needs {key}, {example}"))
            })
    };
    let from = named("from", "the earlier source it reads, as from = \"tx\"");
    let column = named(
        "column",
        "the column of that source whose values it reads, as column = \"asset_id\"",
    );
    let parameter = named(
        "as",
        "the parameter its query reads them as, as as = \"asset_ids\" for :asset_ids",
    )
    .and_then(|name| {
        if is_param_name(&name) {
            Ok(name)
        } else {
            Err(format!(
                "lookup: {name:?} can't be a parameter's name, which is letters, digits and _, \
                 starting with a letter"
            ))
        }
    });
    let batch_size = fields
        .count("batch_size")
        .map(|size| {
            size.and_then(|size| usize::try_from(size).ok())
                .and_then(NonZeroUsize::new)
        })
        .map_err(|found| found.message);
    let mut problems: Vec<String> = fields
        .unknown("a lookup", "from, column, as and batch_size")
        .into_iter()
        .map(|problem| format!("lookup: {}", problem.message))
        .collect();
    match (from, column, parameter, batch_size) {
        (Ok(from), Ok(column), Ok(parameter), Ok(batch_size)) if problems.is_empty() => {
            Ok(Lookup {
                from,
                column,
                parameter,
                batch_size,
            })
        }
        (from, column, parameter, batch_size) => {
            problems.extend(from.err());
            problems.extend(column.err());
            problems.extend(parameter.err());
            problems.extend(batch_size.err().map(|problem| format!("lookup: {problem}")));
            Err(problems)
        }
    }
}

/// A source's `columns`: a list of `{ name, from, type, default }`.
fn read_source_columns(value: toml::Value) -> Result<Vec<SourceColumn>, Vec<String>> {
    let toml::Value::Array(items) = value else {
        return Err(vec![
            "columns is a list of the columns the source makes, as [{ name = \"symbol\", type \
             = \"text\" }]"
                .to_owned(),
        ]);
    };
    if items.is_empty() {
        return Err(vec!["columns lists at least one column".to_owned()]);
    }
    let mut problems = Vec::new();
    let mut read: Vec<SourceColumn> = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let toml::Value::Table(table) = item else {
            problems.push(format!(
                "column {}: each column is a table, as {{ name = \"symbol\", type = \"text\" }}",
                index + 1
            ));
            continue;
        };
        match read_source_column(table) {
            Ok(column)
                if read
                    .iter()
                    .any(|kept| kept.name.eq_ignore_ascii_case(&column.name)) =>
            {
                problems.push(format!(
                    "column {:?}: another column already has this name; each names a column of \
                     the source's table, so each is different",
                    column.name
                ));
            }
            Ok(column) => read.push(column),
            Err((label, problem)) => {
                let label = label.unwrap_or_else(|| format!("{}", index + 1));
                problems.push(format!("column {label}: {problem}"));
            }
        }
    }
    if problems.is_empty() {
        Ok(read)
    } else {
        Err(problems)
    }
}

/// One declared column, or its label (its name, quoted) and the first problem it has.
fn read_source_column(table: toml::Table) -> Result<SourceColumn, (Option<String>, String)> {
    let mut fields = Fields::new(table);
    let name = fields
        .string("name")
        .map_err(|problem| (None, problem.message))?;
    let label = name.as_ref().map(|name| format!("{name:?}"));
    let fail = |problem: String| (label.clone(), problem);
    let name = name.filter(|name| !name.trim().is_empty()).ok_or_else(|| {
        fail("needs a name, the column it makes in the source's table".to_owned())
    })?;
    let from = fields
        .string("from")
        .map_err(message)
        .map_err(fail)?
        .filter(|from| !from.is_empty())
        .unwrap_or_else(|| name.clone());
    let column_type = fields
        .string("type")
        .map_err(message)
        .map_err(fail)?
        .map(|type_name| type_name.parse::<ColumnType>())
        .transpose()
        .map_err(|error| fail(error.to_string()))?
        .unwrap_or(ColumnType::Text);
    if let ColumnType::Decimal(Some(spec)) = column_type
        && spec.precision() > DuckDbType::MAX_DECIMAL_DIGITS
    {
        return Err(fail(format!(
            "a source's decimal has at most {} digits, as DuckDB's DECIMAL holds",
            DuckDbType::MAX_DECIMAL_DIGITS
        )));
    }
    let default = match fields.take("default") {
        None => None,
        Some(toml::Value::String(text)) => Some(text),
        Some(toml::Value::Integer(number)) => Some(number.to_string()),
        Some(toml::Value::Float(number)) => Some(number.to_string()),
        Some(toml::Value::Boolean(flag)) => Some(flag.to_string()),
        Some(other) => {
            return Err(fail(format!(
                "default must be a value of the column's type, not a {}",
                other.type_str()
            )));
        }
    }
    .map(|default| {
        column_type
            .plain_form(&default)
            .map_err(|reason| fail(format!("default: {default:?} {reason}")))
    })
    .transpose()?;
    if let Some(problem) = fields
        .unknown("a source's column", "name, from, type and default")
        .into_iter()
        .next()
    {
        return Err(fail(problem.message));
    }
    Ok(SourceColumn {
        name,
        from,
        column_type,
        default,
    })
}

/// What only the whole list of sources shows about their lookups: each reads a source above
/// it that has a query, and its parameter isn't one of the report's.
fn check_lookups(
    sources: &[Source],
    positions: &[usize],
    params: &BTreeMap<&str, ColumnType>,
    problems: &mut Vec<ReportProblem>,
) {
    for (index, source) in sources.iter().enumerate() {
        let place = || Place::key(Table::Source(positions[index]), "lookup");
        let SourceRead::Query { split, .. } = &source.read else {
            continue;
        };
        let Some(lookup) = split.lookup() else {
            continue;
        };
        let label = format!("source {:?}", source.id);
        if params.contains_key(lookup.parameter.as_str()) {
            problems.push(ReportProblem::new(
                format!(
                    "{label}: lookup: :{} is one of [report] params; a lookup's values need a \
                     name of their own",
                    lookup.parameter
                ),
                place(),
            ));
        }
        let above = sources[..index]
            .iter()
            .find(|earlier| earlier.id.eq_ignore_ascii_case(&lookup.from));
        let problem = match above {
            Some(earlier) if earlier.read == SourceRead::Attach => Some(format!(
                "{label}: lookup: {:?} is attached, and a lookup reads a loaded source's \
                 table; join the attached tables in [combine] instead",
                lookup.from
            )),
            Some(_) => None,
            None if lookup.from.eq_ignore_ascii_case(&source.id) => Some(format!(
                "{label}: lookup: a source can't look up its own values"
            )),
            None if sources
                .iter()
                .any(|later| later.id.eq_ignore_ascii_case(&lookup.from)) =>
            {
                Some(format!(
                    "{label}: lookup: {:?} is below this source; a lookup reads a source above \
                     it in the file, which runs first",
                    lookup.from
                ))
            }
            None => Some(format!(
                "{label}: lookup: there is no source {:?} to read from",
                lookup.from
            )),
        };
        problems.extend(problem.map(|message| ReportProblem::new(message, place())));
    }
}

/// The parameters an output's file name may use: in a partitioned run, a range's bounds too, of
/// the range's type.
fn with_range_params<'a>(
    partition: Option<&Partition>,
    params: &BTreeMap<&'a str, ColumnType>,
) -> BTreeMap<&'a str, ColumnType> {
    let mut named = params.clone();
    let planned =
        partition.and_then(|partition| PlannedPartition::plan(partition.clone(), params).ok());
    if let Some(planned) = planned {
        named.insert(PART_FROM, planned.kind.column_type());
        named.insert(PART_TO, planned.kind.column_type());
    }
    named
}

/// What a partitioned run needs of the rest of the file (RS4d): bounds of the report's
/// parameters, of one kind; ranges that go one at a time; no parameter named as the ranges'
/// own; and sources that aren't partitioned themselves.
fn check_run_partition(
    partition: Option<&Partition>,
    params: &BTreeMap<&str, ColumnType>,
    sources: &[Source],
    positions: &[usize],
    problems: &mut Vec<ReportProblem>,
) {
    let Some(partition) = partition else {
        return;
    };
    let mut found = Vec::new();
    if let Err(problem) = PlannedPartition::plan(partition.clone(), params) {
        found.push(format!("partition: {problem}"));
    }
    if partition.parallel.is_some() {
        found.push(
            "partition: a run's ranges go one at a time, which is what bounds its memory; \
             parallel is for a [[source]]'s partition"
                .to_owned(),
        );
    }
    if let Some(taken) = [PART_FROM, PART_TO]
        .into_iter()
        .find(|name| params.contains_key(name))
    {
        found.push(format!(
            "partition: each range binds :{PART_FROM} and :{PART_TO}, so params can't name \
             {taken}"
        ));
    }
    let place = Place::key(Table::Report, "partition");
    problems.extend(
        found
            .into_iter()
            .map(|problem| ReportProblem::new(format!("[report]: {problem}"), place.clone())),
    );
    for (index, source) in sources.iter().enumerate() {
        if let SourceRead::Query {
            split: SourceSplit::Partition(_),
            ..
        } = &source.read
        {
            problems.push(ReportProblem::new(
                format!(
                    "source {:?}: partition: the run is partitioned already, and its ranges bind \
                     :{PART_FROM} and :{PART_TO} in this source's query; leave the source's \
                     partition out",
                    source.id
                ),
                Place::key(Table::Source(positions[index]), "partition"),
            ));
        }
    }
}

/// What a partitioned source's ranges need of the rest of the file: bounds of the report's
/// parameters, of one kind; no parameter named as the ranges' own; and a combine step to load
/// the ranges for.
fn check_partitions(
    sources: &[Source],
    positions: &[usize],
    params: &BTreeMap<&str, ColumnType>,
    has_combine: bool,
    problems: &mut Vec<ReportProblem>,
) {
    for (index, source) in sources.iter().enumerate() {
        let SourceRead::Query {
            split: SourceSplit::Partition(split),
            ..
        } = &source.read
        else {
            continue;
        };
        let label = format!("source {:?}", source.id);
        let mut found = Vec::new();
        if let Err(problem) = PlannedPartition::plan(split.partition.clone(), params) {
            found.push(format!("partition: {problem}"));
        }
        if let Some(taken) = [PART_FROM, PART_TO]
            .into_iter()
            .find(|name| params.contains_key(name))
        {
            found.push(format!(
                "partition: each range binds :{PART_FROM} and :{PART_TO}, so [report] params \
                 can't name {taken}"
            ));
        }
        if !has_combine {
            found.push(
                "partition: a partitioned source loads its ranges into one table for a \
                 [combine] query; add one, as query = \"SELECT * FROM <source>\""
                    .to_owned(),
            );
        }
        let place = Place::key(Table::Source(positions[index]), "partition");
        problems.extend(
            found
                .into_iter()
                .map(|problem| ReportProblem::new(format!("{label}: {problem}"), place.clone())),
        );
    }
}

/// Whether `id` can name a source's table in DuckDB, unquoted.
fn check_id(id: &str) -> Result<(), String> {
    let plain = id.starts_with(|c: char| c.is_ascii_alphabetic())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !plain {
        return Err(
            "an id names the source's table in [combine], so it is letters, digits and _, \
             starting with a letter"
                .to_owned(),
        );
    }
    if RESERVED_IDS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(id))
    {
        return Err(format!(
            "{id:?} is a name DuckDB keeps for itself; name the source another way"
        ));
    }
    Ok(())
}

fn read_combine(table: toml::Table, problems: &mut Vec<ReportProblem>) -> Option<Combine> {
    let mut fields = Fields::new(table);
    let mut found = Vec::new();
    let sql = fields.string("query");
    let prql = fields.string("prql");
    let query = combine_query(sql, prql);
    let timeout = fields.duration("timeout");
    let memory_limit = fields.byte_size("memory_limit");
    let threads = fields.count("threads");
    let max_temp_size = fields.byte_size("max_temp_size");
    found.extend(fields.unknown(
        "[combine]",
        "query or prql, timeout, memory_limit, threads and max_temp_size",
    ));
    let combine = match (query, timeout, memory_limit, threads, max_temp_size) {
        (Ok(query), Ok(timeout), Ok(memory_limit), Ok(threads), Ok(max_temp_size)) => {
            Some(Combine {
                query,
                limits: CombineLimitsSet {
                    timeout,
                    memory_limit,
                    threads: threads
                        .and_then(|threads| usize::try_from(threads).ok())
                        .and_then(NonZeroUsize::new),
                    max_temp_size,
                },
            })
        }
        (query, timeout, memory_limit, threads, max_temp_size) => {
            found.extend(query.err().into_iter().flatten());
            found.extend(timeout.err());
            found.extend(memory_limit.err());
            found.extend(threads.err());
            found.extend(max_temp_size.err());
            None
        }
    };
    let combine = combine.filter(|_| found.is_empty());
    problems.extend(placed(Table::Combine, Some("[combine]"), found));
    combine
}

/// The combine step's query from its `query` and `prql` keys, of which it has one; PRQL
/// compiled. A PRQL that doesn't compile can give several problems.
fn combine_query(
    sql: Result<Option<String>, Found>,
    prql: Result<Option<String>, Found>,
) -> Result<CombineQuery, Vec<Found>> {
    let given = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
    match (sql.map(given), prql.map(given)) {
        (Err(problem), _) | (_, Err(problem)) => Err(vec![problem]),
        (Ok(Some(_)), Ok(Some(_))) => Err(vec![Found::at(
            "prql",
            "has both a query and prql; it runs one, so keep the one you mean",
        )]),
        (Ok(Some(sql)), Ok(None)) => Ok(CombineQuery::Sql(sql)),
        (Ok(None), Ok(Some(prql))) => crate::prql::compile(&prql)
            .map(|compiled| CombineQuery::Prql { prql, compiled })
            .map_err(|problems| {
                problems
                    .into_iter()
                    .map(|problem| Found::at("prql", problem))
                    .collect()
            }),
        (Ok(None), Ok(None)) => Err(vec![Found::from(
            "needs a query, the DuckDB SQL that combines the sources' tables, or prql, the same \
             in PRQL",
        )]),
    }
}

fn read_outputs(
    outputs: Vec<toml::Table>,
    params: &BTreeMap<&str, ColumnType>,
    problems: &mut Vec<ReportProblem>,
) -> Vec<Output> {
    if outputs.is_empty() {
        problems.push(ReportProblem::new(
            "the file has no [[output]] table, for a file to write",
            Place::table(Table::File),
        ));
    }
    let mut read = Vec::new();
    for (index, table) in outputs.into_iter().enumerate() {
        let label = format!("output {}", index + 1);
        let mut found = Vec::new();
        let output = read_output(table, params, index == 0, &mut found);
        problems.extend(placed(Table::Output(index), Some(&label), found));
        if let Some(output) = output {
            if read
                .iter()
                .any(|kept: &Output| kept.file.as_str() == output.file.as_str())
            {
                problems.push(ReportProblem::new(
                    format!(
                        "{label}: another output already writes {:?}; several outputs in one \
                         file, as sheets of one workbook, come after RP1",
                        output.file.as_str()
                    ),
                    Place::key(Table::Output(index), "file"),
                ));
            }
            read.push(output);
        }
    }
    read
}

/// The keys every output has, beside its writer's options.
const OUTPUT_KEYS: [&str; 6] = [
    "format",
    "preset",
    "file",
    "columns",
    "max_size",
    "continue_on_new_sheet",
];

fn read_output(
    table: toml::Table,
    params: &BTreeMap<&str, ColumnType>,
    first: bool,
    problems: &mut Vec<Found>,
) -> Option<Output> {
    let mut fields = Fields::new(table);
    let format = fields.string("format");
    let preset = fields.string("preset");
    let file = fields.string("file");
    let columns = fields.take("columns");
    let max_size = fields.byte_size("max_size");
    let continue_on_new_sheet = fields.boolean("continue_on_new_sheet");
    let writer = match (format, preset) {
        (Ok(Some(format)), Ok(None)) => read_format(&format, fields, problems),
        (Ok(None), Ok(Some(id))) => {
            problems.extend(fields.0.keys().map(|key| {
                Found::at(
                    key,
                    format!(
                        "{key:?} can't be set on an output with a preset, whose options are set \
                         in exports.toml"
                    ),
                )
            }));
            match CustomFormatId::new(&id) {
                Some(id) => Some(OutputWriter::Preset(id)),
                None => {
                    problems.push(Found::at(
                        "preset",
                        format!("{id:?} can't be the id of a preset in exports.toml"),
                    ));
                    None
                }
            }
        }
        (Ok(Some(_)), Ok(Some(_))) => {
            problems.push(Found::at(
                "preset",
                "an output has a format or a preset from exports.toml, not both",
            ));
            None
        }
        (Ok(None), Ok(None)) => {
            problems.push(Found::from(
                "needs a format, as format = \"xlsx\" or \"csv\", or a preset from exports.toml, \
                 as preset = \"semicolon-csv\"",
            ));
            None
        }
        (format, preset) => {
            problems.extend(format.err());
            problems.extend(preset.err());
            None
        }
    };
    let file = match file {
        Ok(Some(file)) => FileName::parse(&file)
            .and_then(|name| name.check(params).map(|()| name))
            .map_err(|problem| Found::at("file", format!("file: {problem}"))),
        Ok(None) => Err(Found::at(
            "file",
            "needs a file name, as file = \"trial-balance.xlsx\"",
        )),
        Err(problem) => Err(problem),
    };
    let columns = read_columns(columns, first).map_err(|problems| {
        problems
            .into_iter()
            .map(|problem| Found::at("columns", problem))
            .collect::<Vec<_>>()
    });
    let output = match (writer, file, columns, max_size, continue_on_new_sheet) {
        (Some(writer), Ok(file), Ok(columns), Ok(max_size), Ok(continue_on_new_sheet)) => {
            let continue_on_new_sheet = continue_on_new_sheet.unwrap_or(false);
            if continue_on_new_sheet && !writes_excel(&writer) {
                problems.push(Found::at(
                    "continue_on_new_sheet",
                    "continue_on_new_sheet is for Excel outputs",
                ));
            }
            Some(Output {
                writer,
                file,
                columns,
                limits: OutputLimitsSet { max_size },
                continue_on_new_sheet,
            })
        }
        (_, file, columns, max_size, continue_on_new_sheet) => {
            problems.extend(file.err());
            problems.extend(columns.err().into_iter().flatten());
            problems.extend(max_size.err());
            problems.extend(continue_on_new_sheet.err());
            None
        }
    };
    output.filter(|_| problems.is_empty())
}

/// The keys a `[[chart]]` reads itself, beside the chart's own (`kind`, `x`, …), which
/// `dbrover-chart` reads.
const CHART_KEYS: [&str; 5] = ["file", "width", "height", "scale", "query"];

/// A chart's size when its file leaves it out, in points: 16 by 9, as wide as a page's text.
const DEFAULT_CHART_SIZE: (u32, u32) = (800, 450);

/// The sides a chart may have, in points. Below the least, the axes' labels leave no room for
/// the plot; past the most, a PNG at 3× is far past `dbrover_picture::MAX_PIXELS` anyway.
const CHART_SIDES: std::ops::RangeInclusive<u32> = 200..=4_000;

/// A PNG's scale when its chart leaves it out, as **Export ›** offers it first.
const DEFAULT_PNG_SCALE: u8 = 2;
const PNG_SCALES: std::ops::RangeInclusive<u8> = 1..=3;

/// What a chart is read against: the report's parameters and outputs, and whether it has a
/// combine step for a chart's `query`.
struct ChartsRead<'a> {
    params: &'a BTreeMap<&'a str, ColumnType>,
    outputs: &'a [Output],
    has_combine: bool,
}

fn read_charts(
    charts: Vec<toml::Table>,
    report: &ChartsRead<'_>,
    problems: &mut Vec<ReportProblem>,
) -> Vec<Chart> {
    let outputs = report.outputs;
    let mut read: Vec<Chart> = Vec::new();
    for (index, table) in charts.into_iter().enumerate() {
        let label = format!("chart {}", index + 1);
        let mut found = Vec::new();
        let chart = read_chart(table, report, &mut found);
        problems.extend(placed(Table::Chart(index), Some(&label), found));
        let Some(chart) = chart else {
            continue;
        };
        let name = chart.file.as_str();
        let taken = outputs.iter().any(|output| output.file.as_str() == name)
            || read.iter().any(|kept| kept.file.as_str() == name);
        if taken {
            problems.push(ReportProblem::new(
                format!("{label}: another output or chart already writes {name:?}"),
                Place::key(Table::Chart(index), "file"),
            ));
        }
        read.push(chart);
    }
    read
}

fn read_chart(
    table: toml::Table,
    report: &ChartsRead<'_>,
    problems: &mut Vec<Found>,
) -> Option<Chart> {
    let params = report.params;
    let spec = ChartSpec::from_table(&table, &CHART_KEYS).map_err(|spec_problems| {
        problems.extend(
            spec_problems
                .into_iter()
                .map(|problem| Found::at(&problem.key, problem.detail)),
        );
    });
    let mut fields = Fields::new(table);
    let file = chart_file(fields.string("file"), params);
    let query = fields.string("query").and_then(|query| match query {
        Some(_) if !report.has_combine => Err(Found::at(
            "query",
            "query reads the sources' tables in the combine step's DuckDB, which a report \
             without [combine] doesn't have; add a [combine], or leave query out to draw the \
             result",
        )),
        Some(query) if query.trim().is_empty() => Err(Found::at(
            "query",
            "query is empty; leave it out to draw the result",
        )),
        query => Ok(query),
    });
    let width = chart_side(&mut fields, "width", DEFAULT_CHART_SIZE.0);
    let height = chart_side(&mut fields, "height", DEFAULT_CHART_SIZE.1);
    let scale = fields.count("scale").and_then(|scale| match scale {
        None => Ok(None),
        Some(scale) => u8::try_from(scale)
            .ok()
            .filter(|scale| PNG_SCALES.contains(scale))
            .map(Some)
            .ok_or_else(|| Found::at("scale", "scale must be 1, 2 or 3")),
    });
    let (spec, file, width, height, scale, query) = match (spec, file, width, height, scale, query)
    {
        (Ok(spec), Ok(file), Ok(width), Ok(height), Ok(scale), Ok(query)) => {
            (spec, file, width, height, scale, query)
        }
        (_, file, width, height, scale, query) => {
            problems.extend(query.err());
            problems.extend(file.err());
            problems.extend(width.err());
            problems.extend(height.err());
            problems.extend(scale.err());
            return None;
        }
    };
    let picture =
        chart_picture(&file, scale, (width, height)).map_err(|problem| problems.push(problem));
    Some(Chart {
        spec,
        file,
        picture: picture.ok()?,
        query,
        width,
        height,
    })
}

/// The chart's `file`, a name of its own with the report's parameters in it.
fn chart_file(
    file: Result<Option<String>, Found>,
    params: &BTreeMap<&str, ColumnType>,
) -> Result<FileName, Found> {
    match file? {
        Some(file) => FileName::parse(&file)
            .and_then(|name| name.check(params).map(|()| name))
            .map_err(|problem| Found::at("file", format!("file: {problem}"))),
        None => Err(Found::at(
            "file",
            "needs a file name, as file = \"net-by-category.svg\"",
        )),
    }
}

/// `width` or `height`, in points.
fn chart_side(fields: &mut Fields, key: &str, default: u32) -> Result<u32, Found> {
    let side = fields.count(key)?;
    let Some(side) = side else {
        return Ok(default);
    };
    u32::try_from(side)
        .ok()
        .filter(|side| CHART_SIDES.contains(side))
        .ok_or_else(|| {
            Found::at(
                key,
                format!(
                    "{key} is a number of points from {} to {}",
                    grouped(u64::from(*CHART_SIDES.start())),
                    grouped(u64::from(*CHART_SIDES.end()))
                ),
            )
        })
}

/// SVG or PNG, by the file's extension, and a PNG's scale, which must fit
/// `dbrover_picture::MAX_PIXELS` at the chart's size.
fn chart_picture(
    file: &FileName,
    scale: Option<u8>,
    (width, height): (u32, u32),
) -> Result<ChartPicture, Found> {
    let extension = std::path::Path::new(file.as_str())
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match (extension.as_deref(), scale) {
        (Some("svg"), None) => Ok(ChartPicture::Svg),
        (Some("svg"), Some(_)) => Err(Found::at(
            "scale",
            "scale sizes a PNG's pixels, and this chart writes SVG; take it out, or name a .png \
             file",
        )),
        (Some("png"), scale) => {
            let scale = scale.unwrap_or(DEFAULT_PNG_SCALE);
            let pixels = u64::from(width) * u64::from(height) * u64::from(scale).pow(2);
            if pixels > dbrover_picture::MAX_PIXELS {
                return Err(Found::at(
                    "scale",
                    format!(
                        "a PNG of {width} × {height} points at {scale}× is too large to draw; \
                         make the chart smaller, lower scale, or write SVG"
                    ),
                ));
            }
            Ok(ChartPicture::Png { scale })
        }
        _ => Err(Found::at(
            "file",
            "file: a chart writes an .svg or a .png file",
        )),
    }
}

/// Whether the writer is known to write Excel files before the presets are looked up: a
/// preset's format is checked when the report is planned.
fn writes_excel(writer: &OutputWriter) -> bool {
    match writer {
        OutputWriter::Format(preset) => preset.format == ExportFormat::Xlsx,
        OutputWriter::Preset(_) => true,
    }
}

/// `format = "csv"`, `"xlsx"` or `"parquet"`, with the output's other keys as that format's
/// options.
fn read_format(format: &str, fields: Fields, problems: &mut Vec<Found>) -> Option<OutputWriter> {
    let format = match ExportFormat::from_id(format) {
        Some(format @ (ExportFormat::Csv | ExportFormat::Xlsx | ExportFormat::Parquet)) => format,
        Some(other) => {
            problems.push(Found::at(
                "format",
                format!(
                    "a report writes CSV, Excel or Parquet files, not {}",
                    other.name()
                ),
            ));
            return None;
        }
        None => {
            problems.push(Found::at(
                "format",
                format!(
                    "{format:?} is not a format; a report writes \"csv\", \"xlsx\" or \"parquet\""
                ),
            ));
            return None;
        }
    };
    let names = Preset::option_names(format);
    let options: toml::Table = fields
        .0
        .into_iter()
        .filter(|(key, _)| {
            let known = names.contains(key);
            if !known {
                problems.push(Found::at(
                    key,
                    format!(
                        "{key:?} is not part of an output; an output has {}, and {}'s options {}",
                        OUTPUT_KEYS.join(", "),
                        format.name(),
                        names.join(", ")
                    ),
                ));
            }
            known
        })
        .collect();
    match Preset::read(format, toml::Value::Table(options)) {
        Ok(preset) => Some(OutputWriter::Format(preset)),
        Err(error) => {
            problems.push(Found::from(error.message().trim()));
            None
        }
    }
}

fn read_columns(value: Option<toml::Value>, first: bool) -> Result<OutputColumns, Vec<String>> {
    match value {
        None => Ok(OutputColumns::All),
        Some(toml::Value::String(same)) if same == "same" && first => Err(vec![
            "columns = \"same\" takes the first output's columns, and this is the first".to_owned(),
        ]),
        Some(toml::Value::String(same)) if same == "same" => Ok(OutputColumns::Same),
        Some(toml::Value::Array(columns)) if !columns.is_empty() => {
            let mut problems = Vec::new();
            let mut read = Vec::new();
            for (index, column) in columns.into_iter().enumerate() {
                match column {
                    toml::Value::Table(table) => match read_column(table) {
                        Ok(column) => read.push(column),
                        Err((label, problem)) => {
                            let label = label.unwrap_or_else(|| format!("{}", index + 1));
                            problems.push(format!("column {label}: {problem}"));
                        }
                    },
                    _ => problems.push(format!(
                        "column {}: each column is a table, as {{ name = \"Debit\", from = \
                         \"debit\" }}",
                        index + 1
                    )),
                }
            }
            if problems.is_empty() {
                Ok(OutputColumns::Declared(read))
            } else {
                Err(problems)
            }
        }
        Some(_) => Err(vec![
            "columns is a list of columns, as [{ name = \"Debit\", from = \"debit\" }], or \
             \"same\" for the first output's"
                .to_owned(),
        ]),
    }
}

/// One column, or its label (its name, quoted) and the first problem it has.
fn read_column(table: toml::Table) -> Result<SchemaColumn, (Option<String>, String)> {
    let mut fields = Fields::new(table);
    let name = fields
        .string("name")
        .map_err(|problem| (None, problem.message))?;
    let label = name.as_ref().map(|name| format!("{name:?}"));
    let fail = |problem: String| (label.clone(), problem);
    let name = name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| fail("needs a name, the header it writes".to_owned()))?;
    let from = fields.string("from").map_err(message).map_err(fail)?;
    let column_type = fields
        .string("type")
        .map_err(message)
        .map_err(fail)?
        .map(|type_name| type_name.parse::<ColumnType>())
        .transpose()
        .map_err(|error| fail(error.to_string()))?;
    let excel_format = fields.string("format").map_err(message).map_err(fail)?;
    let time_zone = fields
        .string("time_zone")
        .map_err(message)
        .map_err(fail)?
        .map(|zone| {
            jiff::tz::TimeZone::get(&zone)
                .map_err(|_| fail(format!("{zone:?} is not a time zone, as \"UTC\"")))
        })
        .transpose()?;
    if time_zone.is_some() && column_type != Some(ColumnType::Timestamp) {
        return Err(fail("time_zone is for a timestamp column".to_owned()));
    }
    let if_invalid = fields
        .string("if_invalid")
        .map_err(message)
        .map_err(fail)?
        .map(|text| text.parse::<IfInvalid>())
        .transpose()
        .map_err(fail)?
        .unwrap_or_default();
    if let Some(problem) = fields
        .unknown(
            "a column",
            "name, from, type, format, time_zone and if_invalid",
        )
        .into_iter()
        .next()
    {
        return Err(fail(problem.message));
    }
    Ok(SchemaColumn {
        from: from.unwrap_or_else(|| name.clone()),
        name,
        column_type,
        excel_format,
        time_zone,
        if_invalid,
    })
}

/// A table's keys, taken one at a time, so that what is left is what the table doesn't know.
/// Each problem it finds is about the key it was asked for.
struct Fields(toml::Table);

impl Fields {
    fn new(table: toml::Table) -> Self {
        Self(table)
    }

    fn take(&mut self, key: &str) -> Option<toml::Value> {
        self.0.remove(key)
    }

    fn string(&mut self, key: &str) -> Result<Option<String>, Found> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text)),
            Some(other) => Err(Found::at(
                key,
                format!("{key} must be a string, not a {}", other.type_str()),
            )),
        }
    }

    fn boolean(&mut self, key: &str) -> Result<Option<bool>, Found> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Boolean(value)) => Ok(Some(value)),
            Some(other) => Err(Found::at(
                key,
                format!("{key} must be true or false, not a {}", other.type_str()),
            )),
        }
    }

    /// A whole number greater than zero.
    fn count(&mut self, key: &str) -> Result<Option<u64>, Found> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Integer(number)) if number > 0 => Ok(u64::try_from(number).ok()),
            Some(_) => Err(Found::at(
                key,
                format!("{key} must be a whole number greater than zero"),
            )),
        }
    }

    /// A size, as `"500 MB"`, or a number of bytes.
    fn byte_size(&mut self, key: &str) -> Result<Option<ByteSize>, Found> {
        let invalid = || {
            Found::at(
                key,
                format!("{key} must be a size such as \"500 MB\" or \"1 GiB\""),
            )
        };
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => ByteSize::parse(&text)
                .filter(|size| size.0 > 0)
                .map(Some)
                .ok_or_else(invalid),
            Some(toml::Value::Integer(bytes)) if bytes > 0 => {
                Ok(u64::try_from(bytes).ok().map(ByteSize))
            }
            Some(_) => Err(invalid()),
        }
    }

    /// A duration, as `"30s"`, `"10m"` or `"1h 30m"`.
    fn duration(&mut self, key: &str) -> Result<Option<Duration>, Found> {
        let invalid = || {
            Found::at(
                key,
                format!("{key} must be a duration such as \"30s\" or \"10m\""),
            )
        };
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => text
                .parse::<jiff::SignedDuration>()
                .ok()
                .and_then(|duration| Duration::try_from(duration).ok())
                .filter(|duration| !duration.is_zero())
                .map(Some)
                .ok_or_else(invalid),
            Some(_) => Err(invalid()),
        }
    }

    /// A table, if the key is there.
    fn table(&mut self, key: &str) -> Result<Option<toml::Table>, Found> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Table(table)) => Ok(Some(table)),
            Some(_) if key == "report" => Err(Found::at(key, "report must be a [report] table")),
            Some(_) => Err(Found::at(
                key,
                format!("{key} must be a table, as {key} = {{ … }}"),
            )),
        }
    }

    /// The `[[key]]` tables; none if the key isn't there.
    fn tables(&mut self, key: &str) -> Result<Vec<toml::Table>, Found> {
        let not_tables = || Found::at(key, format!("{key} must be a list of [[{key}]] tables"));
        match self.take(key) {
            None => Ok(Vec::new()),
            Some(toml::Value::Array(items)) => items
                .into_iter()
                .map(|item| match item {
                    toml::Value::Table(table) => Ok(table),
                    _ => Err(not_tables()),
                })
                .collect(),
            Some(_) => Err(not_tables()),
        }
    }

    /// A problem for each key left, which `what` doesn't have; it has `has`.
    fn unknown(&self, what: &str, has: &str) -> Vec<Found> {
        self.0
            .keys()
            .map(|key| {
                Found::at(
                    key,
                    format!("{key:?} is not part of {what}, which has {has}"),
                )
            })
            .collect()
    }
}
