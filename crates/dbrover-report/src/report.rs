//! A report file as read, before anything it names is looked up.

use std::num::NonZeroUsize;
use std::time::Duration;

use dbrover_chart::ChartSpec;
use dbrover_export::{ColumnType, CustomFormatId, Preset, SchemaColumn};

use crate::{
    ByteSize, CombineLimitsSet, FileName, HintId, OutputLimitsSet, Partition, SourceLimitsSet,
};

/// A report file, checked on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub name: String,
    pub description: Option<String>,
    /// As the file lists them.
    pub params: Vec<Param>,
    /// How long the whole run may take before the app stops it, when the file says.
    pub timeout: Option<Duration>,
    /// The most disk the run's work folder and staging folder may hold together, when the file
    /// says (`docs/reports-at-scale.md`, RS3b).
    pub max_disk: Option<ByteSize>,
    /// How long the run may go without progress before the app stops it as stuck, when the
    /// file says (RS3e).
    pub stall_timeout: Option<Duration>,
    /// The hints its runs don't give, for a report that, say, reads more than it writes on
    /// purpose.
    pub quiet_hints: Vec<HintId>,
    /// The ranges the whole run goes through one at a time, when the file says (RS4d).
    pub partition: Option<Partition>,
    /// As the file lists them: one without `combine`, one or more with it.
    pub sources: Vec<Source>,
    /// The query that combines the sources' tables into the rows the outputs write.
    pub combine: Option<Combine>,
    pub outputs: Vec<Output>,
    /// The charts its runs draw, as the file lists them (`docs/charts-as-code.md`, CH3).
    pub charts: Vec<Chart>,
}

/// A parameter the report's queries and file names take, and its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub param_type: ColumnType,
}

/// What a report reads from one data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// What the run's progress and errors call it, and the name of its table, or of its
    /// attached database, in the combine step.
    pub id: String,
    /// A data source's name, or its id when two share the name.
    pub data_source: String,
    pub read: SourceRead,
}

/// How a source is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRead {
    /// Its query's rows, under its limits.
    Query {
        query: String,
        limits: SourceLimitsSet,
        /// Whether its query is sent once, a batch of a lookup's values at a time, or a range
        /// at a time.
        split: SourceSplit,
        /// The columns its table has, when the file declares them; otherwise the result's.
        columns: Option<Vec<SourceColumn>>,
    },
    /// A DuckDB file, attached read-only for the combine step to read in place.
    Attach,
}

/// How a source's query becomes the statements it sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSplit {
    /// One statement.
    Whole,
    /// A statement a batch of an earlier source's values.
    Lookup(Lookup),
    /// A statement a range (`docs/reports-at-scale.md`, RS4b).
    Partition(SourcePartition),
}

impl SourceSplit {
    pub fn lookup(&self) -> Option<&Lookup> {
        match self {
            Self::Lookup(lookup) => Some(lookup),
            Self::Whole | Self::Partition(_) => None,
        }
    }
}

/// A source read in ranges, and the most rows each range may return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePartition {
    pub partition: Partition,
    /// `max_rows_per_part`: no default, since only the report knows its data's ranges.
    pub max_rows_per_part: Option<u64>,
}

/// A parameter bound to the distinct values of an earlier source's column
/// (`docs/reporting.md`, "Lookups").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    /// The source whose loaded table the values are read from.
    pub from: String,
    pub column: String,
    /// The parameter the query uses for the values, `asset_ids` for `:asset_ids`.
    pub parameter: String,
    /// At most this many values a statement, when the file lowers the engine's own.
    pub batch_size: Option<NonZeroUsize>,
}

/// A column a source declares: its table's column `name`, where its value is, its type, and
/// the value a missing or `null` one gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceColumn {
    pub name: String,
    /// A result's column by name, or a document's field by its path (`metadata.chain`).
    pub from: String,
    pub column_type: ColumnType,
    /// In the type's plain form, as checked when the file was read.
    pub default: Option<String>,
}

/// The combine step: a DuckDB query over the sources, and the limits of the DuckDB it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combine {
    pub query: CombineQuery,
    pub limits: CombineLimitsSet,
}

/// The combine step's query, as the file writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CombineQuery {
    /// `query`: DuckDB's SQL.
    Sql(String),
    /// `prql`, and the SQL it compiled to when the file was read, which is what runs.
    Prql { prql: String, compiled: String },
}

impl CombineQuery {
    /// The SQL the run sends.
    pub fn sql(&self) -> &str {
        match self {
            Self::Sql(sql) | Self::Prql { compiled: sql, .. } => sql,
        }
    }

    /// The SQL a PRQL query compiled to, which nothing but the run shows otherwise; `None` for
    /// SQL, which is the file's own.
    pub fn compiled(&self) -> Option<&str> {
        match self {
            Self::Sql(_) => None,
            Self::Prql { compiled, .. } => Some(compiled),
        }
    }
}

/// A file a run writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub writer: OutputWriter,
    pub file: FileName,
    pub columns: OutputColumns,
    pub limits: OutputLimitsSet,
    /// Whether rows past an Excel sheet's limit go on to a new sheet, rather than fail.
    pub continue_on_new_sheet: bool,
}

/// What writes an output's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputWriter {
    /// A built-in format with options of the output's own.
    Format(Preset),
    /// A preset of `exports.toml`, looked up when the report is planned.
    Preset(CustomFormatId),
}

/// Which columns an output writes.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputColumns {
    /// Every column of the result, as it comes.
    All,
    Declared(Vec<SchemaColumn>),
    /// The first output's (`columns = "same"`).
    Same,
}

/// A chart a run draws of its rows, written as a picture file beside the outputs'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chart {
    /// The chart's keys, as a block above a statement holds them.
    pub spec: ChartSpec,
    pub file: FileName,
    pub picture: ChartPicture,
    /// A DuckDB query over the sources' tables to draw instead of the result, run after the
    /// combine step; `None` draws the result.
    pub query: Option<String>,
    /// In points.
    pub width: u32,
    pub height: u32,
}

/// The picture a chart's file holds, from its name's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartPicture {
    Svg,
    /// Drawn from the SVG at this many pixels a point: 1, 2 or 3.
    Png {
        scale: u8,
    },
}
