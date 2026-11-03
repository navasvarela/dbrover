//! `dbrover report-worker` as the app runs it: the real binary, started through
//! `dbrover_engine::WorkerRun`, on a settings folder with a SQLite ledger. The run itself is
//! tested in dbrover-report and dbrover-engine; this checks the process around it: the request,
//! the events, Cancel, the run's time limit and, on Linux, the memory limit.
//!
//! Unix only: the home folder is moved by setting `HOME`, which Windows ignores when it looks
//! up the user's folders, and the tests must not read the real one's.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, SafetyLevel, SqliteConfig};
use dbrover_engine::WorkerRun;
use dbrover_report::{LimitDefaults, RunEvent, RunRequest, SourceLimits, SourceStart, Step};
use dbrover_store::DataSourceFile;

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

/// Longer than any of these runs takes on a loaded CI runner, short enough that a hang fails.
const EVENT_WAIT: Duration = Duration::from_secs(60);

/// A home folder, its settings folder with a SQLite ledger as the data source `ledger`, and
/// the report `trial-balance.toml` in it.
struct Settings {
    home: tempfile::TempDir,
}

impl Settings {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let settings = Self { home };
        let database = settings.home.path().join("ledger.db");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(
                "CREATE TABLE accounts (id INTEGER PRIMARY KEY, code TEXT, name TEXT);
                 CREATE TABLE entries (account_id INTEGER, booked_on TEXT, debit NUMERIC, credit NUMERIC);
                 INSERT INTO accounts VALUES (1, '1000', 'Cash'), (2, '4000', 'Sales');
                 INSERT INTO entries VALUES
                   (1, '2026-10-01', 100.25, 0), (2, '2026-10-01', 0, 100.25),
                   (1, '2026-11-02', 5, 0);",
            )
            .unwrap();
        DataSourceFile::new(settings.config().join("data_sources.toml"))
            .save(&[DataSource {
                id: DataSourceId::generate(),
                name: "ledger".to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config: ConnectionConfig::Sqlite(SqliteConfig {
                    path: database,
                    read_only: false,
                }),
            }])
            .unwrap();
        settings.write_report("trial-balance", TRIAL_BALANCE);
        settings
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("settings")
    }

    fn out(&self) -> PathBuf {
        self.home.path().join("out")
    }

    fn write_report(&self, name: &str, text: &str) -> PathBuf {
        let path = self.config().join("reports").join(format!("{name}.toml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command
            .arg("report-worker")
            .arg("--config-dir")
            .arg(self.config())
            .env("HOME", self.home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("DBROVER_CONFIG_DIR");
        command
    }

    fn request(&self, report: &Path) -> RunRequest {
        RunRequest {
            report: report.to_owned(),
            values: [("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            folder: self.out(),
            staging: self.out().join(".run"),
            defaults: LimitDefaults::default(),
            memory_limit: None,
        }
    }

    fn start(&self, request: &RunRequest, timeout: Duration) -> WorkerRun {
        WorkerRun::start(self.command(), request, timeout, || {}).unwrap()
    }
}

const TRIAL_BALANCE: &str = r##"
[report]
name = "Trial balance"
params = { since = "date", until = "date" }

[[source]]
id = "balances"
data_source = "ledger"
query = """
SELECT a.code AS account, SUM(e.debit) - SUM(e.credit) AS balance
FROM entries e JOIN accounts a ON a.id = e.account_id
WHERE e.booked_on >= :since AND e.booked_on < :until
GROUP BY a.code ORDER BY a.code
"""

[[output]]
format = "xlsx"
file = "trial-balance-{since:%Y-%m}.xlsx"
columns = [
  { name = "Account", from = "account", type = "text" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
"##;

/// The trial balance again, with the ledger's two tables as two sources combined in DuckDB.
const COMBINED: &str = r##"
[report]
name = "Combined trial balance"
params = { since = "date", until = "date" }

[[source]]
id = "accounts"
data_source = "ledger"
query = "SELECT id, code FROM accounts"

[[source]]
id = "entries"
data_source = "ledger"
query = """
SELECT account_id, debit, credit FROM entries
WHERE booked_on >= :since AND booked_on < :until
"""

[combine]
query = """
SELECT a.code AS account, SUM(e.debit) - SUM(e.credit) AS balance
FROM entries e JOIN accounts a ON a.id = e.account_id
GROUP BY a.code ORDER BY a.code
"""
threads = 1

[[output]]
format = "csv"
file = "combined-{since:%Y-%m}.csv"
"##;

/// A source that counts for a long time before returning its one row.
const SLOW: &str = r#"
[report]
name = "Slow"
params = { since = "date", until = "date" }
[[source]]
id = "count"
data_source = "ledger"
query = """
WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 5000000000)
SELECT max(x), :since, :until FROM n
"""
[[output]]
format = "csv"
file = "slow.csv"
"#;

/// Every event of the run, up to the one that ends it.
fn all_events(run: &WorkerRun) -> Vec<RunEvent> {
    let mut events = Vec::new();
    loop {
        let event = run
            .next_within(EVENT_WAIT)
            .unwrap_or_else(|| panic!("no event within {EVENT_WAIT:?} after {events:#?}"));
        let ends = event.ends_run();
        events.push(event);
        if ends {
            return events;
        }
    }
}

fn kind(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::Started { .. } => "started",
        RunEvent::Progress { .. } => "progress",
        RunEvent::SourceDone { .. } => "source done",
        RunEvent::SourceAttached { .. } => "attached",
        RunEvent::CombineDone { .. } => "combine done",
        RunEvent::Preview(_) => "preview",
        RunEvent::OutputWritten(_) => "written",
        RunEvent::Finished { .. } => "finished",
        RunEvent::Failed(_) => "failed",
        RunEvent::Cancelled => "cancelled",
    }
}

fn files_in(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn combines_two_sources_in_duckdb() {
    let settings = Settings::new();
    let report = settings.write_report("combined", COMBINED);
    let run = settings.start(&settings.request(&report), Duration::from_secs(120));
    let events = all_events(&run);
    let mut kinds: Vec<&str> = events
        .iter()
        .map(kind)
        .filter(|kind| *kind != "progress")
        .collect();
    // The two sources load at the same time, so either may finish first.
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            "started",
            "source done",
            "combine done",
            "preview",
            "written",
            "finished"
        ],
        "{events:#?}"
    );
    assert_eq!(files_in(&settings.out()), ["combined-2026-10.csv"]);
    assert_eq!(
        std::fs::read_to_string(settings.out().join("combined-2026-10.csv")).unwrap(),
        "account,balance\r\n1000,100.25\r\n4000,-100.25\r\n"
    );
}

/// Waits until the run has started its source, so what follows happens mid-run.
fn wait_until_started(run: &WorkerRun) {
    let started = run
        .next_within(EVENT_WAIT)
        .expect("the worker said nothing");
    assert_eq!(kind(&started), "started", "{started:?}");
}

#[test]
fn runs_a_report_and_reports_each_step() {
    let settings = Settings::new();
    let report = settings.config().join("reports/trial-balance.toml");
    let run = settings.start(&settings.request(&report), Duration::from_secs(120));
    let events = all_events(&run);
    let kinds: Vec<&str> = events
        .iter()
        .map(kind)
        .filter(|kind| *kind != "progress")
        .collect();
    assert_eq!(
        kinds,
        [
            "started",
            "source done",
            "preview",
            "written",
            "written",
            "finished"
        ],
        "{events:#?}"
    );
    assert_eq!(
        events[0],
        RunEvent::Started {
            report: "Trial balance".to_owned(),
            sources: vec![SourceStart {
                id: "balances".to_owned(),
                data_source: "ledger".to_owned(),
                attached: false,
                waits_for: None,
            }],
            combine: false,
        }
    );
    assert_eq!(
        files_in(&settings.out()),
        ["trial-balance-2026-10.csv", "trial-balance-2026-10.xlsx"]
    );
    assert_eq!(
        std::fs::read_to_string(settings.out().join("trial-balance-2026-10.csv")).unwrap(),
        "Account,Balance\r\n1000,100.25\r\n4000,-100.25\r\n"
    );
}

#[test]
fn says_what_is_wrong_with_the_report_or_its_values() {
    let settings = Settings::new();
    let report = settings.write_report("broken", "[report]\nname = \"Broken\"\n");
    let events = all_events(&settings.start(&settings.request(&report), Duration::from_secs(120)));
    let [RunEvent::Failed(failure)] = events.as_slice() else {
        panic!("{events:#?}");
    };
    assert_eq!(failure.step, Step::Report);
    assert!(failure.message.contains("no [[source]] table"), "{failure}");

    let report = settings.config().join("reports/trial-balance.toml");
    let mut request = settings.request(&report);
    request
        .values
        .insert("until".to_owned(), "next month".to_owned());
    let events = all_events(&settings.start(&request, Duration::from_secs(120)));
    let [RunEvent::Failed(failure)] = events.as_slice() else {
        panic!("{events:#?}");
    };
    assert_eq!(
        failure.message,
        "until: \"next month\" isn't a date, as 2026-10-01"
    );
}

/// Settings › Reports' defaults, sent with the request, bound a source whose file sets no
/// limit of its own.
#[test]
fn runs_under_the_defaults_the_request_sends() {
    let settings = Settings::new();
    let report = settings.config().join("reports/trial-balance.toml");
    let mut request = settings.request(&report);
    request.defaults.source = SourceLimits {
        max_rows: 1,
        ..SourceLimits::default()
    };
    let events = all_events(&settings.start(&request, Duration::from_secs(120)));
    let Some(RunEvent::Failed(failure)) = events.last() else {
        panic!("{events:#?}");
    };
    let limit = failure
        .limit
        .as_ref()
        .unwrap_or_else(|| panic!("{failure}"));
    assert_eq!(
        (limit.key.as_str(), limit.value.as_str()),
        ("max_rows", "1 rows")
    );
    assert!(files_in(&settings.out()).is_empty());
}

#[test]
fn cancel_kills_the_worker_and_leaves_no_file() {
    let settings = Settings::new();
    let report = settings.write_report("slow", SLOW);
    let run = settings.start(&settings.request(&report), Duration::from_secs(120));
    wait_until_started(&run);
    let cancelled = Instant::now();
    run.cancel();
    let events = all_events(&run);
    assert_eq!(events.last(), Some(&RunEvent::Cancelled), "{events:#?}");
    assert!(cancelled.elapsed() < Duration::from_secs(10));
    assert_eq!(files_in(&settings.out()), Vec::<String>::new());
}

#[test]
fn stops_a_run_past_its_time_limit() {
    let settings = Settings::new();
    let report = settings.write_report("slow", SLOW);
    let run = settings.start(&settings.request(&report), Duration::from_secs(2));
    let events = all_events(&run);
    let Some(RunEvent::Failed(failure)) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(failure.step, Step::Worker);
    assert_eq!(
        failure.limit.as_ref().map(|limit| limit.key.as_str()),
        Some("timeout")
    );
    assert_eq!(files_in(&settings.out()), Vec::<String>::new());
}

/// Under a small `RLIMIT_DATA`, a source that builds a string far larger fails cleanly: the
/// run ends with an error, whether SQLite reports running out of memory or the worker aborts.
#[cfg(target_os = "linux")]
#[test]
fn fails_cleanly_past_its_memory_limit() {
    let settings = Settings::new();
    let report = settings.write_report(
        "greedy",
        r#"
        [report]
        name = "Greedy"
        params = { since = "date", until = "date" }
        [[source]]
        id = "blob"
        data_source = "ledger"
        query = """
        WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000000)
        SELECT length(group_concat(hex(randomblob(200)))), :since, :until FROM n
        """
        [[output]]
        format = "csv"
        file = "greedy.csv"
        "#,
    );
    let mut request = settings.request(&report);
    request.memory_limit = Some(256 << 20);
    let events = all_events(&settings.start(&request, Duration::from_secs(120)));
    assert!(
        matches!(events.last(), Some(RunEvent::Failed(_))),
        "{events:#?}"
    );
    assert_eq!(files_in(&settings.out()), Vec::<String>::new());
}
