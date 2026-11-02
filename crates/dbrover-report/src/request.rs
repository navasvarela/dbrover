//! What the app asks `dbrover report-worker` to run: one JSON object on its standard input.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::TypedValues;

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
    /// The memory the worker may use, in bytes, instead of its default: half the machine's,
    /// on Linux, where it is enforced. Nothing in the app sets it; tests do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<u64>,
}
