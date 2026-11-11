//! A report against what it names: its data sources and `exports.toml`'s presets, found and
//! checked, so that a run needs nothing but its parameters' values.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::{BlobId, ConnectionConfig, DataSource, DuckDbDatabase};
use dbrover_export::{
    CellTarget, ColumnType, CsvOptions, CustomFormat, CustomFormatKind, ExportFormat, ExportSpec,
    ParquetOptions, SchemaColumn, XlsxOptions,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, ParameterValues, StatementKind};

use crate::events::{
    CombineCaps, Machine, PartitionStart, RunEvent, RunPartitionStart, SourceCaps,
};
use crate::problem::{Place, ReportProblem, Table, messages};
use crate::run::millis;
use crate::{
    ByteSize, Chart, CombineLimits, CombineLimitsSet, CombineQuery, FileName, LimitDefaults,
    LimitSetIn, Lookup, Output, OutputColumns, OutputLimits, OutputLimitsSet, OutputWriter,
    PART_FROM, PART_TO, Param, PartRange, PlannedPartition, Report, Source, SourceColumn,
    SourceLimits, SourceLimitsSet, SourcePartition, SourceRead, SourceSplit, SourceStart, Step,
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
    /// The most disk the run's folders may hold (RS3b).
    pub max_disk: ByteSize,
    /// How long the run may go without progress (RS3e).
    pub stall_timeout: Duration,
    /// The ranges the whole run goes through one at a time (RS4d).
    pub partition: Option<PlannedPartition>,
    pub logic: Logic,
    pub outputs: Vec<PlannedOutput>,
    /// The charts it draws, as its file lists them: a chart names nothing to look up.
    pub charts: Vec<Chart>,
    /// The limits the report's file sets, so a run stopped at one can say who set it.
    file_limits: FileLimits,
}

