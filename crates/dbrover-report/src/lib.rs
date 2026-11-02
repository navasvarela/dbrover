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
//! Nothing here reads a file or connects to anything: the store, the engine and the worker do
//! that, and hand this crate what they read.

mod file_name;
mod limits;
mod plan;
mod read;
mod report;

pub use file_name::FileName;
pub use limits::{
    ByteSize, DEFAULT_MAX_BYTES, DEFAULT_MAX_ROWS, DEFAULT_MAX_SIZE, DEFAULT_RUN_TIMEOUT,
    DEFAULT_SOURCE_TIMEOUT, OutputLimits, SourceLimits,
};
pub use plan::{OutputFormat, Plan, PlannedOutput, PlannedSource, RunValues, TypedValues};
pub use read::read_report;
pub use report::{Output, OutputColumns, OutputWriter, Param, Report, Source};

#[cfg(test)]
mod tests;
