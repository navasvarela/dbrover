//! A report file as read, before anything it names is looked up.

use std::num::NonZeroUsize;
use std::time::Duration;

use dbrover_export::{ColumnType, CustomFormatId, Preset, SchemaColumn};

use crate::{CombineLimits, FileName, OutputLimits, SourceLimits};

/// A report file, checked on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub name: String,
    pub description: Option<String>,
    /// As the file lists them.
    pub params: Vec<Param>,
    /// How long the whole run may take before the app stops it.
    pub timeout: Duration,
    /// As the file lists them: one without `combine`, one or more with it.
    pub sources: Vec<Source>,
    /// The query that combines the sources' tables into the rows the outputs write.
    pub combine: Option<Combine>,
    pub outputs: Vec<Output>,
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
        limits: SourceLimits,
        /// The earlier source whose values its query reads by.
        lookup: Option<Lookup>,
        /// The columns its table has, when the file declares them; otherwise the result's.
        columns: Option<Vec<SourceColumn>>,
    },
    /// A DuckDB file, attached read-only for the combine step to read in place.
    Attach,
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
    pub query: String,
    pub limits: CombineLimits,
}

/// A file a run writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub writer: OutputWriter,
    pub file: FileName,
    pub columns: OutputColumns,
    pub limits: OutputLimits,
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
