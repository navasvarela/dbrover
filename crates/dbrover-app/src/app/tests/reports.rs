//! Reports (RP1f), driven through the whole app: picked in the explorer, their parameters
//! filled and checked in their tab, run, and their files, preview or error shown. Runs go to a
//! stand-in for the worker, which takes the request and answers with scripted events; the
//! real worker is tested by `tests/report_worker.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dbrover_report::{
    Batch, LimitHit, LoadedColumn, Preview, RunEvent, RunFailure, RunRequest, SourceStart, Step,
    WrittenFile,
};
use dbrover_store::{ReportFiles, ReportName, RunOutcome, StepMetrics};
use egui_kittest::kittest::NodeT;

use super::*;
use crate::reports::{ReportLauncher, ReportsSetup, RunningReport};

const TRIAL_BALANCE: &str = r##"
[report]
name = "Trial balance"
description = "Debits and credits per account."
params = { since = "date", until = "date" }

[[source]]
id = "balances"
data_source = "shop"
query = "SELECT account, SUM(amount) AS balance FROM entries WHERE booked_on >= :since AND booked_on < :until GROUP BY account"

[[output]]
format = "xlsx"
file = "trial-balance-{since:%Y-%m}.xlsx"
columns = [
  { name = "Account", from = "account" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
"##;

/// The trial balance from two sources, combined in DuckDB.
const COMBINED: &str = r##"
[report]
name = "Trial balance"
description = "Debits and credits per account."
params = { since = "date", until = "date" }

[[source]]
id = "entries"
data_source = "shop"
query = "SELECT account, amount FROM entries WHERE booked_on >= :since AND booked_on < :until"

[[source]]
id = "accounts"
data_source = "shop"
query = "SELECT DISTINCT account FROM entries"

[combine]
query = "SELECT account, SUM(amount) AS balance FROM entries JOIN accounts USING (account) GROUP BY account"

[[output]]
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
"##;

/// Takes each request, and answers it with the script's events, then nothing.
#[derive(Clone)]
struct StandIn {
    requests: Arc<Mutex<Vec<RunRequest>>>,
    script: Vec<RunEvent>,
}

impl StandIn {
    fn new(script: Vec<RunEvent>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            script,
        }
    }
}

impl ReportLauncher for StandIn {
    fn start(
        &self,
        request: &RunRequest,
        _timeout: Duration,
        _wake: Box<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Box<dyn RunningReport>> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Box::new(Scripted(self.script.iter().cloned().collect())))
    }
}

struct Scripted(VecDeque<RunEvent>);

impl RunningReport for Scripted {
    fn try_next(&mut self) -> Option<RunEvent> {
        self.0.pop_front()
    }

    fn cancel(&mut self) {
        self.0 = VecDeque::from([RunEvent::Cancelled]);
    }
}

/// The app with the `shop` data source and `reports/trial-balance.toml` (or `report`), its
/// runs going to `launcher`.
fn start_with_reports(dir: &Path, report: &str, launcher: StandIn) -> Harness<'static, DbRoverApp> {
    save_shop(
        dir,
        "CREATE TABLE entries (account TEXT, booked_on TEXT, amount NUMERIC);",
    );
    let files = ReportFiles::new(dir.join("reports"));
    std::fs::create_dir_all(files.dir()).unwrap();
    std::fs::write(files.dir().join("trial-balance.toml"), report).unwrap();
    let setup = ReportsSetup {
        files,
        launcher: Box::new(launcher),
        default_folder: dir.join("out"),
    };
    let (_layer, feed) = log_console::pipeline();
    let config = engine_config(dir);
    let mut harness = harness_at_default_size().build_eframe(|cc| {
        DbRoverApp::new(cc, feed, config)
            .unwrap()
            .with_reports(setup)
    });
    crate::test_support::wait_for_startup(&mut harness);
    step_until_visible(&mut harness, "Trial balance");
    harness
}

/// Opens the report from the explorer and fills in October 2026.
fn open_and_fill(harness: &mut Harness<'_, DbRoverApp>, since: &str) {
    harness.get_by_label("Trial balance").click();
    step_until_visible(harness, "Debits and credits per account.");
    for (label, value) in [("since", since), ("until", "2026-11-01")] {
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .type_text(value);
        harness.run();
    }
}

