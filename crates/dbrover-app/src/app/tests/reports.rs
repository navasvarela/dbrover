//! Reports (RP1f), driven through the whole app: picked in the explorer, their parameters
//! filled and checked in their tab, run, and their files, preview or error shown. Runs go to a
//! stand-in for the worker, which takes the request and answers with scripted events; the
//! real worker is tested by `tests/report_worker.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dbrover_report::{
    LimitHit, Preview, RunEvent, RunFailure, RunRequest, SourceStart, Step, WrittenFile,
};
use dbrover_store::ReportFiles;
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
