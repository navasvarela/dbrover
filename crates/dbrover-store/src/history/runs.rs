//! Report runs: what each run of a report measured, in the history's `report_runs` table
//! (`docs/reporting.md`, "Run history"). The app records a run once it ends, however it ends,
//! and only the newest runs of each report are kept.
//!
//! A run's profile (`docs/reports-at-scale.md`, RS1b) is its steps, each with when it started
//! in the run, its phases and its limits, kept in `steps` as RP4's numbers are, and the run's
//! own numbers in `profile`. Every field RS1b added is optional, so a run kept before it reads
//! as one that measured less, and a DbRover from before it reads a newer run's steps.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

use dbrover_core::BlobId;
use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{History, HistoryError, format_timestamp};
use crate::ReportName;

/// How many runs of each report are kept unless Settings says otherwise: enough to see a
/// trend over months of monthly runs, at a few kilobytes each.
pub const DEFAULT_RUNS_KEPT: NonZeroU32 = match NonZeroU32::new(20) {
    Some(kept) => kept,
    None => panic!("the runs kept must not be zero"),
};

/// A kept run's id: its row in `report_runs`, which `dbrover report compare` names it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub i64);

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for RunId {
    type Err = std::num::ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// One run of a report, as it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRun {
    /// Its id once the history keeps it; `None` for a run being recorded.
    pub id: Option<RunId>,
    pub report: ReportName,
    pub started_at: Timestamp,
    /// From asking for the run to its end.
    pub elapsed: Duration,
    pub outcome: RunOutcome,
    /// The parameters' values, as typed.
    pub params: BTreeMap<String, String>,
    /// Each step that finished, in the order it finished.
    pub steps: Vec<StepMetrics>,
    pub origin: RunOrigin,
    /// The Git blob id of the report file's text that ran: `git log --find-object` with it
    /// lists the commits that had exactly that file. `None` for runs from before VC1e.
    pub version: Option<BlobId>,
    /// What the run measured of itself, beside its steps.
    pub profile: RunProfile,
}

/// What a run measured of itself, as against each of its steps: where it ran, its time limit
/// and the worker's memory. Empty for a run kept before RS1b.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunProfile {
    /// The most memory the worker's process held resident at once; `None` where the system
    /// doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_memory: Option<u64>,
    /// The run's time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// The processor's cores the worker was offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
    /// The DbRover that ran it, as `0.31.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbrover: Option<String>,
}

/// The profile as `report_runs.profile` holds it: the run's numbers, and the format they are
/// written in, for a later DbRover that changes it.
#[derive(Serialize, Deserialize)]
struct StoredProfile {
    format: u32,
    #[serde(flatten)]
    profile: RunProfile,
}

/// The format of [`StoredProfile`] this DbRover writes.
const PROFILE_FORMAT: u32 = 1;

/// What started a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunOrigin {
    /// The report's tab. Also every run kept before RP6g, which only the app kept then, as far
    /// as the history knows.
    #[default]
    App,
    /// `dbrover report run`.
    Command,
    /// An MCP client, through `dbrover mcp`.
    Mcp,
}

impl RunOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Command => "command",
            Self::Mcp => "mcp",
        }
    }

    /// `None`, and a value a newer DbRover wrote, read as the app's.
    fn from_stored(origin: Option<&str>) -> Self {
        match origin {
            Some("command") => Self::Command,
            Some("mcp") => Self::Mcp,
            _ => Self::App,
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Finished,
    Failed(RunError),
    Cancelled,
}

/// Where a run failed and why, as its error said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunError {
    /// The step, as the error named it: `source "tx"`, `the combine step`.
    pub step: String,
    pub message: String,
}

impl RunOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_stored(outcome: &str, failure: Option<&str>) -> Option<Self> {
        match outcome {
            "finished" => Some(Self::Finished),
            "cancelled" => Some(Self::Cancelled),
            "failed" => serde_json::from_str(failure?).ok().map(Self::Failed),
            _ => None,
        }
    }
}

/// What one step of a run measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepMetrics {
    Source(SourceMetrics),
    /// A DuckDB file attached for the combine step, which reads nothing of its own.
    Attached {
        id: String,
        data_source: String,
    },
    Combine(CombineMetrics),
    Output(OutputMetrics),
}