/// The limits a report's file sets, by where they are set; every other limit is a default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileLimits {
    /// `[report] timeout`.
    timeout: bool,
    /// `[report] max_disk`.
    max_disk: bool,
    /// `[report] stall_timeout`.
    stall_timeout: bool,
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
            max_disk: report.max_disk.is_some(),
            stall_timeout: report.stall_timeout.is_some(),
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
            (Step::Worker | Step::Report, "max_disk") => Some(self.max_disk),
            (Step::Worker | Step::Report, "stall_timeout") => Some(self.stall_timeout),
            (Step::Source { id }, _) => {
                let set = self.sources.get(id).copied().unwrap_or_default();
                match key {
                    "max_rows" => Some(set.max_rows.is_some()),
                    "max_bytes" => Some(set.max_bytes.is_some()),
                    "timeout" => Some(set.timeout.is_some()),
                    _ => None,
                }
            }
            // A chart's query runs in the combine step's DuckDB, under its limits.
            (Step::Combine | Step::Chart { .. }, "timeout") => Some(self.combine.timeout.is_some()),
            (Step::Combine | Step::Chart { .. }, "memory_limit") => {
                Some(self.combine.memory_limit.is_some())
            }
            (Step::Combine | Step::Chart { .. }, "max_temp_size") => {
                Some(self.combine.max_temp_size.is_some())
            }
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
            let round = source.split.lookup().map_or(0, |lookup| {
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

impl SourceStart {
    fn query(source: &QuerySource) -> Self {
        Self {
            id: source.id.clone(),
            data_source: source.data_source.name.clone(),
            attached: false,
            waits_for: source.split.lookup().map(|lookup| lookup.from.clone()),
            engine: Some(engine(&source.data_source)),
            limits: Some(SourceCaps::from(&source.limits)),
            partition: source.split.partition().map(|split| PartitionStart {
                unit: split.partition.partition.every.unit().to_owned(),
                parallel: u32::try_from(split.parallel().get()).unwrap_or(u32::MAX),
            }),
        }
    }
}

/// The engine a data source runs, as DbRover names it.
fn engine(data_source: &DataSource) -> String {
    data_source.config.driver().display_name().to_owned()
}

impl From<&SourceLimits> for SourceCaps {
    fn from(limits: &SourceLimits) -> Self {
        Self {
            max_rows: limits.max_rows,
            max_bytes: limits.max_bytes.0,
            timeout_ms: millis(limits.timeout),
        }
    }
}

impl From<&CombineLimits> for CombineCaps {
    fn from(limits: &CombineLimits) -> Self {
        Self {
            memory_limit: limits.memory_limit.0,
            max_temp_size: limits.max_temp_size.0,
            timeout_ms: millis(limits.timeout),
        }
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
    /// Whether its query is sent once, a batch of a lookup's values at a time, or a range at a
    /// time.
    pub split: PlannedSplit,
    /// The columns its table has, when the file declares them; otherwise the result's. Always
    /// declared for a language without a SQL dialect.
    pub columns: Option<Vec<SourceColumn>>,
}

/// How a source's query becomes the statements it sends, as planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedSplit {
    Whole,
    Lookup(PlannedLookup),
    Partition(PlannedSourcePartition),
}

impl PlannedSplit {
    pub fn lookup(&self) -> Option<&PlannedLookup> {
        match self {
            Self::Lookup(lookup) => Some(lookup),
            Self::Whole | Self::Partition(_) => None,
        }
    }

    pub fn partition(&self) -> Option<&PlannedSourcePartition> {
        match self {
            Self::Partition(partition) => Some(partition),
            Self::Whole | Self::Lookup(_) => None,
        }
    }

    /// The parameters the run binds itself, which aren't the report's: a lookup's values, or a
    /// range's bounds.
    fn bound(&self) -> Vec<&str> {
        match self {
            Self::Whole => Vec::new(),
            Self::Lookup(lookup) => vec![lookup.parameter.as_str()],
            Self::Partition(_) => vec![PART_FROM, PART_TO],
        }
    }
}

/// A source read in ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSourcePartition {
    pub partition: PlannedPartition,
    pub max_rows_per_part: Option<u64>,
}

impl PlannedSourcePartition {
    /// How many of its ranges are read at once: one unless the file says.
    pub fn parallel(&self) -> NonZeroUsize {
        self.partition
            .partition
            .parallel
            .unwrap_or(NonZeroUsize::MIN)
    }
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

impl PlannedOutput {
    /// Whether its file is one a range of a partitioned run: its name says the range's bounds.
    /// Any other output's file is one that every range appends to.
    pub fn is_per_range(&self) -> bool {
        self.file
            .params()
            .any(|param| param == PART_FROM || param == PART_TO)
    }
}

/// The formats a report writes, with their options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Csv(CsvOptions),
    Xlsx(XlsxOptions),
    /// Written by the combine step's DuckDB with `COPY`, not by DbRover's writers (RS3d).
    Parquet(ParquetOptions),
}

impl OutputFormat {
    pub fn spec(&self) -> ExportSpec {
        match self {
            Self::Csv(options) => ExportSpec::Csv(options.clone()),
            Self::Xlsx(options) => ExportSpec::Xlsx(options.clone()),
            Self::Parquet(options) => ExportSpec::Parquet(*options),
        }
    }

    /// Where its converted values go, which decides how finely times are written.
    pub fn target(&self) -> CellTarget {
        match self {
            Self::Csv(_) | Self::Parquet(_) => CellTarget::Text,
            Self::Xlsx(_) => CellTarget::Excel,
        }
    }

    /// The options of a file DuckDB writes itself, rather than a writer of DbRover's.
    pub fn written_by_duckdb(&self) -> Option<ParquetOptions> {
        match self {
            Self::Parquet(options) => Some(*options),
            Self::Csv(_) | Self::Xlsx(_) => None,
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
    /// Each chart's file name, in the charts' order.
    pub charts: Vec<String>,
    /// Each partitioned source's ranges, by its id.
    pub ranges: BTreeMap<String, SourceRanges>,
    /// A partitioned run's ranges (RS4d), whose values [`Plan::range_values`] makes.
    pub run_ranges: Option<SourceRanges>,
}

/// A partitioned source's ranges for a run, and the whole period they cover together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRanges {
    pub ranges: Vec<PartRange>,
    /// From the partition's `from` to its `to`, which the cost guard judges.
    pub whole: PartRange,
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
        let param_types: BTreeMap<&str, ColumnType> = report
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.param_type))
            .collect();
        let sources: Vec<PlannedSource> = report
            .sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| {
                plan_source(source, data_sources, defaults.source, &param_types)
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
        let partition = report.partition.and_then(|partition| {
            PlannedPartition::plan(partition, &param_types)
                .map_err(|problem| {
                    problems.push(ReportProblem::new(
                        format!("[report]: partition: {problem}"),
                        Place::key(Table::Report, "partition"),
                    ));
                })
                .ok()
        });
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
        let outputs = plan_outputs(
            &report.outputs,
            presets,
            defaults.output,
            combine.is_some(),
            &mut problems,
        );
        for (index, chart) in report.charts.iter().enumerate() {
            let Some(query) = &chart.query else {
                continue;
            };
            if let Err(problem) = check_reads(query, "a chart's query") {
                problems.push(ReportProblem::new(
                    format!("chart {}: {problem}", index + 1),
                    Place::key(Table::Chart(index), "query"),
                ));
            }
        }
        let params = if sources.len() == report.sources.len() {
            let sent = queries(
                &sources,
                combine.as_ref(),
                &report.charts,
                partition.is_some(),
            );
            if partition.is_some() {
                check_reads_ranges(&sent, &mut problems);
                check_appended_parquet(&outputs, &mut problems);
            }
            ordered_params(&report.params, &sent, &mut problems)
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
            max_disk: report.max_disk.unwrap_or(defaults.max_disk),
            stall_timeout: report.stall_timeout.unwrap_or(defaults.stall_timeout),
            partition,
            logic,
            outputs,
            charts: report.charts,
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

    /// The step whose rows the outputs write: the combine step, or the one source without
    /// one.
    pub fn feed_step(&self) -> Step {
        match &self.logic {
            Logic::Direct(source) => Step::Source {
                id: source.id.clone(),
            },
            Logic::Combine(_) => Step::Combine,
        }
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

    /// The event that starts a run of this plan with `values`: what it runs and under which
    /// limits, the Git blob id of the file's text (`version`), and the machine it runs on.
    pub fn started(&self, values: &RunValues, version: BlobId, machine: Machine) -> RunEvent {
        let combine_limits = match &self.logic {
            Logic::Direct(_) => None,
            Logic::Combine(combined) => Some(CombineCaps::from(&combined.combine.limits)),
        };
        RunEvent::Started {
            report: self.name.clone(),
            sources: self.source_starts(),
            combine: combine_limits.is_some(),
            combine_sql: self.compiled_combine().map(str::to_owned),
            version: Some(version),
            combine_limits,
            timeout_ms: Some(millis(self.timeout)),
            machine: Some(machine),
            partition: self.partition.as_ref().zip(values.run_ranges.as_ref()).map(
                |(partition, ranges)| RunPartitionStart {
                    unit: partition.partition.every.unit().to_owned(),
                    ranges: ranges.ranges.len() as u64,
                },
            ),
        }
    }

    /// Each source, as the run's start lists them.
    pub fn source_starts(&self) -> Vec<SourceStart> {
        match &self.logic {
            Logic::Direct(source) => vec![SourceStart::query(source)],
            Logic::Combine(combined) => combined
                .sources
                .iter()
                .map(|source| match source {
                    PlannedSource::Query(source) => SourceStart::query(source),
                    PlannedSource::Attached(attached) => SourceStart {
                        id: attached.id.clone(),
                        data_source: attached.data_source.name.clone(),
                        attached: true,
                        waits_for: None,
                        engine: Some(engine(&attached.data_source)),
                        limits: None,
                        partition: None,
                    },
                })
                .collect(),
        }
    }

    /// Checks `typed` against the parameters' types, and names each output's and chart's file.
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
        let run_ranges = self.run_ranges(&by_name, &mut problems);
        let (files, charts) = match &run_ranges {
            None => self.file_names(&by_name, &mut problems),
            Some(ranges) => self.range_file_names(&by_name, ranges, &mut problems),
        };
        let ranges = self.source_ranges(&by_name, &mut problems);
        if !problems.is_empty() {
            return Err(problems);
        }
        let parameters = checked
            .into_iter()
            .map(|(name, (_, plain))| (name, Some(plain)))
            .collect();
        Ok(RunValues {
            parameters,
            files,
            charts,
            ranges,
            run_ranges,
        })
    }

    /// Each output's and chart's file name with `values`, with a problem for each name that
    /// can't be a file's or that another already has.
    fn file_names(
        &self,
        values: &BTreeMap<&str, (ColumnType, &str)>,
        problems: &mut Vec<String>,
    ) -> (Vec<String>, Vec<String>) {
        let mut files: Vec<String> = Vec::new();
        for (index, output) in self.outputs.iter().enumerate() {
            match output.file.render(values) {
                Ok(file) if files.contains(&file) => problems.push(format!(
                    "output {}: another output already writes {file:?} with these values",
                    index + 1
                )),
                Ok(file) => files.push(file),
                Err(problem) => problems.push(format!("output {}: {problem}", index + 1)),
            }
        }
        let mut charts: Vec<String> = Vec::new();
        for (index, chart) in self.charts.iter().enumerate() {
            match chart.file.render(values) {
                Ok(file) if files.contains(&file) || charts.contains(&file) => {
                    problems.push(format!(
                        "chart {}: another output or chart already writes {file:?} with these \
                         values",
                        index + 1
                    ));
                }
                Ok(file) => charts.push(file),
                Err(problem) => problems.push(format!("chart {}: {problem}", index + 1)),
            }
        }
        (files, charts)
    }

    /// The file names of a partitioned run: each range's, of which the first range's are
    /// returned, with a problem for a name that two ranges would write.
    fn range_file_names(
        &self,
        values: &BTreeMap<&str, (ColumnType, &str)>,
        ranges: &SourceRanges,
        problems: &mut Vec<String>,
    ) -> (Vec<String>, Vec<String>) {
        let mut first = None;
        // Which range wrote each name of an output whose file is one a range.
        let mut written: BTreeMap<String, u64> = BTreeMap::new();
        for range in &ranges.ranges {
            let with_range = self.with_range(values, range);
            let mut found = Vec::new();
            let (files, charts) = self.file_names(&with_range, &mut found);
            if !found.is_empty() {
                problems.extend(found);
                return (files, charts);
            }
            for (index, (output, file)) in self.outputs.iter().zip(&files).enumerate() {
                if !output.is_per_range() {
                    continue;
                }
                if let Some(earlier) = written.insert(file.clone(), range.number) {
                    problems.push(format!(
                        "output {}: ranges {earlier} and {} would both write {file:?}; name the \
                         file by part_from with a format that tells them apart, as \
                         {{part_from:%Y-%m-%d}}",
                        index + 1,
                        range.number
                    ));
                    return (files, charts);
                }
            }
            first.get_or_insert((files, charts));
        }
        first.unwrap_or_default()
    }

    /// `values` with `range`'s bounds as `part_from` and `part_to`.
    fn with_range<'v>(
        &self,
        values: &BTreeMap<&'v str, (ColumnType, &'v str)>,
        range: &'v PartRange,
    ) -> BTreeMap<&'v str, (ColumnType, &'v str)> {
        let mut with_range = values.clone();
        if let Some(partition) = &self.partition {
            let range_type = partition.kind.column_type();
            with_range.insert(PART_FROM, (range_type, range.from.as_str()));
            with_range.insert(PART_TO, (range_type, range.to.as_str()));
        }
        with_range
    }

    /// The run's ranges, for a partitioned run.
    fn run_ranges(
        &self,
        values: &BTreeMap<&str, (ColumnType, &str)>,
        problems: &mut Vec<String>,
    ) -> Option<SourceRanges> {
        let partition = self.partition.as_ref()?;
        match partition.ranges(values).and_then(|ranges| {
            partition
                .whole(values)
                .map(|whole| SourceRanges { ranges, whole })
        }) {
            Ok(ranges) => Some(ranges),
            Err(problem) => {
                problems.push(format!("[report]: partition: {problem}"));
                None
            }
        }
    }

    /// The values of one range of a partitioned run: the run's, with the range's bounds bound
    /// as `:part_from` and `:part_to`, and the file names they make.
    ///
    /// # Errors
    ///
    /// Why a file name can't be made, which [`Plan::values`] has already ruled out for every
    /// range of the run.
    pub fn range_values(&self, values: &RunValues, range: &PartRange) -> Result<RunValues, String> {
        let by_name: BTreeMap<&str, (ColumnType, &str)> = self
            .params
            .iter()
            .filter_map(|param| {
                let value = values.parameters.get(&param.name)?.as_deref()?;
                Some((param.name.as_str(), (param.param_type, value)))
            })
            .collect();
        let mut problems = Vec::new();
        let (files, charts) = self.file_names(&self.with_range(&by_name, range), &mut problems);
        if let Some(problem) = problems.into_iter().next() {
            return Err(problem);
        }
        let mut parameters = values.parameters.clone();
        parameters.set(PART_FROM, Some(range.from.clone()));
        parameters.set(PART_TO, Some(range.to.clone()));
        Ok(RunValues {
            parameters,
            files,
            charts,
            ranges: values.ranges.clone(),
            run_ranges: None,
        })
    }

    /// The sources with a query, as the file lists them.
    fn query_sources(&self) -> Vec<&QuerySource> {
        match &self.logic {
            Logic::Direct(source) => vec![source],
            Logic::Combine(combined) => combined
                .sources
                .iter()
                .filter_map(|source| match source {
                    PlannedSource::Query(source) => Some(source),
                    PlannedSource::Attached(_) => None,
                })
                .collect(),
        }
    }

    /// Each partitioned source's ranges with `values`, with a problem for each source that has
    /// none or too many.
    fn source_ranges(
        &self,
        values: &BTreeMap<&str, (ColumnType, &str)>,
        problems: &mut Vec<String>,
    ) -> BTreeMap<String, SourceRanges> {
        let mut ranges = BTreeMap::new();
        for source in self.query_sources() {
            let Some(split) = source.split.partition() else {
                continue;
            };
            let partition = &split.partition;
            match partition.ranges(values).and_then(|ranges| {
                partition
                    .whole(values)
                    .map(|whole| SourceRanges { ranges, whole })
            }) {
                Ok(planned) => {
                    ranges.insert(source.id.clone(), planned);
                }
                Err(problem) => {
                    problems.push(format!("source {:?}: partition: {problem}", source.id));
                }
            }
        }
        ranges
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
    params: &BTreeMap<&str, ColumnType>,
) -> Result<PlannedSource, KeyedProblem> {
    let data_source = find_data_source(&source.data_source, data_sources)
        .map_err(|problem| (Some("data_source"), problem))?;
    match &source.read {
        SourceRead::Query {
            query,
            limits,
            split,
            columns,
        } => plan_query(
            &source.id,
            data_source,
            QueryRead {
                query,
                limits: limits.or(defaults),
                split,
                columns: columns.as_ref(),
            },
            params,
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
    split: &'a SourceSplit,
    columns: Option<&'a Vec<SourceColumn>>,
}

fn plan_query(
    id: &str,
    data_source: &DataSource,
    read: QueryRead<'_>,
    params: &BTreeMap<&str, ColumnType>,
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
    let split = match read.split {
        SourceSplit::Whole => PlannedSplit::Whole,
        SourceSplit::Lookup(lookup) => plan_lookup(lookup, language, read.query)
            .map(PlannedSplit::Lookup)
            .map_err(|problem| (Some("lookup"), problem))?,
        SourceSplit::Partition(split) => plan_partition(split, language, read.query, params)
            .map(PlannedSplit::Partition)
            .map_err(|problem| (Some("partition"), problem))?,
    };
    Ok(QuerySource {
        id: id.to_owned(),
        data_source: data_source.clone(),
        language,
        query: read.query.to_owned(),
        limits: read.limits,
        split,
        columns: read.columns.cloned(),
    })
}

/// `split` for a query in `language`: its bounds checked against the report's parameters, and
/// the query reading a range's bounds.
fn plan_partition(
    split: &SourcePartition,
    language: QueryLanguage,
    query: &str,
    params: &BTreeMap<&str, ColumnType>,
) -> Result<PlannedSourcePartition, String> {
    let partition = PlannedPartition::plan(split.partition.clone(), params)
        .map_err(|problem| format!("partition: {problem}"))?;
    let reads_range = language
        .parameters(query)
        .iter()
        .any(|parameter| parameter.name == PART_FROM || parameter.name == PART_TO);
    if !reads_range {
        return Err(format!(
            "partition: the query uses neither :{PART_FROM} nor :{PART_TO}, so every range would \
             read the same rows; read a range, as WHERE ts >= :{PART_FROM} AND ts < :{PART_TO}"
        ));
    }
    Ok(PlannedSourcePartition {
        partition,
        max_rows_per_part: split.max_rows_per_part,
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
    check_reads(query, "the combine step")
}

/// Whether a query of the run's DuckDB, the combine step's or a chart's (`what`), is one
/// statement that only reads.
fn check_reads(query: &str, what: &str) -> Result<(), String> {
    let statements = dbrover_sql::split_statements(query, Dialect::DuckDb).len();
    if statements != 1 {
        return Err(format!(
            "the query holds {statements} statements; {what} is one query that only reads"
        ));
    }
    match QueryLanguage::Sql(Dialect::DuckDb).classify(query) {
        Ok(StatementKind::Read) => Ok(()),
        Ok(kind) => Err(format!(
            "the query {}; {what} only reads the sources' tables",
            kind.description()
        )),
        // The parser behind `classify` doesn't read all of DuckDB's SQL (`ASOF JOIN … ON`, for
        // one), so a query it can't parse only has to start as one. The run's DuckDB is the
        // guard that holds: locked down, with its attached files read-only, it refuses what
        // such a query might try beyond reading.
        Err(_) if starts_as_query(query) => Ok(()),
        Err(unclassified) => Err(format!(
            "{unclassified}; {what} is one query that only reads"
        )),
    }
}

fn starts_as_query(query: &str) -> bool {
    dbrover_sql::first_word(query, Dialect::DuckDb)
        .is_some_and(|word| QUERY_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

/// A query the run sends: where it is, its text and language, and the parameters the run binds
/// itself, which aren't the report's: a lookup's, or a range's.
struct Sent<'a> {
    label: String,
    place: Place,
    query: &'a str,
    language: QueryLanguage,
    bound: Vec<&'a str>,
}

/// Each query the run sends, sources first.
fn queries<'a>(
    sources: &'a [PlannedSource],
    combine: Option<&'a PlannedCombine>,
    charts: &'a [Chart],
    partitioned: bool,
) -> Vec<Sent<'a>> {
    let range_bound = || {
        if partitioned {
            vec![PART_FROM, PART_TO]
        } else {
            Vec::new()
        }
    };
    let sources = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| match source {
            PlannedSource::Query(source) => Some(Sent {
                label: format!("source {:?}", source.id),
                place: Place::key(Table::Source(index), "query"),
                query: source.query.as_str(),
                language: source.language,
                bound: [source.split.bound(), range_bound()].concat(),
            }),
            PlannedSource::Attached(_) => None,
        });
    let combine = combine.map(|combine| Sent {
        label: "[combine]".to_owned(),
        place: Place::key(Table::Combine, combine_key(&combine.query)),
        query: combine.query.sql(),
        language: QueryLanguage::Sql(Dialect::DuckDb),
        bound: range_bound(),
    });
    let charts = charts.iter().enumerate().filter_map(|(index, chart)| {
        Some(Sent {
            label: format!("chart {}", index + 1),
            place: Place::key(Table::Chart(index), "query"),
            query: chart.query.as_deref()?,
            language: QueryLanguage::Sql(Dialect::DuckDb),
            bound: Vec::new(),
        })
    });
    sources.chain(combine).chain(charts).collect()
}

/// Whether some query of a partitioned run reads its range, without which every range would
/// write the same rows.
fn check_reads_ranges(sent: &[Sent<'_>], problems: &mut Vec<ReportProblem>) {
    let reads_range = sent.iter().any(|sent| {
        sent.language
            .parameters(sent.query)
            .iter()
            .any(|parameter| parameter.name == PART_FROM || parameter.name == PART_TO)
    });
    if !reads_range {
        problems.push(ReportProblem::new(
            format!(
                "[report]: partition: no query uses :{PART_FROM} or :{PART_TO}, so every range \
                 would write the same rows; read a range in a source's query, as WHERE ts >= \
                 :{PART_FROM} AND ts < :{PART_TO}"
            ),
            Place::key(Table::Report, "partition"),
        ));
    }
}

/// A partitioned run's Parquet outputs each write a file a range for now: one file appended to
/// by every range comes with RS4e.
fn check_appended_parquet(outputs: &[PlannedOutput], problems: &mut Vec<ReportProblem>) {
    for (index, output) in outputs.iter().enumerate() {
        if output.format.written_by_duckdb().is_some() && !output.is_per_range() {
            problems.push(ReportProblem::new(
                format!(
                    "output {}: a partitioned run writes a Parquet file a range; name the file \
                     by its range, as events-{{part_from:%Y-%m}}.parquet",
                    index + 1
                ),
                Place::key(Table::Output(index), "file"),
            ));
        }
    }
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
            if ordered.iter().any(|param| param.name == name) || sent.bound.contains(&name.as_str())
            {
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
    combines: bool,
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
        if format.written_by_duckdb().is_some() {
            check_parquet(output, &label, index, writer_key, combines, problems);
        }
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

/// What a Parquet output can't have: its columns are the combine step's, as DuckDB types
/// them, so it needs a combine step and takes no `columns`.
fn check_parquet(
    output: &Output,
    label: &str,
    index: usize,
    writer_key: &str,
    combines: bool,
    problems: &mut Vec<ReportProblem>,
) {
    if !combines {
        problems.push(ReportProblem::new(
            format!(
                "{label}: a Parquet file is written by the combine step's DuckDB; add a \
                 [combine] step, as query = \"SELECT * FROM <source>\""
            ),
            Place::key(Table::Output(index), writer_key),
        ));
    }
    if output.columns != OutputColumns::All {
        problems.push(ReportProblem::new(
            format!(
                "{label}: a Parquet output writes the combine step's columns as DuckDB types \
                 them; name and cast them in the combine query instead"
            ),
            Place::key(Table::Output(index), "columns"),
        ));
    }
}

fn built_in(
    format: ExportFormat,
    options: &dbrover_export::ExportOptions,
) -> Result<OutputFormat, String> {
    match format {
        ExportFormat::Csv => Ok(OutputFormat::Csv(options.csv.clone())),
        ExportFormat::Xlsx => Ok(OutputFormat::Xlsx(options.xlsx.clone())),
        ExportFormat::Parquet => Ok(OutputFormat::Parquet(options.parquet)),
        other => Err(format!(
            "a report writes CSV, Excel or Parquet files, not {}",
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
            "{id:?} in exports.toml is a template or a program; a report's output is a CSV, \
             Excel or Parquet preset"
        )),
    }
}
