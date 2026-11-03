//! A report against what it names: its data sources and `exports.toml`'s presets, found and
//! checked, so that a run needs nothing but its parameters' values.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::{ConnectionConfig, DataSource, DuckDbDatabase};
use dbrover_export::{
    CellTarget, ColumnType, CsvOptions, CustomFormat, CustomFormatKind, ExportFormat, ExportSpec,
    SchemaColumn, XlsxOptions,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, ParameterValues, StatementKind};

use crate::{
    CombineLimits, FileName, Output, OutputColumns, OutputLimits, OutputWriter, Param, Report,
    Source, SourceLimits, SourceRead, SourceStart,
};

/// A report ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    pub description: Option<String>,
    /// In the order the queries first use them, sources first, then those only file names
    /// use, as the file lists them: the order the tab asks for them in.
    pub params: Vec<Param>,
    pub timeout: Duration,
    pub logic: Logic,
    pub outputs: Vec<PlannedOutput>,
}

/// Where the rows the outputs write come from.
#[derive(Debug, Clone, PartialEq)]
pub enum Logic {
    /// One source's rows, as it returns them, without DuckDB. Boxed: a data source's settings
    /// are large beside a list of sources.
    Direct(Box<QuerySource>),
    /// The combine step's rows, over every source loaded or attached.
    Combine(Combined),
}

/// The sources a combine step reads, and the step.
#[derive(Debug, Clone, PartialEq)]
pub struct Combined {
    /// As the file lists them.
    pub sources: Vec<PlannedSource>,
    pub combine: PlannedCombine,
}

/// A source with its data source found.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedSource {
    Query(QuerySource),
    Attached(AttachedSource),
}

impl PlannedSource {
    pub fn id(&self) -> &str {
        match self {
            Self::Query(source) => &source.id,
            Self::Attached(source) => &source.id,
        }
    }

    pub fn data_source(&self) -> &DataSource {
        match self {
            Self::Query(source) => &source.data_source,
            Self::Attached(source) => &source.data_source,
        }
    }
}

/// A source whose query's rows the run reads.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySource {
    pub id: String,
    pub data_source: DataSource,
    /// The data source's SQL dialect, which its query and parameters are read in.
    pub dialect: Dialect,
    pub query: String,
    pub limits: SourceLimits,
}

/// A DuckDB file the combine step reads in place.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachedSource {
    pub id: String,
    pub data_source: DataSource,
    pub path: PathBuf,
}

/// The combine step's query, in DuckDB's SQL, and the limits of the DuckDB it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCombine {
    pub query: String,
    pub limits: CombineLimits,
}

/// An output, with its preset found and its columns known.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedOutput {
    pub format: OutputFormat,
    pub file: FileName,
    /// `None` writes every column of the result as it comes.
    pub columns: Option<Vec<SchemaColumn>>,
    pub limits: OutputLimits,
    pub continue_on_new_sheet: bool,
}

/// The formats a report writes in RP1, with their options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Csv(CsvOptions),
    Xlsx(XlsxOptions),
}

impl OutputFormat {
    pub fn spec(&self) -> ExportSpec {
        match self {
            Self::Csv(options) => ExportSpec::Csv(options.clone()),
            Self::Xlsx(options) => ExportSpec::Xlsx(options.clone()),
        }
    }

    /// Where its converted values go, which decides how finely times are written.
    pub fn target(&self) -> CellTarget {
        match self {
            Self::Csv(_) => CellTarget::Text,
            Self::Xlsx(_) => CellTarget::Excel,
        }
    }

    /// Whether the file is a workbook, which holds so many rows a sheet.
    pub fn is_excel(&self) -> bool {
        matches!(self, Self::Xlsx(_))
    }
}

/// The values of a run's parameters as typed, by name.
pub type TypedValues = BTreeMap<String, String>;

/// A run's parameters, checked, and the files they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunValues {
    /// Each value in its type's plain form (`2026-10-01`), as the query is sent them.
    pub parameters: ParameterValues,
    /// Each output's file name, in the outputs' order.
    pub files: Vec<String>,
}