/// A source, copied into the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMetrics {
    pub id: String,
    pub data_source: String,
    pub rows: u64,
    pub bytes: u64,
    /// `None` when it returned no row.
    pub first_row_ms: Option<u64>,
    pub elapsed_ms: u64,
    /// For a source with a lookup, the statements it sent, one a batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batches: Option<u64>,
    /// When it was opened, from the run's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// Its data source's engine, as `PostgreSQL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The source its lookup reads, which it waited for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
    /// Where its time went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<SourcePhases>,
    /// The caps it ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<SourceCaps>,
    /// The table it was loaded as for the combine step, which explaining the report plans the
    /// combine over (RS2d); empty when the report has no combine step, and for a run kept
    /// before RS2c.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<KeptColumn>,
    /// The bytes its engine scanned, where the engine says (RS2f): what a warehouse bills
    /// by, and the funnel's first bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_bytes: Option<u64>,
}

/// A column of a source's table, as a run loaded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptColumn {
    pub name: String,
    /// As DuckDB names it, as `DECIMAL(18,2)`.
    pub type_name: String,
    /// The bytes of its values the source returned, as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// Where a source's time went, in milliseconds: see `dbrover_report::SourcePhases`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePhases {
    pub connect_ms: u64,
    /// From sending its statement to its first row.
    pub server_ms: u64,
    /// Waiting for the rest of its rows.
    pub transfer_ms: u64,
    /// Appending to its table in DuckDB.
    pub load_ms: u64,
    /// Typing its table's columns.
    pub type_ms: u64,
}

/// A source's caps, as the run applied them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCaps {
    pub max_rows: u64,
    pub max_bytes: u64,
    pub timeout_ms: u64,
}

/// The combine step's limits, as the run applied them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombineCaps {
    pub memory_limit: u64,
    pub max_temp_size: u64,
    pub timeout_ms: u64,
}

/// The combine step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombineMetrics {
    pub rows: u64,
    pub bytes: u64,
    pub first_row_ms: Option<u64>,
    pub elapsed_ms: u64,
    /// The most memory the run's DuckDB held at once, when DuckDB said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_memory: Option<u64>,
    /// The most it held spilled to disk at once, when DuckDB said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_temp_size: Option<u64>,
    /// When its query started, from the run's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// Waiting on DuckDB for the query's rows; the rest of its time went to the outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_ms: Option<u64>,
    /// The limits it ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<CombineCaps>,
    /// Its query's plan, as DuckDB's profiler measured it, in DuckDB's JSON profile
    /// (`dbrover_plan::PlanFormat::DuckDbJson`); `None` for a run kept before RS2c.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// A file the run wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMetrics {
    /// Counted from 1, in the file's order.
    pub number: usize,
    /// The file's name, which a run's parameters can change.
    pub file: String,
    pub rows: u64,
    /// On disk.
    pub size: u64,
    /// In its writer, overlapping the time of the step its rows came from.
    pub elapsed_ms: u64,
    /// When the step its rows came from started, which is when the file was started, from
    /// the run's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// When the file was put in place, from the run's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
}

