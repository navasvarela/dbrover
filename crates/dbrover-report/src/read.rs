//! Reading a report file: TOML, checked as a whole. A report with a problem can't run, so
//! unlike `exports.toml` nothing is skipped: every problem found is listed, each a sentence
//! that names where it is (`[report]`, `source "balances"`, `output 2, column "Debit"`).

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use dbrover_export::{ColumnType, CustomFormatId, ExportFormat, IfInvalid, Preset, SchemaColumn};

use dbrover_driver_duckdb::DuckDbType;

use crate::{
    ByteSize, Combine, CombineLimitsSet, CombineQuery, FileName, HintId, Lookup, Output,
    OutputColumns, OutputLimitsSet, OutputWriter, Param, Report, Source, SourceColumn,
    SourceLimitsSet, SourceRead,
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
    let mut problems = Vec::new();
    let table = match toml::from_str::<toml::Table>(text) {
        Ok(table) => table,
        Err(error) => return Err(vec![toml_problem(text, &error)]),
    };
    let mut file = Fields::new(table);
    let header = file.table("report");
    let sources = file.tables("source");
    let combine = file.table("combine");
    let outputs = file.tables("output");
    for (key, place) in MISPLACED_REPORT_KEYS {
        if file.take(key).is_some() {
            problems.push(place.to_owned());
        }
    }
    problems.extend(file.unknown(
        "a report file",
        "[report], [[source]], [combine] and [[output]]",
    ));

    let header = match header {
        Ok(Some(header)) => read_header(header, &mut problems),
        Ok(None) => {
            problems.push("the file has no [report] table, with the report's name".to_owned());
            None
        }
        Err(problem) => {
            problems.push(problem);
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

    let sources = match sources {
        Ok(sources) => read_sources(sources, &mut problems),
        Err(problem) => {
            problems.push(problem);
            Vec::new()
        }
    };
    check_lookups(&sources, &params, &mut problems);
    let combine = match combine {
        Ok(Some(combine)) => read_combine(combine, &mut problems),
        Ok(None) => None,
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    if !problems
        .iter()
        .any(|problem| problem.starts_with("[combine]"))
    {
        check_shape(&sources, combine.is_some(), &mut problems);
    }
    let outputs = match outputs {
        Ok(outputs) => read_outputs(outputs, &params, &mut problems),
        Err(problem) => {
            problems.push(problem);
            Vec::new()
        }
    };

    match header {
        Some(header) if problems.is_empty() => Ok(Report {
            name: header.name,
            description: header.description,
            params: header.params,
            timeout: header.timeout,
            quiet_hints: header.quiet_hints,
            sources,
            combine,
            outputs,
        }),
        _ => Err(problems),
    }
}

/// What a TOML error says, with the line it is on.
fn toml_problem(text: &str, error: &toml::de::Error) -> String {
    let line = error
        .span()
        .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
    match line {
        Some(line) => format!("line {line}: {}", error.message().trim()),
        None => error.message().trim().to_owned(),
    }
}

struct Header {
    name: String,
    description: Option<String>,
    params: Vec<Param>,
    timeout: Option<Duration>,
    quiet_hints: Vec<HintId>,
}

fn read_header(table: toml::Table, problems: &mut Vec<String>) -> Option<Header> {
    let mut fields = Fields::new(table);
    let label = "[report]";
    let mut found = Vec::new();
    let name = fields.string("name").and_then(|name| {
        name.filter(|name| !name.trim().is_empty())
            .ok_or_else(|| "needs a name, as name = \"Trial balance\"".to_owned())
    });
    let description = fields.string("description");
    let params = fields.table("params").map(Option::unwrap_or_default);
    let timeout = fields.duration("timeout");
    let quiet_hints = read_quiet_hints(fields.take("quiet_hints"));
    for (key, place) in MISPLACED_REPORT_KEYS {
        if fields.take(key).is_some() {
            found.push(place.to_owned());
        }
    }
    found.extend(fields.unknown(
        "[report]",
        "name, description, params, timeout and quiet_hints",
    ));
    let params = params.map(|params| read_params(params, &mut found));
    let header = match (name, description, params, timeout, quiet_hints) {
        (Ok(name), Ok(description), Ok(params), Ok(timeout), Ok(quiet_hints)) => Some(Header {
            name,
            description: description.filter(|text| !text.trim().is_empty()),
            params,
            timeout,
            quiet_hints,
        }),
        (name, description, params, timeout, quiet_hints) => {
            found.extend(name.err());
            found.extend(description.err());
            found.extend(params.err());
            found.extend(timeout.err());
            found.extend(quiet_hints.err().into_iter().flatten());
            None
        }
    };
    problems.extend(
        found
            .into_iter()
            .map(|problem| format!("{label}: {problem}")),
    );
    header
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

fn read_params(table: toml::Table, problems: &mut Vec<String>) -> Vec<Param> {
    let mut params = Vec::new();
    for (name, value) in table {
        if !is_param_name(&name) {
            problems.push(format!(
                "params: {name:?} can't be a parameter's name, which is letters, digits and _, \
                 starting with a letter"
            ));
            continue;
        }
        let toml::Value::String(type_name) = value else {
            problems.push(format!(
                "params: {name}'s type is a string, as {name} = \"date\""
            ));
            continue;
        };
        match type_name.parse::<ColumnType>() {
            Ok(param_type) => params.push(Param { name, param_type }),
            Err(error) => problems.push(format!("params: {name}: {error}")),
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
/// already has. A source with a problem is left out, and its problems listed.
fn read_sources(sources: Vec<toml::Table>, problems: &mut Vec<String>) -> Vec<Source> {
    let mut read: Vec<Source> = Vec::new();
    for table in sources {
        let Some(source) = read_source(table, problems) else {
            continue;
        };
        if read
            .iter()
            .any(|kept| kept.id.eq_ignore_ascii_case(&source.id))
        {
            problems.push(format!(
                "source {:?}: another source already has this id; each source's id names its \
                 table, so each is different",
                source.id
            ));
            continue;
        }
        read.push(source);
    }
    read
}

/// What only the whole list of sources shows: whether there is one, and whether the sources
/// need a combine step they don't have.
fn check_shape(sources: &[Source], combined: bool, problems: &mut Vec<String>) {
    let source_problems = problems
        .iter()
        .any(|problem| problem.starts_with("source ") || problem.starts_with("[[source]]"));
    match sources {
        [] if !source_problems => problems.push(
            "the file has no [[source]] table, with the query whose rows the report writes"
                .to_owned(),
        ),
        [one] if !combined && one.read == SourceRead::Attach => problems.push(format!(
            "source {:?}: an attached source is read in place by a [combine] query; add \
             one, or give the source a query",
            one.id
        )),
        [_, _, ..] if !combined => problems.push(format!(
            "the file has {} [[source]] tables; several sources are combined by a [combine] \
             query over their tables",
            sources.len()
        )),
        _ => {}
    }
}

fn read_source(table: toml::Table, problems: &mut Vec<String>) -> Option<Source> {
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
            .ok_or_else(|| "needs an id, as id = \"balances\"".to_owned())?;
        check_id(&id).map(|()| id)
    });
    let data_source = fields.string("data_source").and_then(|name| {
        name.filter(|name| !name.trim().is_empty()).ok_or_else(|| {
            "needs the data source it reads, by name: data_source = \"ledger\"".to_owned()
        })
    });
    let attach = fields.boolean("attach");
    let query = fields.string("query");
    let max_rows = fields.count("max_rows");
    let max_bytes = fields.byte_size("max_bytes");
    let timeout = fields.duration("timeout");
    let lookup = fields.take("lookup").map(read_lookup).transpose();
    let columns = fields.take("columns").map(read_source_columns).transpose();
    found.extend(fields.unknown(
        "a source",
        "id, data_source, query, lookup, columns, attach, max_rows, max_bytes and timeout",
    ));
    let read = match (attach, query, max_rows, max_bytes, timeout, lookup, columns) {
        (Ok(Some(true)), query, max_rows, max_bytes, timeout, lookup, columns) => {
            if !matches!(query, Ok(None)) {
                found.push(
                    "an attached source has no query: the [combine] query reads its tables in \
                     place"
                        .to_owned(),
                );
            }
            let limits = [
                ("max_rows", max_rows.map(|value| value.is_some())),
                ("max_bytes", max_bytes.map(|value| value.is_some())),
                ("timeout", timeout.map(|value| value.is_some())),
            ];
            for (key, set) in limits {
                if !matches!(set, Ok(false)) {
                    found.push(format!(
                        "{key} bounds what a source's query copies, and an attached source \
                         copies nothing; [combine] sets the limits of what reads it"
                    ));
                }
            }
            let query_keys = [
                ("lookup", lookup.map(|value| value.is_some())),
                ("columns", columns.map(|value| value.is_some())),
            ];
            for (key, set) in query_keys {
                if !matches!(set, Ok(false)) {
                    found.push(format!(
                        "{key} is for a source's query, and an attached source has none; \
                         [combine] reads its tables as they are"
                    ));
                }
            }
            Ok(SourceRead::Attach)
        }
        (Ok(_), Ok(query), Ok(max_rows), Ok(max_bytes), Ok(timeout), Ok(lookup), Ok(columns)) => {
            query
                .filter(|query| !query.trim().is_empty())
                .map(|query| SourceRead::Query {
                    query,
                    limits: SourceLimitsSet {
                        max_rows,
                        max_bytes,
                        timeout,
                    },
                    lookup,
                    columns,
                })
                .ok_or_else(|| {
                    vec![
                        "needs a query, or attach = true to read a DuckDB file in place".to_owned(),
                    ]
                })
        }
        (attach, query, max_rows, max_bytes, timeout, lookup, columns) => {
            let mut errors = Vec::new();
            errors.extend(attach.err());
            errors.extend(query.err());
            errors.extend(max_rows.err());
            errors.extend(max_bytes.err());
            errors.extend(timeout.err());
            errors.extend(lookup.err().into_iter().flatten());
            errors.extend(columns.err().into_iter().flatten());
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
    problems.extend(
        found
            .into_iter()
            .map(|problem| format!("{label}: {problem}")),
    );
    source
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
        fields.string(key).and_then(|text| {
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
    let batch_size = fields.count("batch_size").map(|size| {
        size.and_then(|size| usize::try_from(size).ok())
            .and_then(NonZeroUsize::new)
    });
    let mut problems: Vec<String> = fields
        .unknown("a lookup", "from, column, as and batch_size")
        .into_iter()
        .map(|problem| format!("lookup: {problem}"))
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
    let name = fields.string("name").map_err(|problem| (None, problem))?;
    let label = name.as_ref().map(|name| format!("{name:?}"));
    let fail = |problem: String| (label.clone(), problem);
    let name = name.filter(|name| !name.trim().is_empty()).ok_or_else(|| {
        fail("needs a name, the column it makes in the source's table".to_owned())
    })?;
    let from = fields
        .string("from")
        .map_err(fail)?
        .filter(|from| !from.is_empty())
        .unwrap_or_else(|| name.clone());
    let column_type = fields
        .string("type")
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
        return Err(fail(problem));
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
    params: &BTreeMap<&str, ColumnType>,
    problems: &mut Vec<String>,
) {
    for (index, source) in sources.iter().enumerate() {
        let SourceRead::Query {
            lookup: Some(lookup),
            ..
        } = &source.read
        else {
            continue;
        };
        let label = format!("source {:?}", source.id);
        if params.contains_key(lookup.parameter.as_str()) {
            problems.push(format!(
                "{label}: lookup: :{} is one of [report] params; a lookup's values need a name \
                 of their own",
                lookup.parameter
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
        problems.extend(problem);
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

fn read_combine(table: toml::Table, problems: &mut Vec<String>) -> Option<Combine> {
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
    problems.extend(
        found
            .into_iter()
            .map(|problem| format!("[combine]: {problem}")),
    );
    combine
}

/// The combine step's query from its `query` and `prql` keys, of which it has one; PRQL
/// compiled. A PRQL that doesn't compile can give several problems.
fn combine_query(
    sql: Result<Option<String>, String>,
    prql: Result<Option<String>, String>,
) -> Result<CombineQuery, Vec<String>> {
    let given = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
    match (sql.map(given), prql.map(given)) {
        (Err(problem), _) | (_, Err(problem)) => Err(vec![problem]),
        (Ok(Some(_)), Ok(Some(_))) => Err(vec![
            "has both a query and prql; it runs one, so keep the one you mean".to_owned(),
        ]),
        (Ok(Some(sql)), Ok(None)) => Ok(CombineQuery::Sql(sql)),
        (Ok(None), Ok(Some(prql))) => {
            crate::prql::compile(&prql).map(|compiled| CombineQuery::Prql { prql, compiled })
        }
        (Ok(None), Ok(None)) => Err(vec![
            "needs a query, the DuckDB SQL that combines the sources' tables, or prql, the same \
             in PRQL"
                .to_owned(),
        ]),
    }
}

fn read_outputs(
    outputs: Vec<toml::Table>,
    params: &BTreeMap<&str, ColumnType>,
    problems: &mut Vec<String>,
) -> Vec<Output> {
    if outputs.is_empty() {
        problems.push("the file has no [[output]] table, for a file to write".to_owned());
    }
    let mut read = Vec::new();
    for (index, table) in outputs.into_iter().enumerate() {
        let label = format!("output {}", index + 1);
        let mut found = Vec::new();
        let output = read_output(table, params, index == 0, &mut found);
        problems.extend(
            found
                .into_iter()
                .map(|problem| format!("{label}: {problem}")),
        );
        if let Some(output) = output {
            if read
                .iter()
                .any(|kept: &Output| kept.file.as_str() == output.file.as_str())
            {
                problems.push(format!(
                    "{label}: another output already writes {:?}; several outputs in one \
                     file, as sheets of one workbook, come after RP1",
                    output.file.as_str()
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
    problems: &mut Vec<String>,
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
                format!(
                    "{key:?} can't be set on an output with a preset, whose options are set in \
                     exports.toml"
                )
            }));
            match CustomFormatId::new(&id) {
                Some(id) => Some(OutputWriter::Preset(id)),
                None => {
                    problems.push(format!(
                        "{id:?} can't be the id of a preset in exports.toml"
                    ));
                    None
                }
            }
        }
        (Ok(Some(_)), Ok(Some(_))) => {
            problems
                .push("an output has a format or a preset from exports.toml, not both".to_owned());
            None
        }
        (Ok(None), Ok(None)) => {
            problems.push(
                "needs a format, as format = \"xlsx\" or \"csv\", or a preset from exports.toml, \
                 as preset = \"semicolon-csv\""
                    .to_owned(),
            );
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
            .map_err(|problem| format!("file: {problem}")),
        Ok(None) => Err("needs a file name, as file = \"trial-balance.xlsx\"".to_owned()),
        Err(problem) => Err(problem),
    };
    let columns = read_columns(columns, first);
    let output = match (writer, file, columns, max_size, continue_on_new_sheet) {
        (Some(writer), Ok(file), Ok(columns), Ok(max_size), Ok(continue_on_new_sheet)) => {
            let continue_on_new_sheet = continue_on_new_sheet.unwrap_or(false);
            if continue_on_new_sheet && !writes_excel(&writer) {
                problems.push("continue_on_new_sheet is for Excel outputs".to_owned());
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

/// Whether the writer is known to write Excel files before the presets are looked up: a
/// preset's format is checked when the report is planned.
fn writes_excel(writer: &OutputWriter) -> bool {
    match writer {
        OutputWriter::Format(preset) => preset.format == ExportFormat::Xlsx,
        OutputWriter::Preset(_) => true,
    }
}

/// `format = "csv"` or `"xlsx"`, with the output's other keys as that format's options.
fn read_format(format: &str, fields: Fields, problems: &mut Vec<String>) -> Option<OutputWriter> {
    let format = match ExportFormat::from_id(format) {
        Some(format @ (ExportFormat::Csv | ExportFormat::Xlsx)) => format,
        Some(other) => {
            problems.push(format!(
                "a report writes CSV or Excel files in RP1, not {}",
                other.name()
            ));
            return None;
        }
        None => {
            problems.push(format!(
                "{format:?} is not a format; a report writes \"csv\" or \"xlsx\""
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
                problems.push(format!(
                    "{key:?} is not part of an output; an output has {}, and {}'s options {}",
                    OUTPUT_KEYS.join(", "),
                    format.name(),
                    names.join(", ")
                ));
            }
            known
        })
        .collect();
    match Preset::read(format, toml::Value::Table(options)) {
        Ok(preset) => Some(OutputWriter::Format(preset)),
        Err(error) => {
            problems.push(error.message().trim().to_owned());
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
    let name = fields.string("name").map_err(|problem| (None, problem))?;
    let label = name.as_ref().map(|name| format!("{name:?}"));
    let fail = |problem: String| (label.clone(), problem);
    let name = name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| fail("needs a name, the header it writes".to_owned()))?;
    let from = fields.string("from").map_err(fail)?;
    let column_type = fields
        .string("type")
        .map_err(fail)?
        .map(|type_name| type_name.parse::<ColumnType>())
        .transpose()
        .map_err(|error| fail(error.to_string()))?;
    let excel_format = fields.string("format").map_err(fail)?;
    let time_zone = fields
        .string("time_zone")
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
        return Err(fail(problem));
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
struct Fields(toml::Table);

impl Fields {
    fn new(table: toml::Table) -> Self {
        Self(table)
    }

    fn take(&mut self, key: &str) -> Option<toml::Value> {
        self.0.remove(key)
    }

    fn string(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text)),
            Some(other) => Err(format!(
                "{key} must be a string, not a {}",
                other.type_str()
            )),
        }
    }

    fn boolean(&mut self, key: &str) -> Result<Option<bool>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Boolean(value)) => Ok(Some(value)),
            Some(other) => Err(format!(
                "{key} must be true or false, not a {}",
                other.type_str()
            )),
        }
    }

    /// A whole number greater than zero.
    fn count(&mut self, key: &str) -> Result<Option<u64>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Integer(number)) if number > 0 => Ok(u64::try_from(number).ok()),
            Some(_) => Err(format!("{key} must be a whole number greater than zero")),
        }
    }

    /// A size, as `"500 MB"`, or a number of bytes.
    fn byte_size(&mut self, key: &str) -> Result<Option<ByteSize>, String> {
        let invalid = || format!("{key} must be a size such as \"500 MB\" or \"1 GiB\"");
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
    fn duration(&mut self, key: &str) -> Result<Option<Duration>, String> {
        let invalid = || format!("{key} must be a duration such as \"30s\" or \"10m\"");
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
    fn table(&mut self, key: &str) -> Result<Option<toml::Table>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Table(table)) => Ok(Some(table)),
            Some(_) if key == "report" => Err("report must be a [report] table".to_owned()),
            Some(_) => Err(format!("{key} must be a table, as {key} = {{ … }}")),
        }
    }

    /// The `[[key]]` tables; none if the key isn't there.
    fn tables(&mut self, key: &str) -> Result<Vec<toml::Table>, String> {
        let not_tables = || format!("{key} must be a list of [[{key}]] tables");
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
    fn unknown(&self, what: &str, has: &str) -> Vec<String> {
        self.0
            .keys()
            .map(|key| format!("{key:?} is not part of {what}, which has {has}"))
            .collect()
    }
}
