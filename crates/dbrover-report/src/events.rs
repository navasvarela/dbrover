//! What a run reports as it goes: the events the worker writes as JSON Lines and the app
//! reads (`docs/reporting.md`, "The worker in RP1" and "Progress in RP2"). Shared by both
//! sides, so they can't disagree on a field.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One step of a run, or how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// The report was read and planned, and its sources are being opened.
    Started {
        report: String,
        /// As the file lists them.
        sources: Vec<SourceStart>,
        /// Whether a combine step makes the rows the outputs write.
        combine: bool,
    },
    /// What a step has returned so far, about once a second.
    Progress {
        step: Step,
        rows: u64,
        bytes: u64,
        elapsed_ms: u64,
        /// For a source whose lookup runs in batches, the one it is reading.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<Batch>,
    },
    /// A source returned its last row, and was loaded into DuckDB if the report combines.
    SourceDone {
        id: String,
        rows: u64,
        bytes: u64,
        /// From opening the source to its first row; `None` when it returned none.
        first_row_ms: Option<u64>,
        elapsed_ms: u64,
        /// The table it was loaded as, for the combine step; empty when the report writes
        /// its rows as they come.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        table: Vec<LoadedColumn>,
        /// One sentence each, such as a column too precise for DuckDB's decimals.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
        /// How many statements a source with a lookup sent, one a batch of its values.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batches: Option<u64>,
    },
    /// A DuckDB file was attached for the combine step to read in place.
    SourceAttached {
        id: String,
        path: PathBuf,
    },
    /// The combine step returned its last row, written to every output.
    CombineDone {
        rows: u64,
        bytes: u64,
        /// From starting the query to its first row; `None` when it returned none.
        first_row_ms: Option<u64>,
        elapsed_ms: u64,
        /// The most memory the run's DuckDB held at once, loading the sources included, which
        /// `memory_limit` bounds; `None` when DuckDB didn't say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak_memory: Option<u64>,
        /// The most the run's DuckDB held spilled to disk at once, which `max_temp_size`
        /// bounds; `None` when DuckDB didn't say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak_temp_size: Option<u64>,
    },
    /// The first rows the outputs were given.
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

/// A source as a run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStart {
    pub id: String,
    /// Its data source's name.
    pub data_source: String,
    pub attached: bool,
    /// The source whose values its lookup reads, which it waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
}

/// A column of a source's table in DuckDB, and the type it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedColumn {
    pub name: String,
    /// As DuckDB names it, as `DECIMAL(38,18)`.
    pub type_name: String,
    /// Whether the report file declares the column's type, rather than the run typing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declared: bool,
}

/// Which of a lookup's batches a source is reading, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub number: u64,
    pub of: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Whether the outputs were given more rows than these.
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub rows: u64,
    /// The file's size on disk.
    pub size: u64,
    /// The time spent in its writer: converting its rows, writing and completing the file.
    /// Rows are written as they come, so this overlaps the time of the step they come from.
    #[serde(default)]
    pub elapsed_ms: u64,
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
    /// The combine step's query, and the DuckDB it runs in.
    Combine,
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
            Self::Combine => f.write_str("the combine step"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker and an app of one build always agree, but RP6's command line may read events a
    /// run of an older DbRover wrote.
    #[test]
    fn reads_events_from_before_the_metrics() {
        let cases = [
            (
                r#"{"event":"combine_done","rows":3,"bytes":30,"first_row_ms":1,"elapsed_ms":5}"#,
                RunEvent::CombineDone {
                    rows: 3,
                    bytes: 30,
                    first_row_ms: Some(1),
                    elapsed_ms: 5,
                    peak_memory: None,
                    peak_temp_size: None,
                },
            ),
            (
                r#"{"event":"output_written","path":"tb.csv","rows":3,"size":40}"#,
                RunEvent::OutputWritten(WrittenFile {
                    path: PathBuf::from("tb.csv"),
                    rows: 3,
                    size: 40,
                    elapsed_ms: 0,
                    warnings: Vec::new(),
                }),
            ),
        ];
        for (line, event) in cases {
            assert_eq!(
                serde_json::from_str::<RunEvent>(line).unwrap(),
                event,
                "{line}"
            );
        }
    }
}
