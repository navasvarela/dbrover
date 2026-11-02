//! Reports: saved, re-runnable recipes of a query, the parameters it takes and the files it
//! writes, each with a declared schema. The design is in `docs/reporting.md`.
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
//! worker, which hand this crate what they read. [`run`] then streams a source's rows
//! ([`SourceReader`]) into the output files.

mod events;
mod file_name;
mod limits;
mod plan;
mod read;
mod report;
mod run;

pub use events::{LimitHit, Preview, RunEvent, RunFailure, Step, WrittenFile};
pub use file_name::FileName;
pub use limits::{
    ByteSize, DEFAULT_MAX_BYTES, DEFAULT_MAX_ROWS, DEFAULT_MAX_SIZE, DEFAULT_RUN_TIMEOUT,
    DEFAULT_SOURCE_TIMEOUT, OutputLimits, SourceLimits,
};
pub use plan::{OutputFormat, Plan, PlannedOutput, PlannedSource, RunValues, TypedValues};
pub use read::read_report;
pub use report::{Output, OutputColumns, OutputWriter, Param, Report, Source};
pub use run::{PREVIEW_ROWS, RunSummary, SourceError, SourceReader, run};

#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;