impl Plan {
    /// Finds what `report` names among `data_sources` and `presets`.
    ///
    /// # Errors
    ///
    /// Every problem found, each a sentence naming where it is.
    pub fn new(
        report: Report,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
    ) -> Result<Self, Vec<String>> {
        let mut problems = Vec::new();
        let sources: Vec<PlannedSource> = report
            .sources
            .iter()
            .filter_map(|source| {
                plan_source(source, data_sources)
                    .map_err(|problem| {
                        problems.push(format!("source {:?}: {problem}", source.id));
                    })
                    .ok()
            })
            .collect();
        let combine = report.combine.map(|combine| {
            if let Err(problem) = check_combine(&combine.query) {
                problems.push(format!("[combine]: {problem}"));
            }
            PlannedCombine {
                query: combine.query,
                limits: combine.limits,
            }
        });
        let outputs = plan_outputs(&report.outputs, presets, &mut problems);
        let params = if sources.len() == report.sources.len() {
            ordered_params(
                &report.params,
                &queries(&sources, combine.as_ref()),
                &mut problems,
            )
        } else {
            report.params.clone()
        };
        let logic = match (combine, <[PlannedSource; 1]>::try_from(sources)) {
            (Some(combine), sources) => Logic::Combine(Combined {
                sources: sources.map_or_else(|sources| sources, Vec::from),
                combine,
            }),
            (None, Ok([PlannedSource::Query(source)])) => Logic::Direct(Box::new(source)),
            // Reading the file found what makes these two cases, a source left unplanned
            // above, or an attached source without [combine], and listed it.
            (None, _) => return Err(problems),
        };
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Self {
            name: report.name,
            description: report.description,
            params,
            timeout: report.timeout,
            logic,
            outputs,
        })
    }

    /// Each source, as the run's start lists them.
    pub fn source_starts(&self) -> Vec<SourceStart> {
        let start = |source: &PlannedSource| SourceStart {
            id: source.id().to_owned(),
            data_source: source.data_source().name.clone(),
            attached: matches!(source, PlannedSource::Attached(_)),
        };
        match &self.logic {
            Logic::Direct(source) => vec![SourceStart {
                id: source.id.clone(),
                data_source: source.data_source.name.clone(),
                attached: false,
            }],
            Logic::Combine(combined) => combined.sources.iter().map(start).collect(),
        }
    }

    /// Checks `typed` against the parameters' types, and names each output's file.
    ///
    /// # Errors
    ///
    /// One sentence per parameter whose value is missing or isn't of its type, or per file
    /// name the values make unusable.
    pub fn values(&self, typed: &TypedValues) -> Result<RunValues, Vec<String>> {
        let mut problems = Vec::new();
        let mut checked: BTreeMap<&str, (ColumnType, String)> = BTreeMap::new();
        for param in &self.params {
            let value = typed
                .get(&param.name)
                .map(String::as_str)
                .unwrap_or_default();
            if value.trim().is_empty() && param.param_type != ColumnType::Text {
                problems.push(format!("{} needs a value", param.name));
                continue;
            }
            match param.param_type.plain_form(value.trim()) {
                Ok(plain) => {
                    checked.insert(&param.name, (param.param_type, plain));
                }
                Err(reason) => problems.push(format!("{}: {value:?} {reason}", param.name)),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let by_name: BTreeMap<&str, (ColumnType, &str)> = checked
            .iter()
            .map(|(name, (param_type, plain))| (*name, (*param_type, plain.as_str())))
            .collect();
        let mut files: Vec<String> = Vec::new();
        for (index, output) in self.outputs.iter().enumerate() {
            match output.file.render(&by_name) {
                Ok(file) if files.contains(&file) => problems.push(format!(
                    "output {}: another output already writes {file:?} with these values",
                    index + 1
                )),
                Ok(file) => files.push(file),
                Err(problem) => problems.push(format!("output {}: {problem}", index + 1)),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let parameters = checked
            .into_iter()
            .map(|(name, (_, plain))| (name, Some(plain)))
            .collect();
        Ok(RunValues { parameters, files })
    }
}

fn plan_source(source: &Source, data_sources: &[DataSource]) -> Result<PlannedSource, String> {
    let data_source = find_data_source(&source.data_source, data_sources)?;
    match &source.read {
        SourceRead::Query { query, limits } => {
            plan_query(&source.id, data_source, query, *limits).map(PlannedSource::Query)
        }
        SourceRead::Attach => attach(&source.id, data_source).map(PlannedSource::Attached),
    }
}

fn plan_query(
    id: &str,
    data_source: &DataSource,
    query: &str,
    limits: SourceLimits,
) -> Result<QuerySource, String> {
    let driver = data_source.config.driver();
    let dialect = QueryLanguage::from(driver).dialect().ok_or_else(|| {
        format!(
            "{:?} is a {} data source, which a report reads once RP3 declares the columns \
             its results have; until then a source is on a SQL data source",
            data_source.name,
            driver.display_name()
        )
    })?;
    Ok(QuerySource {
        id: id.to_owned(),
        data_source: data_source.clone(),
        dialect,
        query: query.to_owned(),
        limits,
    })
}

/// The file an attached source reads, which must be a DuckDB file.
fn attach(id: &str, data_source: &DataSource) -> Result<AttachedSource, String> {
    let path = match &data_source.config {
        ConnectionConfig::DuckDb(config) => match &config.database {
            DuckDbDatabase::File { path, .. } => path.clone(),
            DuckDbDatabase::InMemory => {
                return Err(format!(
                    "{:?} is a DuckDB database in memory, which has nothing to attach; attach \
                     a DuckDB file, or give the source a query",
                    data_source.name
                ));
            }
        },
        other => {
            return Err(format!(
                "attach = true reads a DuckDB file in place, and {:?} is a {} data source; \
                 give the source a query instead",
                data_source.name,
                other.driver().display_name()
            ));
        }
    };
    Ok(AttachedSource {
        id: id.to_owned(),
        data_source: data_source.clone(),
        path,
    })
}

/// First words of a statement that reads, in DuckDB.
const QUERY_WORDS: [&str; 8] = [
    "from",
    "pivot",
    "select",
    "summarize",
    "table",
    "unpivot",
    "values",
    "with",
];

/// Whether the combine query is one statement that only reads.
fn check_combine(query: &str) -> Result<(), String> {
    let statements = dbrover_sql::split_statements(query, Dialect::DuckDb).len();
    if statements != 1 {
        return Err(format!(
            "the query holds {statements} statements; the combine step is one query that only \
             reads"
        ));
    }
    match QueryLanguage::Sql(Dialect::DuckDb).classify(query) {
        Ok(StatementKind::Read) => Ok(()),
        Ok(kind) => Err(format!(
            "the query {}; the combine step only reads the sources' tables",
            kind.description()
        )),
        // The parser behind `classify` doesn't read all of DuckDB's SQL (`ASOF JOIN … ON`, for
        // one), so a query it can't parse only has to start as one. The run's DuckDB is the
        // guard that holds: locked down, with its attached files read-only, it refuses what
        // such a query might try beyond reading.
        Err(_) if starts_as_query(query) => Ok(()),
        Err(unclassified) => Err(format!(
            "{unclassified}; the combine step is one query that only reads"
        )),
    }
}

fn starts_as_query(query: &str) -> bool {
    dbrover_sql::first_word(query, Dialect::DuckDb)
        .is_some_and(|word| QUERY_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

/// Each query the run sends, with the dialect it is read in, sources first.
fn queries<'a>(
    sources: &'a [PlannedSource],
    combine: Option<&'a PlannedCombine>,
) -> Vec<(String, &'a str, Dialect)> {
    let sources = sources.iter().filter_map(|source| match source {
        PlannedSource::Query(source) => Some((
            format!("source {:?}", source.id),
            source.query.as_str(),
            source.dialect,
        )),
        PlannedSource::Attached(_) => None,
    });
    let combine = combine.map(|combine| {
        (
            "[combine]".to_owned(),
            combine.query.as_str(),
            Dialect::DuckDb,
        )
    });
    sources.chain(combine).collect()
}

/// The data source `wanted` names: by name if one has it, else by id.
fn find_data_source<'a>(
    wanted: &str,
    data_sources: &'a [DataSource],
) -> Result<&'a DataSource, String> {
    let named: Vec<&DataSource> = data_sources
        .iter()
        .filter(|data_source| data_source.name == wanted)
        .collect();
    match named.as_slice() {
        [one] => return Ok(one),
        [] => {}
        several => {
            let ids: Vec<String> = several
                .iter()
                .map(|data_source| data_source.id.to_string())
                .collect();
            return Err(format!(
                "{} data sources are named {wanted:?}; name one by its id instead: {}",
                several.len(),
                ids.join(", ")
            ));
        }
    }
    data_sources
        .iter()
        .find(|data_source| data_source.id.to_string() == wanted)
        .ok_or_else(|| {
            let mut names: Vec<&str> = data_sources
                .iter()
                .map(|data_source| data_source.name.as_str())
                .collect();
            names.sort_unstable();
            if names.is_empty() {
                format!("there is no data source {wanted:?}, nor any other yet")
            } else {
                format!(
                    "there is no data source {wanted:?}; there are {}",
                    names.join(", ")
                )
            }
        })
}

/// The parameters in the order the queries first use them, then the rest, with a problem for
/// each `:name` the report doesn't declare.
fn ordered_params(
    params: &[Param],
    queries: &[(String, &str, Dialect)],
    problems: &mut Vec<String>,
) -> Vec<Param> {
    let mut ordered: Vec<Param> = Vec::new();
    for (label, query, dialect) in queries {
        for name in dbrover_sql::parameter_names(query, *dialect) {
            if ordered.iter().any(|param| param.name == name) {
                continue;
            }
            match params.iter().find(|param| param.name == name) {
                Some(param) => ordered.push(param.clone()),
                None => problems.push(format!(
                    "{label}: the query uses :{name}, which [report] params doesn't declare; \
                     add {name} = \"text\" (or its type) to params"
                )),
            }
        }
    }
    let unused: Vec<Param> = params
        .iter()
        .filter(|param| !ordered.iter().any(|used| used.name == param.name))
        .cloned()
        .collect();
    ordered.extend(unused);
    ordered
}

fn plan_outputs(
    outputs: &[Output],
    presets: &[CustomFormat],
    problems: &mut Vec<String>,
) -> Vec<PlannedOutput> {
    let first_columns = outputs.first().and_then(|first| match &first.columns {
        OutputColumns::Declared(columns) => Some(columns.clone()),
        OutputColumns::All | OutputColumns::Same => None,
    });
    let mut planned = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let label = format!("output {}", index + 1);
        let format = match &output.writer {
            OutputWriter::Format(preset) => built_in(preset.format, &preset.options),
            OutputWriter::Preset(id) => find_preset(id.as_str(), presets),
        };
        let format = match format {
            Ok(format) => format,
            Err(problem) => {
                problems.push(format!("{label}: {problem}"));
                continue;
            }
        };
        if output.continue_on_new_sheet && !format.is_excel() {
            problems.push(format!(
                "{label}: continue_on_new_sheet is for Excel outputs"
            ));
        }
        let columns = match &output.columns {
            OutputColumns::All => None,
            OutputColumns::Declared(columns) => Some(columns.clone()),
            OutputColumns::Same => first_columns.clone(),
        };
        planned.push(PlannedOutput {
            format,
            file: output.file.clone(),
            columns,
            limits: output.limits,
            continue_on_new_sheet: output.continue_on_new_sheet,
        });
    }
    planned
}

fn built_in(
    format: ExportFormat,
    options: &dbrover_export::ExportOptions,
) -> Result<OutputFormat, String> {
    match format {
        ExportFormat::Csv => Ok(OutputFormat::Csv(options.csv.clone())),
        ExportFormat::Xlsx => Ok(OutputFormat::Xlsx(options.xlsx.clone())),
        other => Err(format!(
            "a report writes CSV or Excel files in RP1, not {}",
            other.name()
        )),
    }
}

fn find_preset(id: &str, presets: &[CustomFormat]) -> Result<OutputFormat, String> {
    let Some(found) = presets.iter().find(|format| format.id.as_str() == id) else {
        return Err(format!(
            "exports.toml has no export with the id {id:?}; give the output a format, or add \
             the preset to exports.toml"
        ));
    };
    match &found.kind {
        CustomFormatKind::Preset(preset) => built_in(preset.format, &preset.options)
            .map_err(|problem| format!("the preset {id:?}: {problem}")),
        CustomFormatKind::Template(_) | CustomFormatKind::Program(_) => Err(format!(
            "{id:?} in exports.toml is a template or a program; a report's output is a CSV or \
             Excel preset in RP1"
        )),
    }
}