fn finished_run(folder: &Path) -> Vec<RunEvent> {
    let written = |name: &str, size| {
        RunEvent::OutputWritten(WrittenFile {
            path: folder.join(name),
            rows: 2,
            size,
            elapsed_ms: 3,
            warnings: Vec::new(),
        })
    };
    vec![
        RunEvent::Started {
            report: "Trial balance".to_owned(),
            sources: vec![SourceStart {
                id: "balances".to_owned(),
                data_source: "shop".to_owned(),
                attached: false,
                waits_for: None,
            }],
            combine: false,
        },
        RunEvent::SourceDone {
            id: "balances".to_owned(),
            rows: 2,
            bytes: 30,
            first_row_ms: Some(4),
            elapsed_ms: 5,
            table: Vec::new(),
            warnings: Vec::new(),
            batches: None,
        },
        RunEvent::Preview(Preview {
            columns: vec!["account".to_owned(), "balance".to_owned()],
            rows: vec![
                vec![Some("1000".to_owned()), Some("150.375".to_owned())],
                vec![Some("4000".to_owned()), None],
            ],
            more: false,
        }),
        written("trial-balance-2026-10.xlsx", 6_100),
        written("trial-balance-2026-10.csv", 48),
        RunEvent::Finished { elapsed_ms: 1_200 },
    ]
}

#[test]
fn runs_a_report_from_its_tab_and_shows_its_files_and_preview() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let launcher = StandIn::new(finished_run(&out));
    let requests = Arc::clone(&launcher.requests);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label_contains("trial-balance-2026-10.xlsx, trial-balance-2026-10.csv");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Finished in 1.2 s: 2 rows read.");

    let requests = requests.lock().unwrap();
    let [request] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(
        request.report,
        dir.path().join("reports/trial-balance.toml")
    );
    assert_eq!(request.folder, out);
    assert!(request.staging.starts_with(&out), "{request:?}");
    assert_eq!(
        request.values.get("since").map(String::as_str),
        Some("2026-10-01")
    );
    harness.get_by_label("trial-balance-2026-10.xlsx");
    harness.get_by_label("trial-balance-2026-10.csv");
    harness.get_by_label("Preview: All 2 rows, as the source returned them");
    harness.get_by_label("150.375");
    harness.get_by_label("NULL");

    let history = HistoryFile::new(dir.path().join("history.sqlite"));
    let report = ReportName::new("trial-balance").unwrap();
    let runs = || history.open().unwrap().runs(&report, 5).unwrap();
    step_until(&mut harness, "the run kept in the history", |_| {
        !runs().is_empty()
    });
    let [run] = runs().try_into().unwrap_or_else(|runs| panic!("{runs:?}"));
    assert_eq!(run.outcome, RunOutcome::Finished);
    assert_eq!(run.elapsed, Duration::from_millis(1_200));
    assert_eq!(
        run.params.get("since").map(String::as_str),
        Some("2026-10-01")
    );
    let steps: Vec<String> = run
        .steps
        .iter()
        .map(|step| match step {
            StepMetrics::Source(source) => format!("{} {} rows", source.id, source.rows),
            StepMetrics::Output(output) => format!("{} {} bytes", output.file, output.size),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        steps,
        [
            "balances 2 rows",
            "trial-balance-2026-10.xlsx 6100 bytes",
            "trial-balance-2026-10.csv 48 bytes"
        ]
    );
}

/// The start of a run of `COMBINED`, both sources loaded.
fn combined_start() -> Vec<RunEvent> {
    let source = |id: &str| SourceStart {
        id: id.to_owned(),
        data_source: "shop".to_owned(),
        attached: false,
        waits_for: None,
    };
    let loaded = |id: &str, rows, table: Vec<(&str, &str)>| RunEvent::SourceDone {
        id: id.to_owned(),
        rows,
        bytes: rows * 20,
        first_row_ms: Some(3),
        elapsed_ms: 40,
        table: table
            .into_iter()
            .map(|(name, type_name)| LoadedColumn {
                name: name.to_owned(),
                type_name: type_name.to_owned(),
                declared: false,
            })
            .collect(),
        warnings: Vec::new(),
        batches: None,
    };
    vec![
        RunEvent::Started {
            report: "Trial balance".to_owned(),
            sources: vec![source("entries"), source("accounts")],
            combine: true,
        },
        loaded(
            "entries",
            3,
            vec![("account", "VARCHAR"), ("amount", "DECIMAL(6,3)")],
        ),
        loaded("accounts", 2, vec![("account", "VARCHAR")]),
    ]
}