impl History {
    /// Adds `run`, then deletes all but the newest `kept` runs of its report, in one
    /// transaction.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if the run can't be written; nothing is.
    pub fn record_run(&self, run: &ReportRun, kept: NonZeroU32) -> Result<(), HistoryError> {
        let failure = match &run.outcome {
            RunOutcome::Failed(error) => Some(to_json(error)?),
            RunOutcome::Finished | RunOutcome::Cancelled => None,
        };
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO report_runs
                 (report, started_at, elapsed_ms, outcome, params, steps, failure, origin, version,
                  profile)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run.report.as_str(),
                format_timestamp(run.started_at),
                i64::try_from(run.elapsed.as_millis()).unwrap_or(i64::MAX),
                run.outcome.as_str(),
                to_json(&run.params)?,
                to_json(&run.steps)?,
                failure,
                run.origin.as_str(),
                run.version.map(|version| version.to_string()),
                to_json(&StoredProfile {
                    format: PROFILE_FORMAT,
                    profile: run.profile.clone(),
                })?,
            ],
        )?;
        delete_older_runs(&transaction, &run.report, kept)?;
        transaction.commit()?;
        Ok(())
    }

    /// The newest `limit` runs of `report`, newest first. Rows that don't parse (from a newer
    /// DbRover, or edited by hand) are skipped.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if the query fails.
    pub fn runs(&self, report: &ReportName, limit: usize) -> Result<Vec<ReportRun>, HistoryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT started_at, elapsed_ms, outcome, params, steps, failure, origin, version, profile,
                    id
             FROM report_runs
             WHERE report = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let stored = statement
            .query_map(params![report.as_str(), limit], StoredRun::read)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(stored
            .into_iter()
            .filter_map(|stored| stored.into_run(report.clone()))
            .collect())
    }

    /// Deletes every run of `report`. Returns how many went.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if they can't be deleted.
    pub fn clear_runs(&self, report: &ReportName) -> Result<u64, HistoryError> {
        let deleted = self.connection.execute(
            "DELETE FROM report_runs WHERE report = ?1",
            [report.as_str()],
        )?;
        Ok(deleted as u64)
    }

    /// Moves `from`'s runs to `to`, a report renamed, in one transaction. Runs already kept
    /// under `to`, from an older report of that name, are deleted first, so the two reports'
    /// runs don't mix. Returns how many moved.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if they can't be moved; nothing is.
    pub fn rename_runs(&self, from: &ReportName, to: &ReportName) -> Result<u64, HistoryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM report_runs WHERE report = ?1", [to.as_str()])?;
        let moved = transaction.execute(
            "UPDATE report_runs SET report = ?2 WHERE report = ?1",
            [from.as_str(), to.as_str()],
        )?;
        transaction.commit()?;
        Ok(moved as u64)
    }
}

