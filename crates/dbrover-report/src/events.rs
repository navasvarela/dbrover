//! What a run reports as it goes: the events the worker writes as JSON Lines and the app
//! reads (`docs/reporting.md`, "The worker in RP1" and "Progress in RP2"). Shared by both
//! sides, so they can't disagree on a field.
//!
//! Each line is a [`TimedEvent`]: the event, and when in the run it was sent, which with each
//! step's own times places the step on the run's timeline (`docs/reports-at-scale.md`, RS1b).

use std::path::PathBuf;

use dbrover_core::BlobId;
use serde::{Deserialize, Serialize};

use crate::{Explanation, Plan};

/// An event as a line carries it: the event, and how long after the worker started it was
/// sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedEvent {
    /// Milliseconds from the worker's start, by its own clock. For an event the app's side
    /// makes (a cancelled run, a worker that died), by the app's clock from starting it.
    #[serde(default)]
    pub at_ms: u64,
    #[serde(flatten)]
    pub event: RunEvent,
}

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
        /// The SQL a combine step written in PRQL compiled to, which is what runs; `None` for a
        /// combine step in SQL, whose SQL is the file's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        combine_sql: Option<String>,
        /// The Git blob id of the report file's text that runs (VC1e): what Git itself calls
        /// that version of the file, for `git log --find-object`. `None` from a worker older
        /// than VC1e.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<BlobId>,
        /// The combine step's limits, for a report that combines; `None` from a worker older
        /// than RS1b too.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        combine_limits: Option<CombineCaps>,
        /// The whole run's time limit; `None` from a worker older than RS1b.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Where the run runs; `None` from a worker older than RS1b.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<Machine>,
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
        /// What the step spent the time since its last progress event on; `None` from a
        /// worker older than RS1d.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        busy: Option<Busy>,
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
        /// Where its time went; `None` from a worker older than RS1b.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phases: Option<SourcePhases>,
        /// The bytes its engine scanned, all its statements together, where the engine says
        /// (ClickHouse, BigQuery, Athena, Trino); `None` elsewhere, and from a worker older
        /// than RS2f.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scanned_bytes: Option<u64>,
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
        /// The time spent waiting on DuckDB for the query's rows, the first page's included;
        /// the rest of `elapsed_ms` went to the outputs. `None` from a worker older than RS1b.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_ms: Option<u64>,
        /// The query's plan as DuckDB's profiler measured it, each operator with its time and
        /// rows, as DuckDB's JSON profile writes it (`dbrover_plan::PlanFormat::DuckDbJson`);
        /// `None` when DuckDB didn't say, and from a worker older than RS2c.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<String>,
    },
    /// What the worker process used, sent once just before the run's end.
    WorkerUsage {
        /// The most memory the worker's process held resident at once, DuckDB's included;
        /// `None` where the system doesn't say (Windows).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak_memory: Option<u64>,
    },
    /// What explaining the report found, for a request to explain it (RS2d), which ends with
    /// [`RunEvent::Finished`] after it. Boxed: it is large beside the other events.
    Explained(Box<Explanation>),
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

    /// The event, sent `at` into the run.
    pub fn at(self, at: std::time::Duration) -> TimedEvent {
        TimedEvent {
            at_ms: u64::try_from(at.as_millis()).unwrap_or(u64::MAX),
            event: self,
        }
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
    /// Its data source's engine, as DbRover names it (`PostgreSQL`); `None` from a worker
    /// older than RS1b.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The caps it runs under; `None` for an attached file, which reads nothing itself, and
    /// from a worker older than RS1b.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<SourceCaps>,
}

/// A source's caps as a run applies them, whoever set them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCaps {
    pub max_rows: u64,
    pub max_bytes: u64,
    pub timeout_ms: u64,
}

/// The combine step's limits as a run applies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombineCaps {
    pub memory_limit: u64,
    pub max_temp_size: u64,
    pub timeout_ms: u64,
}

/// The machine a run runs on, as far as a profile compares runs by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// The processor's cores, as the system offers them to the worker.
    pub cores: u32,
    /// DbRover's version, as `0.31.0`.
    pub dbrover: String,
}

/// What a running step spent a stretch of time on, each in percent of it. The rest is
/// DbRover's own work between them, such as checking a page against the caps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Busy {
    /// Waiting on its engine: a source's database, or DuckDB for the combine step.
    pub waiting_pct: u8,
    /// Appending a source's rows to its table in DuckDB.
    pub loading_pct: u8,
    /// The outputs converting and writing rows.
    pub writing_pct: u8,
}

/// A share smaller than this doesn't name what a step is doing: it is busy with no one thing.
const BUSY_NAMED_PCT: u8 = 20;