#[test]
fn shows_each_source_and_the_combine_step_of_a_combined_run() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut script = combined_start();
    script.extend([
        RunEvent::CombineDone {
            rows: 2,
            bytes: 40,
            first_row_ms: Some(1),
            elapsed_ms: 15,
            peak_memory: Some(40 << 20),
            peak_temp_size: Some(0),
        },
        RunEvent::Preview(Preview {
            columns: vec!["account".to_owned(), "balance".to_owned()],
            rows: vec![vec![Some("1000".to_owned()), Some("150.375".to_owned())]],
            more: true,
        }),
        RunEvent::OutputWritten(WrittenFile {
            path: out.join("trial-balance-2026-10.csv"),
            rows: 2,
            size: 48,
            elapsed_ms: 3,
            warnings: Vec::new(),
        }),
        RunEvent::Finished { elapsed_ms: 1_200 },
    ]);
    let mut harness = start_with_reports(dir.path(), COMBINED, StandIn::new(script));

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Finished in 1.2 s: 2 rows combined.");
    harness.get_by_label("Source entries");
    harness.get_by_label("Source accounts");
    harness.get_by_label("Combine");
    harness.get_by_label("3 rows, 60 bytes · 0.0 s");
    harness.get_by_label("2 columns in DuckDB").click();
    harness.run();
    harness.get_by_label("DECIMAL(6,3)");
    // Once in the line of the files the report writes, and once as the file written.
    assert_eq!(
        harness
            .get_all_by_label("trial-balance-2026-10.csv")
            .count(),
        2
    );
    harness.get_by_label("Preview: The first 1 rows, as the combine step returned them");
}

/// Entries, and their counterparties read by the entries' accounts.
const LOOKUP: &str = r##"
[report]
name = "Trial balance"
description = "Debits and credits per account."
params = { since = "date", until = "date" }

[[source]]
id = "entries"
data_source = "shop"
query = "SELECT account, amount FROM entries WHERE booked_on >= :since AND booked_on < :until"

[[source]]
id = "accounts"
data_source = "shop"
lookup = { from = "entries", column = "account", as = "codes" }
query = "SELECT account, name FROM accounts WHERE account IN :codes"
columns = [{ name = "account" }, { name = "name" }]

[combine]
query = "SELECT * FROM entries JOIN accounts USING (account)"

[[output]]
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
"##;

#[test]
fn shows_a_lookup_waiting_then_its_batches_then_done() {
    let start = RunEvent::Started {
        report: "Trial balance".to_owned(),
        sources: vec![
            SourceStart {
                id: "entries".to_owned(),
                data_source: "shop".to_owned(),
                attached: false,
                waits_for: None,
            },
            SourceStart {
                id: "accounts".to_owned(),
                data_source: "shop".to_owned(),
                attached: false,
                waits_for: Some("entries".to_owned()),
            },
        ],
        combine: true,
    };
    let progress = |id: &str, batch| RunEvent::Progress {
        step: Step::Source { id: id.to_owned() },
        rows: 1_000,
        bytes: 20_000,
        elapsed_ms: 1_000,
        batch,
    };
    let entries_done = RunEvent::SourceDone {
        id: "entries".to_owned(),
        rows: 3,
        bytes: 60,
        first_row_ms: Some(3),
        elapsed_ms: 40,
        table: Vec::new(),
        warnings: Vec::new(),
        batches: None,
    };

    let dir = tempfile::tempdir().unwrap();
    let script = vec![start.clone(), progress("entries", None)];
    let mut harness = start_with_reports(dir.path(), LOOKUP, StandIn::new(script));
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "waiting for entries");
    drop(harness);

    let dir = tempfile::tempdir().unwrap();
    let batch = Batch { number: 2, of: 7 };
    let script = vec![
        start.clone(),
        entries_done.clone(),
        progress("accounts", Some(batch)),
    ];
    let mut harness = start_with_reports(dir.path(), LOOKUP, StandIn::new(script));
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "batch 2 of 7 · 1,000 rows, 20.0 KB · 1.0 s");
    drop(harness);

    let dir = tempfile::tempdir().unwrap();
    let script = vec![
        start,
        entries_done,
        RunEvent::SourceDone {
            id: "accounts".to_owned(),
            rows: 2,
            bytes: 40,
            first_row_ms: Some(5),
            elapsed_ms: 2_500,
            table: ["account", "name"]
                .into_iter()
                .map(|name| LoadedColumn {
                    name: name.to_owned(),
                    type_name: "VARCHAR".to_owned(),
                    declared: true,
                })
                .collect(),
            warnings: Vec::new(),
            batches: Some(7),
        },
        RunEvent::CombineDone {
            rows: 3,
            bytes: 90,
            first_row_ms: Some(1),
            elapsed_ms: 10,
            peak_memory: None,
            peak_temp_size: None,
        },
        RunEvent::Finished { elapsed_ms: 3_000 },
    ];
    let mut harness = start_with_reports(dir.path(), LOOKUP, StandIn::new(script));
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Finished in 3.0 s: 3 rows combined.");
    harness.get_by_label("7 batches · 2 rows, 40 bytes · 2.5 s");
    harness.get_by_label("2 columns in DuckDB").click();
    harness.run();
    assert_eq!(harness.get_all_by_label("declared").count(), 2);
}

