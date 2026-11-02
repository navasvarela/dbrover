//! What a run reports as it goes: the events the worker writes as JSON Lines and the app
//! reads (`docs/reporting.md`, "The worker in RP1"). Shared by both sides, so they can't
//! disagree on a field.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One step of a run, or how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// The report was read and planned, and its source is being opened.
    Started {
        report: String,
        source: String,
        data_source: String,
    },
    /// What the source has returned so far, about once a second.
    Progress {
        rows: u64,
        bytes: u64,
        elapsed_ms: u64,
    },
    /// The source returned its last row.
    SourceDone {
        rows: u64,
        bytes: u64,
        /// From opening the source to its first row; `None` when it returned none.
        first_row_ms: Option<u64>,
        elapsed_ms: u64,
    },
    /// The first rows, as the source returned them.
    Preview(Preview),
    /// A file put in place.
    OutputWritten(WrittenFile),
    Finished {
        elapsed_ms: u64,
    },
    Failed(RunFailure),
    /// **Cancel** stopped the worker. Sent by the app's side, never by the worker.
    Cancelled,
}

impl RunEvent {
    /// Whether the run is over: nothing comes after this event.
    pub fn ends_run(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. } | Self::Failed(_) | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Whether the source returned more rows than these.
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub rows: u64,
    /// The file's size on disk.
    pub size: u64,
    /// One sentence each, such as cells Excel could only hold as text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Why a run stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{step}: {message}")]
pub struct RunFailure {
    pub step: Step,
    pub message: String,
    /// The guardrail it hit, if a guardrail stopped it. Boxed: a run's every step returns
    /// this as its error, and it is rare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Box<LimitHit>>,
}

impl RunFailure {
    pub fn new(step: Step, message: impl Into<String>) -> Self {
        Self {
            step,
            message: message.into(),
            limit: None,
        }
    }

    pub fn at_limit(step: Step, message: impl Into<String>, limit: LimitHit) -> Self {
        Self {
            step,
            message: message.into(),
            limit: Some(Box::new(limit)),
        }
    }
}

/// Where in a run something happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Reading the report, finding what it names and checking the values.
    Report,
    Source {
        id: String,
    },
    Output {
        /// Counted from 1, as the report's problems count them.
        number: usize,
        file: String,
    },
    /// The worker process itself: it couldn't start, ran out of time, or stopped.
    Worker,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Report => f.write_str("the report"),
            Self::Source { id } => write!(f, "source {id:?}"),
            Self::Output { number, file } => write!(f, "output {number} ({file})"),
            Self::Worker => f.write_str("the worker"),
        }
    }
}

/// A guardrail a run stopped at, and where to raise it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitHit {
    /// The key that sets it, such as `max_rows`.
    pub key: String,
    /// Its value, as `1,000,000 rows`.
    pub value: String,
    /// Where the key goes, as `the [[source]] "balances"`.
    pub raise_in: String,
}