impl Busy {
    /// What the step is mostly doing, as `waiting on PostgreSQL (92%)`; `engine` is what it
    /// waits on. `None` when no one thing took a fifth of the time.
    pub fn describe(&self, engine: &str) -> Option<String> {
        let shares = [
            (self.waiting_pct, format!("waiting on {engine}")),
            (self.loading_pct, "loading into DuckDB".to_owned()),
            (self.writing_pct, "writing the files".to_owned()),
        ];
        let (pct, doing) = shares.into_iter().max_by_key(|(pct, _)| *pct)?;
        (pct >= BUSY_NAMED_PCT).then(|| format!("{doing} ({pct}%)"))
    }
}

/// Where a source's time went, in milliseconds. The phases add up to about the source's time;
/// a source with a lookup adds each batch's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePhases {
    /// Connecting to its data source.
    pub connect_ms: u64,
    /// From sending the statement to its first row: the engine's own work, mostly.
    pub server_ms: u64,
    /// Waiting for the rest of its rows, once the first came.
    pub transfer_ms: u64,
    /// Its rows appended to its table in DuckDB, and a lookup's values read from there; zero
    /// for a report without a combine step, whose outputs' time is their own.
    pub load_ms: u64,
    /// Typing its table's columns once every row is in, which rewrites the table.
    pub type_ms: u64,
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
    /// The bytes of its values the source returned, as text, which says what dropping the
    /// column from the source's query would save; `None` from a worker older than RS2c.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
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

    /// Says where the limit it stopped at was set, from `plan`, the plan it ran, unless the
    /// limit already says.
    pub fn place_limit(&mut self, plan: &Plan) {
        if let Some(limit) = &mut self.limit
            && limit.set_in.is_none()
        {
            limit.set_in = plan.limit_set_in(&self.step, &limit.key);
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
    /// A `[[chart]]`: reading its rows, drawing it and writing its file
    /// (`docs/charts-as-code.md`, CH3).
    Chart {
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
            Self::Chart { number, file } => write!(f, "chart {number} ({file})"),
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
    /// Who set the value the run stopped at; `None` for a limit that is a format's own (an
    /// Excel sheet's rows), and in events from before RP4h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_in: Option<LimitSetIn>,
}

/// Where the value of a limit a run stopped at came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSetIn {
    /// The report's file sets the key.
    File,
    /// The file leaves it out, so the default in Settings › Reports holds: the one set there,
    /// or the built-in one the page shows.
    Settings,
}

/// The key a run stops at past an Excel sheet's rows, which is Excel's limit, not DbRover's.
const EXCEL_ROWS_KEY: &str = "continue_on_new_sheet";

/// The key a run stops at past the rows a chart's kind draws, which `limit` cuts to the first
/// ones; no default in Settings raises it.
pub(crate) const CHART_ROWS_KEY: &str = "limit";

/// The key a run stops at past a chart's colours, one a series; nothing raises it.
pub(crate) const CHART_SERIES_KEY: &str = "series";

impl LimitHit {
    /// What to change to let the run go further, as the report's tab and `dbrover report run`
    /// say it.
    pub fn advice(&self) -> String {
        let Self {
            key,
            value,
            raise_in,
            set_in,
        } = self;
        if key == CHART_ROWS_KEY {
            return format!(
                "It stopped at {value}, the most a chart of its kind draws. Group the rows in \
                 the query, or set limit in {raise_in} of the report's file to draw the first \
                 ones."
            );
        }
        if key == CHART_SERIES_KEY {
            return format!(
                "It stopped at {value}, one a colour. Group or filter the series' values in \
                 the query, so {raise_in} has fewer."
            );
        }
        if key == EXCEL_ROWS_KEY {
            return format!(
                "It stopped at {value}. To go on in a new sheet, set {key} = true in {raise_in} \
                 of the report's file."
            );
        }
        match set_in {
            Some(LimitSetIn::File) => format!(
                "It stopped at {key} = {value}, set in {raise_in} of the report's file. To allow \
                 more, raise it there."
            ),
            Some(LimitSetIn::Settings) => format!(
                "It stopped at {key} = {value}, the default in Settings › Reports, since the \
                 report's file doesn't set it. To allow more, raise it there for every report, \
                 or set {key} in {raise_in} of the report's file for this one."
            ),
            None => format!(
                "It stopped at {key} = {value}. To allow more, raise {key} in {raise_in} of the \
                 report's file, or its default for every report in Settings › Reports."
            ),
        }
    }
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
                    query_ms: None,
                    plan: None,
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

    /// A limit's `set_in` came with RP4h: older lines have none, and a limit without one
    /// writes none, so older readers see the line they knew.
    #[test]
    fn reads_a_limit_from_before_it_said_who_set_it() {
        let line = r#"{"key":"max_rows","value":"10 rows","raise_in":"[combine]"}"#;
        let limit: LimitHit = serde_json::from_str(line).unwrap();
        assert_eq!(limit.set_in, None);
        assert_eq!(serde_json::to_string(&limit).unwrap(), line);
        let settings = LimitHit {
            set_in: Some(LimitSetIn::Settings),
            ..limit
        };
        assert!(
            serde_json::to_string(&settings)
                .unwrap()
                .ends_with(r#""set_in":"settings"}"#)
        );
    }
}
