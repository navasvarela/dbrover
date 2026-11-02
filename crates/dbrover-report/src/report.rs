//! A report file as read, before anything it names is looked up.

use std::time::Duration;

use dbrover_export::{ColumnType, CustomFormatId, Preset, SchemaColumn};

use crate::{FileName, OutputLimits, SourceLimits};

/// A report file, checked on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub name: String,
    pub description: Option<String>,
    /// As the file lists them.
    pub params: Vec<Param>,
    /// How long the whole run may take before the app stops it.
    pub timeout: Duration,
    /// The one source RP1 reads; RP2 makes this a list.
    pub source: Source,
    pub outputs: Vec<Output>,
}

/// A parameter the report's queries and file names take, and its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub param_type: ColumnType,
}

/// A query on one data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// What the run's progress and errors call it.
    pub id: String,
    /// A data source's name, or its id when two share the name.
    pub data_source: String,
    pub query: String,
    pub limits: SourceLimits,
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
