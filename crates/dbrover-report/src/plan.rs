//! A report against what it names: its data sources and `exports.toml`'s presets, found and
//! checked, so that a run needs nothing but its parameters' values.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::{ConnectionConfig, DataSource, DuckDbDatabase};
use dbrover_export::{
    CellTarget, ColumnType, CsvOptions, CustomFormat, CustomFormatKind, ExportFormat, ExportSpec,
    SchemaColumn, XlsxOptions,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, ParameterValues, StatementKind};

use crate::problem::{Place, ReportProblem, Table, messages};
use crate::{
    CombineLimits, CombineLimitsSet, CombineQuery, FileName, LimitDefaults, LimitSetIn, Lookup,
    Output, OutputColumns, OutputLimits, OutputLimitsSet, OutputWriter, Param, Report, Source,
    SourceColumn, SourceLimits, SourceLimitsSet, SourceRead, SourceStart, Step,
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
    /// The limits the report's file sets, so a run stopped at one can say who set it.
    file_limits: FileLimits,
}

/// The limits a report's file sets, by where they are set; every other limit is a default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileLimits {
    /// `[report] timeout`.
    timeout: bool,
    /// By source id.
    sources: BTreeMap<String, SourceLimitsSet>,
    combine: CombineLimitsSet,
    /// In the outputs' order.
    outputs: Vec<OutputLimitsSet>,
}

impl FileLimits {
    fn of(report: &Report) -> Self {
        Self {
            timeout: report.timeout.is_some(),
            sources: report
                .sources
                .iter()
                .filter_map(|source| match &source.read {
                    SourceRead::Query { limits, .. } => Some((source.id.clone(), *limits)),
                    SourceRead::Attach => None,
                })
                .collect(),
            combine: report
                .combine
                .as_ref()
                .map(|combine| combine.limits)
                .unwrap_or_default(),
            outputs: report.outputs.iter().map(|output| output.limits).collect(),
        }
    }

    /// Whether the file sets `key` where `step` stopped at it; `None` for a key no default
    /// holds, such as an Excel sheet's rows.
    fn sets(&self, step: &Step, key: &str) -> Option<bool> {
        match (step, key) {
            (Step::Worker | Step::Report, "timeout") => Some(self.timeout),
            (Step::Source { id }, _) => {
                let set = self.sources.get(id).copied().unwrap_or_default();
                match key {
                    "max_rows" => Some(set.max_rows.is_some()),
                    "max_bytes" => Some(set.max_bytes.is_some()),
                    "timeout" => Some(set.timeout.is_some()),
                    _ => None,
                }
            }
            (Step::Combine, "timeout") => Some(self.combine.timeout.is_some()),
            (Step::Combine, "memory_limit") => Some(self.combine.memory_limit.is_some()),
            (Step::Combine, "max_temp_size") => Some(self.combine.max_temp_size.is_some()),
            (Step::Output { number, .. }, "max_size") => {
                let set = number
                    .checked_sub(1)
                    .and_then(|index| self.outputs.get(index))
                    .copied()
                    .unwrap_or_default();
                Some(set.max_size.is_some())
            }
            _ => None,
        }
    }
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

impl Combined {
    /// The sources with a query, in the rounds they load in (`docs/reporting.md`,
    /// "Lookups"): the first holds every source without a lookup, and each later one the
    /// sources whose lookup reads a source of the round before it. A lookup only reads a
    /// source above it, so each source's round is known once the ones above it are.
    pub fn rounds(&self) -> Vec<Vec<&QuerySource>> {
        let mut round_of: Vec<(&str, usize)> = Vec::new();
        let mut rounds: Vec<Vec<&QuerySource>> = Vec::new();
        for source in &self.sources {
            let PlannedSource::Query(source) = source else {
                continue;
            };
            let round = source.lookup.as_ref().map_or(0, |lookup| {
                round_of
                    .iter()
                    .find(|(id, _)| id.eq_ignore_ascii_case(&lookup.from))
                    .map_or(0, |(_, round)| round + 1)
            });
            round_of.push((&source.id, round));
            if rounds.len() <= round {
                rounds.resize_with(round + 1, Vec::new);
            }
            rounds[round].push(source);
        }
        rounds
    }
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
    /// The data source's language, which its query and parameters are read in.
    pub language: QueryLanguage,
    pub query: String,
    pub limits: SourceLimits,
    pub lookup: Option<PlannedLookup>,
    /// The columns its table has, when the file declares them; otherwise the result's. Always
    /// declared for a language without a SQL dialect.
    pub columns: Option<Vec<SourceColumn>>,
}

/// A lookup, with the most values a statement of its source takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLookup {
    /// The source above whose loaded table the values are read from.
    pub from: String,
    pub column: String,
    pub parameter: String,
    pub batch_size: NonZeroUsize,
}

