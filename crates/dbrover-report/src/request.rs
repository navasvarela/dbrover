//! What the app asks `dbrover report-worker` to run: one JSON object on its standard input.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{KnownTable, LimitDefaults, TypedValues};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    /// The report file, read again by the worker, so it runs what is saved.
    pub report: PathBuf,
    /// The parameters' values, as typed.
    pub values: TypedValues,
    /// The folder the files go in, made if it doesn't exist.
    pub folder: PathBuf,
    /// A folder of this run's own inside `folder`, where files are written until they are
    /// all complete. The run removes it; the app's side removes it again after a worker it had
    /// to kill, which couldn't.
    pub staging: PathBuf,
    /// The run's work folder (`docs/reports-at-scale.md`, RS3b), which holds its DuckDB's file,
    /// what DuckDB spills and the run's manifest: made by the worker and removed when the run
    /// ends, and removed again by the app's side after a worker it had to kill, unless it is
    /// kept for the run to resume (RS5c).
    pub work: PathBuf,
    /// The limits where the report's file leaves one out, as Settings › Reports has them, so
    /// the worker reads no settings of its own and runs what the tab showed.
    #[serde(default)]
    pub defaults: LimitDefaults,
    /// The memory the worker may use, in bytes, instead of its default: half the machine's,
    /// on Linux, where it is enforced. Nothing in the app sets it; tests do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<u64>,
    /// Explain the report rather than run it (RS2d): the worker answers with one
    /// [`RunEvent::Explained`](crate::RunEvent::Explained) and writes no file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<ExplainRequest>,
    /// Carry on from the run kept in `work` (RS5d): the worker checks its manifest against
    /// this run, and the run skips what it finished.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
}

/// What explaining a report needs beside the report: what the app's history knows of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainRequest {
    /// Each source's table as the last finished run loaded it, for the combine step's plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<KnownTable>,
}
