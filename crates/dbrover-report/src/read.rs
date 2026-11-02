//! Reading a report file: TOML, checked as a whole. A report with a problem can't run, so
//! unlike `exports.toml` nothing is skipped: every problem found is listed, each a sentence
//! that names where it is (`[report]`, `source "balances"`, `output 2, column "Debit"`).

use std::collections::BTreeMap;
use std::time::Duration;

use dbrover_export::{ColumnType, CustomFormatId, ExportFormat, IfInvalid, Preset, SchemaColumn};

use crate::{
    ByteSize, DEFAULT_RUN_TIMEOUT, FileName, Output, OutputColumns, OutputLimits, OutputWriter,
    Param, Report, Source, SourceLimits,
};

/// Keys of later milestones, and what each waits for, so a file written for one says why it
/// can't run yet rather than running something else.
const LATER_REPORT_KEYS: [(&str, &str); 2] = [
    (
        "combine",
        "[combine] combines several sources in DuckDB, which comes with RP2",
    ),
    ("prql", "PRQL comes with RP5"),
];
const LATER_SOURCE_KEYS: [(&str, &str); 3] = [
    (
        "lookup",
        "a lookup reads by the keys an earlier source found, which comes with RP3",
    ),
    (
        "attach",
        "attaching a DuckDB data source to the combine step comes with RP2",
    ),
    (
        "columns",
        "a source's declared columns come with RP3, for document data sources",
    ),
];

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
    let outputs = file.tables("output");
    for (key, later) in LATER_REPORT_KEYS {
        if file.take(key).is_some() {
            problems.push(later.to_owned());
        }
    }
    problems.extend(file.unknown("a report file", "[report], [[source]] and [[output]]"));

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

    let source = match sources {
        Ok(sources) => read_sources(sources, &mut problems),
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    let outputs = match outputs {
        Ok(outputs) => read_outputs(outputs, &params, &mut problems),
        Err(problem) => {
            problems.push(problem);
            Vec::new()
        }
    };

    match (header, source) {
        (Some(header), Some(source)) if problems.is_empty() => Ok(Report {
            name: header.name,
            description: header.description,
            params: header.params,
            timeout: header.timeout,
            source,
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
    timeout: Duration,
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
    for (key, later) in LATER_REPORT_KEYS {
        if fields.take(key).is_some() {
            found.push(later.to_owned());
        }
    }
    found.extend(fields.unknown("[report]", "name, description, params and timeout"));
    let params = params.map(|params| read_params(params, &mut found));
    let header = match (name, description, params, timeout) {
        (Ok(name), Ok(description), Ok(params), Ok(timeout)) => Some(Header {
            name,
            description: description.filter(|text| !text.trim().is_empty()),
            params,
            timeout: timeout.unwrap_or(DEFAULT_RUN_TIMEOUT),
        }),
        (name, description, params, timeout) => {
            found.extend(name.err());
            found.extend(description.err());
            found.extend(params.err());
            found.extend(timeout.err());
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

fn read_sources(sources: Vec<toml::Table>, problems: &mut Vec<String>) -> Option<Source> {
    let count = sources.len();
    let mut sources = sources.into_iter();
    let Some(first) = sources.next() else {
        problems.push(
            "the file has no [[source]] table, with the query whose rows the report writes"
                .to_owned(),
        );
        return None;
    };
    if count > 1 {
        problems.push(format!(
            "the file has {count} [[source]] tables; a report reads one until RP2 combines \
             several"
        ));
    }
    read_source(first, problems)
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
        id.filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "needs an id, as id = \"balances\"".to_owned())
    });
    let data_source = fields.string("data_source").and_then(|name| {
        name.filter(|name| !name.trim().is_empty()).ok_or_else(|| {
            "needs the data source it reads, by name: data_source = \"ledger\"".to_owned()
        })
    });
    let query = fields.string("query").and_then(|query| {
        query
            .filter(|query| !query.trim().is_empty())
            .ok_or_else(|| "needs a query".to_owned())
    });
    let max_rows = fields.count("max_rows");
    let max_bytes = fields.byte_size("max_bytes");
    let timeout = fields.duration("timeout");
    for (key, later) in LATER_SOURCE_KEYS {
        if fields.take(key).is_some() {
            found.push(later.to_owned());
        }
    }
    found.extend(fields.unknown(
        "a source",
        "id, data_source, query, max_rows, max_bytes and timeout",
    ));
    let defaults = SourceLimits::default();
    let source = match (id, data_source, query, max_rows, max_bytes, timeout) {
        (Ok(id), Ok(data_source), Ok(query), Ok(max_rows), Ok(max_bytes), Ok(timeout)) => {
            Some(Source {
                id,
                data_source,
                query,
                limits: SourceLimits {
                    max_rows: max_rows.unwrap_or(defaults.max_rows),
                    max_bytes: max_bytes.unwrap_or(defaults.max_bytes),
                    timeout: timeout.unwrap_or(defaults.timeout),
                },
            })
        }
        (id, data_source, query, max_rows, max_bytes, timeout) => {
            found.extend(id.err());
            found.extend(data_source.err());
            found.extend(query.err());
            found.extend(max_rows.err());
            found.extend(max_bytes.err());
            found.extend(timeout.err());
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
                limits: OutputLimits {
                    max_size: max_size.unwrap_or(OutputLimits::default().max_size),
                },
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
