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
    /// The memory the worker may use, in bytes, instead of its default: half the machine's,
    /// on Linux, where it is enforced. Nothing in the app sets it; tests do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<u64>,
}