/// A DuckDB file the combine step reads in place.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachedSource {
    pub id: String,
    pub data_source: DataSource,
    pub path: PathBuf,
}

/// The combine step's query, and the limits of the DuckDB it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCombine {
    pub query: CombineQuery,
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
    /// Finds what `report` names among `data_sources` and `presets`, its limits being
    /// `defaults` where its file leaves them out.
    ///
    /// # Errors
    ///
    /// Every problem found, each a sentence naming where it is.
    pub fn new(
        report: Report,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
        defaults: &LimitDefaults,
    ) -> Result<Self, Vec<String>> {
        Self::with_places(report, data_sources, presets, defaults).map_err(messages)
    }

    /// Plans `report` as [`Plan::new`] does.
    ///
    /// # Errors
    ///
    /// Every problem found, each with its place in the report's file. A report that read
    /// without problems has every source and output its file has, in the file's order, so a
    /// source's or output's index here is its position in the file.
    pub fn with_places(
        report: Report,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
        defaults: &LimitDefaults,
    ) -> Result<Self, Vec<ReportProblem>> {
        let mut problems = Vec::new();
        let file_limits = FileLimits::of(&report);
        let sources: Vec<PlannedSource> = report
            .sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| {
                plan_source(source, data_sources, defaults.source)
                    .map_err(|(key, problem)| {
                        problems.push(ReportProblem::new(
                            format!("source {:?}: {problem}", source.id),
                            Place::Key {
                                table: Table::Source(index),
                                key: key.map(str::to_owned),
                            },
                        ));
                    })
                    .ok()
            })
            .collect();
        let combine = report.combine.map(|combine| {
            if let Err(problem) = check_combine(combine.query.sql()) {
                problems.push(ReportProblem::new(
                    format!("[combine]: {problem}"),
                    Place::key(Table::Combine, combine_key(&combine.query)),
                ));
            }
            PlannedCombine {
                query: combine.query,
                limits: combine.limits.or(defaults.combine),
            }
        });
        let outputs = plan_outputs(&report.outputs, presets, defaults.output, &mut problems);
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
            timeout: report.timeout.unwrap_or(defaults.run_timeout),
            logic,
            outputs,
            file_limits,
        })
    }

    /// Who set the limit `key` a run stopped at in `step`: the report's file, or Settings ›
    /// Reports' default where the file leaves it out; `None` for a limit no default holds.
    pub fn limit_set_in(&self, step: &Step, key: &str) -> Option<LimitSetIn> {
        self.file_limits.sets(step, key).map(|set| {
            if set {
                LimitSetIn::File
            } else {
                LimitSetIn::Settings
            }
        })
    }

    /// The processor time the run may use: its time limit on each of DuckDB's threads and
    /// one more, since DuckDB's threads use the processor at once.
    pub fn processor_time(&self) -> Duration {
        let threads = match &self.logic {
            Logic::Direct(_) => 0,
            Logic::Combine(combined) => combined.combine.limits.threads(),
        };
        self.timeout
            .saturating_mul(u32::try_from(threads + 1).unwrap_or(u32::MAX))
    }

    /// The SQL the combine step's PRQL compiled to; `None` without a combine step, or with one
    /// in SQL.
    pub fn compiled_combine(&self) -> Option<&str> {
        match &self.logic {
            Logic::Direct(_) => None,
            Logic::Combine(combined) => combined.combine.query.compiled(),
        }
    }

    /// Each source, as the run's start lists them.
    pub fn source_starts(&self) -> Vec<SourceStart> {
        let start = |source: &PlannedSource| SourceStart {
            id: source.id().to_owned(),
            data_source: source.data_source().name.clone(),
            attached: matches!(source, PlannedSource::Attached(_)),
            waits_for: match source {
                PlannedSource::Query(source) => {
                    source.lookup.as_ref().map(|lookup| lookup.from.clone())
                }
                PlannedSource::Attached(_) => None,
            },
        };
        match &self.logic {
            Logic::Direct(source) => vec![SourceStart {
                id: source.id.clone(),
                data_source: source.data_source.name.clone(),
                attached: false,
                waits_for: None,
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

/// A problem planning a source, and the key of its table it is about.
type KeyedProblem = (Option<&'static str>, String);

/// The key of `[combine]` that holds its query.
fn combine_key(query: &CombineQuery) -> &'static str {
    match query {
        CombineQuery::Sql(_) => "query",
        CombineQuery::Prql { .. } => "prql",
    }
}

fn plan_source(
    source: &Source,
    data_sources: &[DataSource],
    defaults: SourceLimits,
) -> Result<PlannedSource, KeyedProblem> {
    let data_source = find_data_source(&source.data_source, data_sources)
        .map_err(|problem| (Some("data_source"), problem))?;
    match &source.read {
        SourceRead::Query {
            query,
            limits,
            lookup,
            columns,
        } => plan_query(
            &source.id,
            data_source,
            QueryRead {
                query,
                limits: limits.or(defaults),
                lookup: lookup.as_ref(),
                columns: columns.as_ref(),
            },
        )
        .map(PlannedSource::Query),
        SourceRead::Attach => attach(&source.id, data_source)
            .map(PlannedSource::Attached)
            .map_err(|problem| (Some("attach"), problem)),
    }
}

/// What the file says of a source with a query.
struct QueryRead<'a> {
    query: &'a str,
    limits: SourceLimits,
    lookup: Option<&'a Lookup>,
    columns: Option<&'a Vec<SourceColumn>>,
}

fn plan_query(
    id: &str,
    data_source: &DataSource,
    read: QueryRead<'_>,
) -> Result<QuerySource, KeyedProblem> {
    let driver = data_source.config.driver();
    let language = QueryLanguage::from(driver);
    if language.dialect().is_none() && read.columns.is_none() {
        return Err((
            Some("columns"),
            format!(
                "{:?} is a {} data source, whose results have no fixed columns; declare the \
                 columns the source makes, as columns = [{{ name = \"symbol\", type = \"text\" \
                 }}]",
                data_source.name,
                driver.display_name()
            ),
        ));
    }
    let lookup = read
        .lookup
        .map(|lookup| plan_lookup(lookup, language, read.query))
        .transpose()
        .map_err(|problem| (Some("lookup"), problem))?;
    Ok(QuerySource {
        id: id.to_owned(),
        data_source: data_source.clone(),
        language,
        query: read.query.to_owned(),
        limits: read.limits,
        lookup,
        columns: read.columns.cloned(),
    })
}

/// `lookup` for a query in `language`: the query uses its parameter, and its batches are no
/// larger than a statement of the language takes.
fn plan_lookup(
    lookup: &Lookup,
    language: QueryLanguage,
    query: &str,
) -> Result<PlannedLookup, String> {
    let used = language
        .parameters(query)
        .iter()
        .any(|parameter| parameter.name == lookup.parameter);
    if !used {
        return Err(format!(
            "lookup: the query doesn't use :{}, which the lookup's values are bound to; read \
             by them, as WHERE {} IN :{}",
            lookup.parameter, lookup.column, lookup.parameter
        ));
    }
    let most = language.max_list_values();
    let batch_size = match lookup.batch_size {
        Some(size) if size > most => {
            return Err(format!(
                "lookup: batch_size is {size}, more than the {most} values a {} statement takes",
                language.display_name()
            ));
        }
        Some(size) => size,
        None => most,
    };
    Ok(PlannedLookup {
        from: lookup.from.clone(),
        column: lookup.column.clone(),
        parameter: lookup.parameter.clone(),
        batch_size,
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
            // A report's DuckDB is cut off from the network once its sources are in (RP2).
            DuckDbDatabase::MotherDuck { .. } => {
                return Err(format!(
                    "{:?} is a MotherDuck database, which a report can't attach; give the \
                     source a query",
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

/// A query the run sends: where it is, its text and language, and the parameter its lookup
/// binds, which isn't one of the report's.
struct Sent<'a> {
    label: String,
    place: Place,
    query: &'a str,
    language: QueryLanguage,
    lookup: Option<&'a str>,
}

/// Each query the run sends, sources first.
fn queries<'a>(sources: &'a [PlannedSource], combine: Option<&'a PlannedCombine>) -> Vec<Sent<'a>> {
    let sources = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| match source {
            PlannedSource::Query(source) => Some(Sent {
                label: format!("source {:?}", source.id),
                place: Place::key(Table::Source(index), "query"),
                query: source.query.as_str(),
                language: source.language,
                lookup: source
                    .lookup
                    .as_ref()
                    .map(|lookup| lookup.parameter.as_str()),
            }),
            PlannedSource::Attached(_) => None,
        });
    let combine = combine.map(|combine| Sent {
        label: "[combine]".to_owned(),
        place: Place::key(Table::Combine, combine_key(&combine.query)),
        query: combine.query.sql(),
        language: QueryLanguage::Sql(Dialect::DuckDb),
        lookup: None,
    });
    sources.chain(combine).collect()
}

/// The data source `wanted` names: by name if one has it, else by id.
pub(crate) fn find_data_source<'a>(
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
    queries: &[Sent<'_>],
    problems: &mut Vec<ReportProblem>,
) -> Vec<Param> {
    let mut ordered: Vec<Param> = Vec::new();
    for sent in queries {
        let label = &sent.label;
        let mut names: Vec<String> = Vec::new();
        for parameter in sent.language.parameters(sent.query) {
            if !names.contains(&parameter.name) {
                names.push(parameter.name);
            }
        }
        for name in names {
            if ordered.iter().any(|param| param.name == name) || sent.lookup == Some(&name) {
                continue;
            }
            match params.iter().find(|param| param.name == name) {
                Some(param) => ordered.push(param.clone()),
                None => problems.push(ReportProblem::new(
                    format!(
                        "{label}: the query uses :{name}, which [report] params doesn't \
                         declare; add {name} = \"text\" (or its type) to params"
                    ),
                    sent.place.clone(),
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
    defaults: OutputLimits,
    problems: &mut Vec<ReportProblem>,
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
        let writer_key = match &output.writer {
            OutputWriter::Format(_) => "format",
            OutputWriter::Preset(_) => "preset",
        };
        let format = match format {
            Ok(format) => format,
            Err(problem) => {
                problems.push(ReportProblem::new(
                    format!("{label}: {problem}"),
                    Place::key(Table::Output(index), writer_key),
                ));
                continue;
            }
        };
        if output.continue_on_new_sheet && !format.is_excel() {
            problems.push(ReportProblem::new(
                format!("{label}: continue_on_new_sheet is for Excel outputs"),
                Place::key(Table::Output(index), "continue_on_new_sheet"),
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
            limits: output.limits.or(defaults),
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
