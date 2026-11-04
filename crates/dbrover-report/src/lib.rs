//! Reports: saved, re-runnable recipes of queries on one or more data sources, a DuckDB query
//! that combines their rows, the parameters they take and the files they write, each with a
//! declared schema. The design is in `docs/reporting.md`.
//!
//! A report goes through three steps, each a type:
//!
//! 1. [`Report`]: the file as read ([`read_report`]), on its own, with every problem it has;
//! 2. [`Plan`]: the report against what it names, the data sources and `exports.toml`'s
//!    presets, ready to run but for its parameters' values;
//! 3. [`RunValues`]: the values typed for a run, checked against their types, and the file
//!    names they make.
//!
//! Reading a file and connecting to a data source are left to the store, the engine and the
//! worker, which hand this crate what they read. [`run`] then opens the sources through a
//! [`SourceOpener`] and streams their rows ([`SourceReader`]) into the output files.

mod combine;
mod declared;
mod events;
mod file_name;
mod hints;
mod limits;
mod plan;
mod prql;
mod read;
mod report;
mod request;
mod run;
mod statement;
mod typing;

pub use events::{
    Batch, LimitHit, LimitSetIn, LoadedColumn, Preview, RunEvent, RunFailure, SourceStart, Step,
    WrittenFile,
};
pub use file_name::FileName;
pub use hints::{HintId, UnknownHint};
pub use limits::{
    ByteSize, CombineLimits, CombineLimitsSet, DEFAULT_COMBINE_TIMEOUT, DEFAULT_MAX_BYTES,
    DEFAULT_MAX_ROWS, DEFAULT_MAX_SIZE, DEFAULT_MAX_TEMP_SIZE, DEFAULT_MEMORY_LIMIT,
    DEFAULT_RUN_TIMEOUT, DEFAULT_SOURCE_TIMEOUT, LimitDefaults, OutputLimits, OutputLimitsSet,
    SourceLimits, SourceLimitsSet,
};
pub use plan::{
    AttachedSource, Combined, Logic, OutputFormat, Plan, PlannedCombine, PlannedLookup,
    PlannedOutput, PlannedSource, QuerySource, RunValues, TypedValues,
};
pub use read::read_report;
pub use report::{
    Combine, CombineQuery, Lookup, Output, OutputColumns, OutputWriter, Param, Report, Source,
    SourceColumn, SourceRead,
};
pub use request::RunRequest;
pub use run::{
    PREVIEW_ROWS, RunFolders, RunSummary, SourceError, SourceOpener, SourceReader, SourceStatement,
    grouped, run,
};

#[cfg(test)]
mod combine_tests;
#[cfg(test)]
mod lookup_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod tests;