#[test]
fn marks_the_combine_step_a_run_failed_at() {
    let dir = tempfile::tempdir().unwrap();
    let mut script = combined_start();
    script.push(RunEvent::Failed(RunFailure::at_limit(
        Step::Combine,
        "DuckDB ran past 2 GiB, its memory_limit, and stopped",
        LimitHit {
            key: "memory_limit".to_owned(),
            value: "2 GiB".to_owned(),
            raise_in: "[combine]".to_owned(),
        },
    )));
    let mut harness = start_with_reports(dir.path(), COMBINED, StandIn::new(script));

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Failed at the combine step.");
    harness.get_by_label("failed");
    harness.get_by_label_contains(
        "To allow more, raise memory_limit in [combine] of the report's file.",
    );
}

#[test]
fn checks_each_value_before_the_report_can_run() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(Vec::new());
    let requests = Arc::clone(&launcher.requests);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-13-01");
    harness.get_by_label("isn't a date, as 2026-10-01");
    assert!(harness.get_by_label("Run").accesskit_node().is_disabled());
    harness.get_by_label("Run").click();
    harness.run();
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn shows_a_failed_run_and_the_limit_it_hit() {
    let dir = tempfile::tempdir().unwrap();
    let limit = LimitHit {
        key: "max_rows".to_owned(),
        value: "1,000,000 rows".to_owned(),
        raise_in: "the [[source]] \"balances\"".to_owned(),
    };
    let launcher = StandIn::new(vec![RunEvent::Failed(RunFailure::at_limit(
        Step::Source {
            id: "balances".to_owned(),
        },
        "the source returned more than 1,000,000 rows, its max_rows",
        limit,
    ))]);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Failed at source \"balances\".");
    harness.get_by_label("the source returned more than 1,000,000 rows, its max_rows");
    harness.get_by_label_contains(
        "To allow more, raise max_rows in the [[source]] \"balances\" of the report's file.",
    );
}

#[test]
fn cancel_stops_the_running_report() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(vec![RunEvent::Started {
        report: "Trial balance".to_owned(),
        sources: vec![SourceStart {
            id: "balances".to_owned(),
            data_source: "shop".to_owned(),
            attached: false,
            waits_for: None,
        }],
        combine: false,
    }]);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    // A run in progress shows a spinner, which repaints every frame: step rather than run.
    step_until_visible(&mut harness, "Reading the source");
    assert!(harness.get_by_label("Run").accesskit_node().is_disabled());
    harness.get_by_label("Cancel").click();
    step_until_visible(&mut harness, "Cancelled. No file was written.");
    harness.run();
    assert!(!harness.get_by_label("Run").accesskit_node().is_disabled());
}

#[test]
fn a_report_with_problems_says_what_they_are_and_cant_run() {
    let dir = tempfile::tempdir().unwrap();
    let broken = TRIAL_BALANCE.replace("data_source = \"shop\"", "data_source = \"books\"");
    let mut harness = start_with_reports(dir.path(), &broken, StandIn::new(Vec::new()));

    harness.get_by_label("Trial balance").click();
    step_until_visible(
        &mut harness,
        "This report can't run until its file is fixed:",
    );
    harness.get_by_label_contains("there is no data source \"books\"; there are shop");
    assert!(harness.query_by_label("Run").is_none());
}

#[test]
fn new_report_writes_a_starter_file_and_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));

    harness.get_by_label("Reports").click_secondary();
    harness.run();
    harness.get_by_label("New report").click();
    step_until_visible(&mut harness, "What this report is for.");
    let written: PathBuf = dir.path().join("reports/report.toml");
    let text = std::fs::read_to_string(&written).unwrap();
    assert!(text.contains("data_source = \"shop\""), "{text}");
}
