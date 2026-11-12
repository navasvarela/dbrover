//! Reports: saved, re-runnable recipes of queries on one or more data sources, a DuckDB query
//! that combines their rows, the parameters they take and the files they write, each with a
//! declared schema. The design is in `docs/reporting.md`.
//!
//! A report goes through three steps, each a type:
//!
//! 1. [`Report`]: the file as read ([`read_report`]), on its own, with every problem it has
//!    (each with its place in the file, from [`read_with_places`], for an editor);
//! 2. [`Plan`]: the report against what it names, the data sources and `exports.toml`'s
//!    presets, ready to run but for its parameters' values;
//! 3. [`RunValues`]: the values typed for a run, checked against their types, and the file
//!    names they make.
//!
//! Reading a file and connecting to a data source are left to the store, the engine and the
//! worker, which hand this crate what they read. [`run`] then opens the sources through a
//! [`SourceOpener`] and streams their rows ([`SourceReader`]) into the output files.

mod chart;
mod combine;
mod declared;
mod disk;
mod embedded;
mod events;
mod explain;
mod file_name;
mod format;
mod hints;
mod kept_rows;
mod limits;
mod live;
mod parquet;
mod partition;
mod plan;
mod problem;
mod prql;
mod read;
mod report;
mod request;
mod run;
mod statement;
mod text;
mod typing;

pub use combine::QUEUE_PAGES;
pub use disk::DiskBudget;
pub use embedded::{Embedded, PrqlToken, StepLanguage, embedded_code, prql_tokens};
pub use events::{
    Batch, Busy, CombineCaps, LimitHit, LimitSetIn, LoadedColumn, Machine, PartDone,
    PartitionStart, Parts, Preview, RunEvent, RunFailure, RunPartitionStart, SourceCaps,
    SourcePhases, SourceStart, Step, TimedEvent, WrittenFile, counted,
};
pub use explain::{
    CombineExplanation, Estimate, ExplainedPlan, Explanation, KnownTable, SourceExplained,
    SourceExplainer, SourceExplanation, explain,
};
pub use file_name::FileName;
pub use format::{FormatReportError, FormattedReport, format_report};
pub use hints::{HintId, UnknownHint};
pub use limits::{
    ByteSize, CombineLimits, CombineLimitsSet, DEFAULT_COMBINE_TIMEOUT, DEFAULT_MAX_BYTES,
    DEFAULT_MAX_DISK, DEFAULT_MAX_ROWS, DEFAULT_MAX_SIZE, DEFAULT_MAX_TEMP_SIZE,
    DEFAULT_MEMORY_LIMIT, DEFAULT_RUN_TIMEOUT, DEFAULT_SOURCE_TIMEOUT, DEFAULT_STALL_TIMEOUT,
    LimitDefaults, OutputLimits, OutputLimitsSet, SourceLimits, SourceLimitsSet,
};
pub use partition::{
    CalendarUnit, Every, MAX_PARALLEL, MAX_RANGES, PART_FROM, PART_TO, PartRange, Partition,
    PlannedPartition, RangeBound, RangeKind,
};
pub use plan::{
    AttachedSource, Combined, Logic, OutputFormat, Plan, PlannedCombine, PlannedLookup,
    PlannedOutput, PlannedSource, PlannedSourcePartition, PlannedSplit, QuerySource, RunValues,
    SourceRanges, TypedValues,
};
pub use problem::{Locator, Place, ReportProblem, Table};
pub use read::{read_report, read_with_places};
pub use report::{
    Chart, ChartPicture, Combine, CombineQuery, Lookup, Output, OutputColumns, OutputWriter, Param,
    Report, Source, SourceColumn, SourcePartition, SourceRead, SourceSplit,
};
pub use request::{ExplainRequest, RunRequest};
pub use run::{
    CostCheck, Opening, PREVIEW_ROWS, RunFolders, RunSummary, SourceError, SourceOpener,
    SourceReader, SourceStatement, WholePeriod, grouped, run,
};
pub use text::plan_text;

#[cfg(test)]
mod chart_tests;
#[cfg(test)]
mod combine_tests;
#[cfg(test)]
mod lookup_tests;
#[cfg(test)]
mod partition_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod tests;