/// Deletes the runs of `report` past its newest `kept`: everything from the first one past
/// them, on `report_runs_by_report`.
fn delete_older_runs(
    connection: &rusqlite::Connection,
    report: &ReportName,
    kept: NonZeroU32,
) -> rusqlite::Result<()> {
    let first_gone: Option<(String, i64)> = connection
        .prepare_cached(
            "SELECT started_at, id FROM report_runs WHERE report = ?1
             ORDER BY started_at DESC, id DESC LIMIT 1 OFFSET ?2",
        )?
        .query_row(params![report.as_str(), kept.get()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    if let Some((started_at, id)) = first_gone {
        connection.execute(
            "DELETE FROM report_runs WHERE report = ?1
                 AND (started_at < ?2 OR (started_at = ?2 AND id <= ?3))",
            params![report.as_str(), started_at, id],
        )?;
    }
    Ok(())
}

fn to_json(value: &impl Serialize) -> Result<String, HistoryError> {
    serde_json::to_string(value).map_err(HistoryError::Encode)
}

/// A `report_runs` row as stored, before parsing.
struct StoredRun {
    started_at: String,
    elapsed_ms: i64,
    outcome: String,
    params: String,
    steps: String,
    failure: Option<String>,
    origin: Option<String>,
    version: Option<String>,
    profile: Option<String>,
    id: i64,
}

impl StoredRun {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            started_at: row.get(0)?,
            elapsed_ms: row.get(1)?,
            outcome: row.get(2)?,
            params: row.get(3)?,
            steps: row.get(4)?,
            failure: row.get(5)?,
            origin: row.get(6)?,
            version: row.get(7)?,
            profile: row.get(8)?,
            id: row.get(9)?,
        })
    }

    fn into_run(self, report: ReportName) -> Option<ReportRun> {
        Some(ReportRun {
            id: Some(RunId(self.id)),
            report,
            started_at: self.started_at.parse().ok()?,
            elapsed: Duration::from_millis(u64::try_from(self.elapsed_ms).ok()?),
            outcome: RunOutcome::from_stored(&self.outcome, self.failure.as_deref())?,
            params: serde_json::from_str(&self.params).ok()?,
            steps: serde_json::from_str(&self.steps).ok()?,
            origin: RunOrigin::from_stored(self.origin.as_deref()),
            // An id that doesn't parse (edited by hand) is shown as no id, not as no run.
            version: self.version.and_then(|version| version.parse().ok()),
            // Likewise a profile: the run's steps still say most of what it measured.
            profile: self
                .profile
                .and_then(|profile| serde_json::from_str::<StoredProfile>(&profile).ok())
                .map(|stored| stored.profile)
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::DataSourceId;

    use super::*;
    use crate::{HistoryEntry, HistoryFile, HistoryOutcome, HistoryQuery, HistorySource};

    fn name(name: &str) -> ReportName {
        ReportName::new(name).unwrap()
    }

    /// A run of `report` started `minute` minutes into October 2026.
    fn run(report: &str, minute: i64, outcome: RunOutcome) -> ReportRun {
        ReportRun {
            id: None,
            report: name(report),
            started_at: Timestamp::from_second(1_790_812_800 + minute * 60).unwrap(),
            elapsed: Duration::from_millis(4_250),
            outcome,
            params: [("since".to_owned(), "2026-10-01".to_owned())]
                .into_iter()
                .collect(),
            steps: vec![
                StepMetrics::Source(SourceMetrics {
                    id: "tx".to_owned(),
                    data_source: "ledger".to_owned(),
                    rows: 1_100_000,
                    bytes: 88_000_000,
                    first_row_ms: Some(40),
                    elapsed_ms: 3_100,
                    batches: None,
                    started_ms: Some(12),
                    engine: Some("PostgreSQL".to_owned()),
                    phases: Some(SourcePhases {
                        connect_ms: 30,
                        server_ms: 40,
                        transfer_ms: 2_000,
                        load_ms: 900,
                        type_ms: 130,
                    }),
                    limits: Some(SourceCaps {
                        max_rows: 1_000_000,
                        max_bytes: 1 << 30,
                        timeout_ms: 600_000,
                    }),
                    waits_for: None,
                    columns: Vec::new(),
                    scanned_bytes: None,
                }),
                StepMetrics::Attached {
                    id: "prices".to_owned(),
                    data_source: "prices-duckdb".to_owned(),
                },
                StepMetrics::Combine(CombineMetrics {
                    rows: 40,
                    bytes: 2_000,
                    first_row_ms: Some(900),
                    elapsed_ms: 1_000,
                    peak_memory: Some(2_400_000_000),
                    peak_temp_size: Some(3_200_000_000),
                    started_ms: Some(3_120),
                    query_ms: Some(990),
                    limits: Some(CombineCaps {
                        memory_limit: 2 << 30,
                        max_temp_size: 10 << 30,
                        timeout_ms: 600_000,
                    }),
                    plan: None,
                }),
                StepMetrics::Output(OutputMetrics {
                    number: 1,
                    file: "values-2026-10.csv".to_owned(),
                    rows: 40,
                    size: 2_100,
                    elapsed_ms: 2,
                    started_ms: Some(3_120),
                    ended_ms: Some(4_125),
                }),
            ],
            origin: RunOrigin::App,
            version: None,
            profile: RunProfile {
                peak_memory: Some(2_600_000_000),
                timeout_ms: Some(1_800_000),
                cores: Some(10),
                dbrover: Some("0.31.0".to_owned()),
            },
        }
    }

    fn failed() -> RunOutcome {
        RunOutcome::Failed(RunError {
            step: "the combine step".to_owned(),
            message: "DuckDB ran past 2 GiB, its memory_limit".to_owned(),
        })
    }

    #[test]
    fn reads_back_each_run_newest_first_with_every_number() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        let runs = [
            run("values", 0, RunOutcome::Finished),
            run("values", 2, failed()),
            run("values", 1, RunOutcome::Cancelled),
        ];
        for run in &runs {
            history.record_run(run, DEFAULT_RUNS_KEPT).unwrap();
        }
        // Each comes back with its id, in the order it was kept.
        let kept = |index: usize, id| ReportRun {
            id: Some(RunId(id)),
            ..runs[index].clone()
        };
        let read = history.runs(&name("values"), 10).unwrap();
        assert_eq!(read, [kept(1, 2), kept(2, 3), kept(0, 1)]);
        assert_eq!(history.runs(&name("values"), 1).unwrap(), [kept(1, 2)]);
        assert!(history.runs(&name("other"), 10).unwrap().is_empty());
    }

    #[test]
    fn keeps_the_newest_runs_of_each_report_alone() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        let kept = NonZeroU32::new(3).unwrap();
        history
            .record_run(&run("other", 0, RunOutcome::Finished), kept)
            .unwrap();
        for minute in 1..=5 {
            history
                .record_run(&run("values", minute, RunOutcome::Finished), kept)
                .unwrap();
        }
        let minutes = |report: &str| -> Vec<i64> {
            history
                .runs(&name(report), 10)
                .unwrap()
                .iter()
                .map(|run| (run.started_at.as_second() - 1_790_812_800) / 60)
                .collect()
        };
        assert_eq!(minutes("values"), [5, 4, 3]);
        assert_eq!(minutes("other"), [0], "another report's runs stay");

        assert_eq!(history.clear_runs(&name("values")).unwrap(), 3);
        assert!(minutes("values").is_empty());
        assert_eq!(minutes("other"), [0]);
    }

    #[test]
    fn adds_the_runs_table_to_a_version_2_file_and_keeps_its_statements() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        let shop = DataSourceId::generate();
        let history = file.open().unwrap();
        history
            .record(&HistoryEntry {
                data_source: shop,
                executed_at: Timestamp::UNIX_EPOCH,
                sql: "SELECT 1".to_owned(),
                duration: None,
                rows: None,
                outcome: HistoryOutcome::Ok,
                source: HistorySource::DbRover,
            })
            .unwrap();
        drop(history);
        let version_2 = rusqlite::Connection::open(file.path()).unwrap();
        version_2
            .execute_batch("DROP TABLE report_runs; PRAGMA user_version = 2;")
            .unwrap();

        let history = file.open().unwrap();
        history
            .record_run(&run("values", 0, RunOutcome::Finished), DEFAULT_RUNS_KEPT)
            .unwrap();
        assert_eq!(history.runs(&name("values"), 10).unwrap().len(), 1);
        assert_eq!(
            history.search(&HistoryQuery::all(shop), 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn moves_a_renamed_reports_runs_over_any_kept_under_its_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        for (report, minute) in [("sales", 0), ("sales", 1), ("monthly", 2), ("stock", 3)] {
            history
                .record_run(
                    &run(report, minute, RunOutcome::Finished),
                    DEFAULT_RUNS_KEPT,
                )
                .unwrap();
        }
        assert_eq!(
            history
                .rename_runs(&name("sales"), &name("monthly"))
                .unwrap(),
            2
        );
        assert!(history.runs(&name("sales"), 10).unwrap().is_empty());
        let moved = history.runs(&name("monthly"), 10).unwrap();
        let minutes: Vec<i64> = moved
            .iter()
            .map(|run| (run.started_at.as_second() - 1_790_812_800) / 60)
            .collect();
        assert_eq!(minutes, [1, 0], "the old monthly's run went");
        assert!(moved.iter().all(|run| run.report == name("monthly")));
        assert_eq!(history.runs(&name("stock"), 10).unwrap().len(), 1);
    }

    #[test]
    fn keeps_where_each_run_came_from_and_reads_older_runs_as_the_apps() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        drop(file.open().unwrap());
        // A version 3 file, with a run from before origins.
        let version_3 = rusqlite::Connection::open(file.path()).unwrap();
        version_3
            .execute_batch(
                "ALTER TABLE report_runs DROP COLUMN origin; PRAGMA user_version = 3;
                 INSERT INTO report_runs (report, started_at, elapsed_ms, outcome, params, steps)
                 VALUES ('values', '2026-10-01T00:00:00Z', 5, 'finished', '{}', '[]');",
            )
            .unwrap();
        drop(version_3);

        let history = file.open().unwrap();
        for (minute, origin) in [(1, RunOrigin::Command), (2, RunOrigin::Mcp)] {
            let run = ReportRun {
                origin,
                ..run("values", minute, RunOutcome::Finished)
            };
            history.record_run(&run, DEFAULT_RUNS_KEPT).unwrap();
        }
        let origins: Vec<RunOrigin> = history
            .runs(&name("values"), 10)
            .unwrap()
            .iter()
            .map(|run| run.origin)
            .collect();
        assert_eq!(
            origins,
            [RunOrigin::Mcp, RunOrigin::Command, RunOrigin::App]
        );
    }

    #[test]
    fn keeps_the_version_each_run_ran_and_reads_older_runs_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        drop(file.open().unwrap());
        // A version 4 file, with a run from before versions.
        let version_4 = rusqlite::Connection::open(file.path()).unwrap();
        version_4
            .execute_batch(
                "ALTER TABLE report_runs DROP COLUMN version; PRAGMA user_version = 4;
                 INSERT INTO report_runs (report, started_at, elapsed_ms, outcome, params, steps)
                 VALUES ('values', '2026-10-01T00:00:00Z', 5, 'finished', '{}', '[]');",
            )
            .unwrap();
        drop(version_4);

        let history = file.open().unwrap();
        let ran = BlobId::of(b"[report]\ntitle = \"Values\"\n");
        let run = ReportRun {
            version: Some(ran),
            ..run("values", 1, RunOutcome::Finished)
        };
        history.record_run(&run, DEFAULT_RUNS_KEPT).unwrap();

        let versions: Vec<Option<BlobId>> = history
            .runs(&name("values"), 10)
            .unwrap()
            .iter()
            .map(|run| run.version)
            .collect();
        assert_eq!(versions, [Some(ran), None]);
    }

    #[test]
    fn reads_a_run_from_before_profiles_as_one_that_measured_less() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        drop(file.open().unwrap());
        // A version 5 file, with a run as 0.31.0 kept it.
        let version_5 = rusqlite::Connection::open(file.path()).unwrap();
        version_5
            .execute_batch(
                r#"ALTER TABLE report_runs DROP COLUMN profile; PRAGMA user_version = 5;
                 INSERT INTO report_runs (report, started_at, elapsed_ms, outcome, params, steps)
                 VALUES ('values', '2026-10-01T00:00:00Z', 5, 'finished', '{}',
                   '[{"step":"source","id":"tx","data_source":"ledger","rows":3,"bytes":30,
                      "first_row_ms":1,"elapsed_ms":4}]');"#,
            )
            .unwrap();
        drop(version_5);

        let history = file.open().unwrap();
        let [old] = history
            .runs(&name("values"), 10)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(old.profile, RunProfile::default());
        let [StepMetrics::Source(source)] = old.steps.as_slice() else {
            panic!("{:?}", old.steps);
        };
        assert_eq!((source.started_ms, source.phases), (None, None));

        // And a newer run's steps read as RP4's, for a DbRover from before RS1b.
        let new = run("values", 1, RunOutcome::Finished);
        history.record_run(&new, DEFAULT_RUNS_KEPT).unwrap();
        let steps: String = rusqlite::Connection::open(file.path())
            .unwrap()
            .query_row(
                "SELECT steps FROM report_runs ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        /// A step as RP4 read one, strictly by its tag.
        #[derive(Deserialize)]
        #[serde(tag = "step", rename_all = "snake_case")]
        enum Rp4Step {
            Source { id: String },
            Attached { id: String },
            Combine { rows: u64 },
            Output { number: usize },
        }
        let read: Vec<Rp4Step> = serde_json::from_str(&steps).unwrap();
        let named: Vec<String> = read
            .into_iter()
            .map(|step| match step {
                Rp4Step::Source { id } | Rp4Step::Attached { id } => id,
                Rp4Step::Combine { rows } => format!("combine {rows}"),
                Rp4Step::Output { number } => format!("output {number}"),
            })
            .collect();
        assert_eq!(named, ["tx", "prices", "combine 40", "output 1"]);
    }

    #[test]
    fn skips_a_run_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        let history = file.open().unwrap();
        history
            .record_run(&run("values", 0, RunOutcome::Finished), DEFAULT_RUNS_KEPT)
            .unwrap();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch(
                "INSERT INTO report_runs (report, started_at, elapsed_ms, outcome, params, steps)
                 VALUES ('values', '2026-10-01T00:05:00.000Z', 1, 'later', '{}', '[]')",
            )
            .unwrap();
        assert_eq!(history.runs(&name("values"), 10).unwrap().len(), 1);
    }
}
