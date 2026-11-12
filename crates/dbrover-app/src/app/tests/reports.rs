//! Reports (RP1f), driven through the whole app: picked in the explorer, their parameters
//! filled and checked in their tab, run, and their files, preview or error shown. Runs go to a
//! stand-in for the worker, which takes the request and answers with scripted events; the
//! real worker is tested by `tests/report_worker.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dbrover_report::{
    Batch, Busy, LimitHit, LoadedColumn, Preview, RunEvent, RunFailure, RunRequest, SourcePhases,
    SourceStart, Step, TimedEvent, WrittenFile,
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
    script: Vec<TimedEvent>,
}

impl StandIn {
    /// A run whose events come at once, as if all sent at its start.
    fn new(script: Vec<RunEvent>) -> Self {
        Self::timed(
            script
                .into_iter()
                .map(|event| event.at(Duration::ZERO))
                .collect(),
        )
    }

    fn timed(script: Vec<TimedEvent>) -> Self {
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
        _watch: dbrover_engine::RunWatch,
        _wake: Box<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Box<dyn RunningReport>> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Box::new(Scripted(self.script.iter().cloned().collect())))
    }
}

struct Scripted(VecDeque<TimedEvent>);

impl RunningReport for Scripted {
    fn try_next(&mut self) -> Option<TimedEvent> {
        self.0.pop_front()
    }

    fn cancel(&mut self) {
        self.0 = VecDeque::from([RunEvent::Cancelled.at(Duration::ZERO)]);
    }

    fn pause(&mut self) {
        let kept = dbrover_report::WorkKept {
            folder: "paused".to_owned(),
            ranges: 2,
            sources: 0,
            read: 1,
        };
        self.0 = VecDeque::from([
            RunEvent::WorkKept(kept).at(Duration::ZERO),
            RunEvent::Cancelled.at(Duration::ZERO),
        ]);
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
        work_root: dir.join("report-runs"),
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

/// Asserts a widget is labelled `label`, once or more: a step's name and how it ended show in
/// the last run and again in the runs the history keeps, once those are listed.
fn shows(harness: &Harness<'_, DbRoverApp>, label: &str) {
    assert!(
        harness.query_all_by_label(label).next().is_some(),
        "{label:?} isn't shown"
    );
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
            output: None,
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
                engine: None,
                limits: None,
                partition: None,
            }],
            combine: false,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
            partition: None,
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
            parts: None,
            phases: None,
            scanned_bytes: None,
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
    // The side panel names the files the run will write.
    harness.get_by_label("trial-balance-2026-10.xlsx");
    harness.get_by_label("trial-balance-2026-10.csv");
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
    // Once in the side panel's files to write, and once as the file written.
    for file in ["trial-balance-2026-10.xlsx", "trial-balance-2026-10.csv"] {
        assert_eq!(harness.get_all_by_label(file).count(), 2, "{file}");
    }
    harness.get_by_label("Preview").click();
    harness.run();
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

/// A report's chart is a file it writes: named before the run with the outputs' files, and
/// listed with them once written (`docs/charts-as-code.md`, CH3d).
#[test]
fn lists_a_reports_chart_with_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut script = finished_run(&out);
    let chart = RunEvent::OutputWritten(WrittenFile {
        output: None,
        path: out.join("balances-2026-10.svg"),
        rows: 2,
        size: 2_400,
        elapsed_ms: 1,
        warnings: vec!["1 gap in balance: null or not a number".to_owned()],
    });
    let finished = script.len() - 1;
    script.insert(finished, chart);
    let report = format!(
        "{TRIAL_BALANCE}\n[[chart]]\nfile = \"balances-{{since:%Y-%m}}.svg\"\nx = \"account\"\n"
    );
    let mut harness = start_with_reports(dir.path(), &report, StandIn::new(script));

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("1 source · 3 files");
    harness.get_by_label("balances-2026-10.svg");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Finished in 1.2 s: 2 rows read.");

    // Once in the side panel's files to write, and once as the file written.
    assert_eq!(harness.get_all_by_label("balances-2026-10.svg").count(), 2);
    shows(&harness, "1 gap in balance: null or not a number");
}

/// The start of a run of `COMBINED`, both sources loaded.
fn combined_start() -> Vec<RunEvent> {
    let source = |id: &str| SourceStart {
        id: id.to_owned(),
        data_source: "shop".to_owned(),
        attached: false,
        waits_for: None,
        engine: None,
        limits: None,
        partition: None,
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
                bytes: None,
            })
            .collect(),
        warnings: Vec::new(),
        batches: None,
        parts: None,
        phases: None,
        scanned_bytes: None,
    };
    vec![
        RunEvent::Started {
            report: "Trial balance".to_owned(),
            sources: vec![source("entries"), source("accounts")],
            combine: true,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
            partition: None,
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
            query_ms: None,
            plan: None,
        },
        RunEvent::Preview(Preview {
            columns: vec!["account".to_owned(), "balance".to_owned()],
            rows: vec![vec![Some("1000".to_owned()), Some("150.375".to_owned())]],
            more: true,
        }),
        RunEvent::OutputWritten(WrittenFile {
            output: None,
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
    for step in ["Source entries", "Source accounts", "Combine"] {
        shows(&harness, step);
    }
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
    harness.get_by_label("Preview").click();
    harness.run();
    harness.get_by_label("Preview: The first 1 rows, as the combine step returned them");
}

/// [`COMBINED`] with its combine step in PRQL.
fn combined_in_prql() -> String {
    COMBINED.replace(
        "query = \"SELECT account, SUM(amount) AS balance FROM entries JOIN accounts USING (account) GROUP BY account\"",
        "prql = \"from entries | join accounts (==account) | group {entries.account} (aggregate {balance = sum entries.amount})\"",
    )
}

#[test]
fn shows_the_sql_a_prql_combine_step_compiled_to() {
    let dir = tempfile::tempdir().unwrap();
    let compiled = "SELECT\n  entries.account,\n  SUM(entries.amount) AS balance\nFROM\n  entries";
    let mut script = combined_start();
    if let Some(RunEvent::Started { combine_sql, .. }) = script.first_mut() {
        *combine_sql = Some(compiled.to_owned());
    }
    script.push(RunEvent::Failed(RunFailure::new(
        Step::Combine,
        "Binder Error: Referenced column \"amont\" not found",
    )));
    let mut harness = start_with_reports(dir.path(), &combined_in_prql(), StandIn::new(script));
    open_and_fill(&mut harness, "2026-10-01");
    // The run panel's Last run shows what the next run sends.
    harness.get_by_label("Last run").click();
    harness.run();
    harness
        .get_by_label("Combine SQL, compiled from PRQL")
        .click();
    harness.run();
    let shown = |harness: &Harness<'_, DbRoverApp>| {
        harness.query_all_by_label_contains("AS balance").count()
    };
    assert_eq!(shown(&harness), 1, "the tab shows the compiled SQL");

    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Failed at the combine step.");
    // The run's combine line shows the SQL it ran, unfolded, since DuckDB's error points into it.
    harness.get_by_label("The SQL it ran, compiled from PRQL");
    assert_eq!(shown(&harness), 2);
}

#[test]
fn shows_no_compiled_sql_for_a_combine_step_in_sql() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), COMBINED, StandIn::new(Vec::new()));
    open_and_fill(&mut harness, "2026-10-01");
    assert!(
        harness
            .query_by_label("Combine SQL, compiled from PRQL")
            .is_none()
    );
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
fn says_how_fast_a_running_source_reads_and_what_it_waits_on() {
    let mut script = combined_start();
    script.truncate(1);
    if let Some(RunEvent::Started { sources, .. }) = script.first_mut() {
        sources[0].engine = Some("SQLite".to_owned());
    }
    let progress = |elapsed_ms, rows, waiting_pct| RunEvent::Progress {
        step: Step::Source {
            id: "entries".to_owned(),
        },
        rows,
        bytes: rows * 20,
        elapsed_ms,
        batch: None,
        parts: None,
        busy: Some(Busy {
            waiting_pct,
            loading_pct: 4,
            writing_pct: 0,
        }),
    };
    script.extend([progress(1_000, 1_000, 60), progress(3_000, 2_000, 92)]);
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), COMBINED, StandIn::new(script));
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(
        &mut harness,
        "2,000 rows, 40.0 KB · 3.0 s · 500 rows/s, 10.0 KB/s · waiting on SQLite (92%)",
    );
}

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
                engine: None,
                limits: None,
                partition: None,
            },
            SourceStart {
                id: "accounts".to_owned(),
                data_source: "shop".to_owned(),
                attached: false,
                waits_for: Some("entries".to_owned()),
                engine: None,
                limits: None,
                partition: None,
            },
        ],
        combine: true,
        combine_sql: None,
        version: None,
        combine_limits: None,
        timeout_ms: None,
        machine: None,
        partition: None,
    };
    let progress = |id: &str, batch| RunEvent::Progress {
        step: Step::Source { id: id.to_owned() },
        rows: 1_000,
        bytes: 20_000,
        elapsed_ms: 1_000,
        batch,
        parts: None,
        busy: None,
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
        parts: None,
        phases: None,
        scanned_bytes: None,
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
                    bytes: None,
                })
                .collect(),
            warnings: Vec::new(),
            batches: Some(7),
            parts: None,
            phases: None,
            scanned_bytes: None,
        },
        RunEvent::CombineDone {
            rows: 3,
            bytes: 90,
            first_row_ms: Some(1),
            elapsed_ms: 10,
            peak_memory: None,
            peak_temp_size: None,
            query_ms: None,
            plan: None,
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
            set_in: None,
        },
    )));
    let mut harness = start_with_reports(dir.path(), COMBINED, StandIn::new(script));

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Failed at the combine step.");
    shows(&harness, "failed");
    harness.get_by_label_contains(
        "It stopped at memory_limit = 2 GiB, the default in Settings › Reports, since the \
         report's file doesn't set it.",
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
        set_in: None,
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
        "or set max_rows in the [[source]] \"balances\" of the report's file for this one.",
    );
    harness.get_by_label("Open Settings › Reports").click();
    step_until_visible(&mut harness, "Rows a source may return");
}

/// A default changed in Settings › Reports is saved to `settings.toml`, and the next run's
/// request carries it to the worker.
#[test]
fn sends_the_defaults_settings_sets_with_each_run() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(finished_run(&dir.path().join("out")));
    let requests = Arc::clone(&launcher.requests);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label_contains("Settings…").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Reports")
        .click();
    harness.run();
    harness
        .get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("default: 2 GiB")
        })
        .click();
    harness.run();
    harness.get_by_label("4 GiB").click();
    step_until(&mut harness, "the settings file", |_| {
        std::fs::read_to_string(dir.path().join("settings.toml"))
            .is_ok_and(|text| text.contains("[reports]\nmemory_limit = \"4 GiB\""))
    });

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Finished in 1.2 s");
    let requests = requests.lock().unwrap();
    let [request] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(
        request.defaults.combine.memory_limit,
        dbrover_report::ByteSize(4 << 30)
    );
    assert_eq!(
        request.defaults.source,
        dbrover_report::SourceLimits::default()
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
            engine: None,
            limits: None,
            partition: None,
        }],
        combine: false,
        combine_sql: None,
        version: None,
        combine_limits: None,
        timeout_ms: None,
        machine: None,
        partition: None,
    }]);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    // A run in progress shows a spinner, which repaints every frame: step rather than run.
    step_until_visible(&mut harness, "Reading the source");
    assert!(
        harness.query_by_label("Run").is_none(),
        "Cancel takes Run's place"
    );
    harness.get_by_label("Cancel").click();
    step_until_visible(&mut harness, "Cancelled. No file was written.");
    harness.run();
    assert!(!harness.get_by_label("Run").accesskit_node().is_disabled());
}

/// RS5f: **Pause** stops the run keeping what it finished; once the history lists it,
/// **Resume** carries on from its kept work folder with its values, and **Discard** removes it.
#[test]
fn pause_keeps_a_run_that_resume_carries_on_from() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(vec![RunEvent::Started {
        report: "Trial balance".to_owned(),
        sources: Vec::new(),
        combine: false,
        combine_sql: None,
        version: None,
        combine_limits: None,
        timeout_ms: None,
        machine: None,
        partition: None,
    }]);
    let requests = Arc::clone(&launcher.requests);
    // What the worker would have kept: the folder the stand-in's pause names.
    let kept = dir.path().join("report-runs").join("paused");
    let mut manifest = dbrover_report::Manifest::start(dbrover_report::RunIdentity {
        report: dir.path().join("reports/trial-balance.toml"),
        folder: dir.path().join("out"),
        version: "0123".to_owned(),
        values: [("since", "2026-09-01")]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        dbrover: env!("CARGO_PKG_VERSION").to_owned(),
        data_sources: Vec::new(),
    });
    manifest.ranges.push(dbrover_report::RangeKept {
        number: 1,
        from: "2026-09-01".to_owned(),
        to: "2026-09-02".to_owned(),
        read: 10,
        rows: 1,
        bytes: 10,
        files: Vec::new(),
    });
    manifest.mark_kept();
    manifest.write(&kept).unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);

    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Pause");
    harness.get_by_label("Pause").click();
    step_until_visible(
        &mut harness,
        "Stopped. What it finished is kept: Resume carries on from there.",
    );
    step_until_visible(&mut harness, "Resume");
    harness.get_by_label("Resume").click();
    harness.run_steps(4);
    let sent = requests.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "the run, then the run resuming it");
    assert!(sent[1].resume);
    assert_eq!(sent[1].work, kept);
    assert_eq!(
        sent[1].values.get("since").map(String::as_str),
        Some("2026-09-01"),
        "the stopped run's values"
    );
}

#[test]
fn a_report_with_problems_says_what_they_are_and_cant_run() {
    let dir = tempfile::tempdir().unwrap();
    let broken = TRIAL_BALANCE.replace("data_source = \"shop\"", "data_source = \"books\"");
    let mut harness = start_with_reports(dir.path(), &broken, StandIn::new(Vec::new()));

    harness.get_by_label("Trial balance").click();
    step_until_visible(&mut harness, "1 problem");
    // Listed in the side panel by its line, and underlined in the file.
    harness.get_by_label_contains("there is no data source \"books\"; there are shop");
    assert!(harness.get_by_label("Run").accesskit_node().is_disabled());
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

/// A run of `COMBINED` whose combine step takes most of its second and spills.
fn spilling_run(out: &Path) -> Vec<RunEvent> {
    let mut script = combined_start();
    script.extend([
        RunEvent::CombineDone {
            rows: 2,
            bytes: 40,
            first_row_ms: Some(800),
            elapsed_ms: 903,
            peak_memory: Some(2 << 30),
            peak_temp_size: Some(3_200_000_000),
            query_ms: None,
            plan: None,
        },
        RunEvent::OutputWritten(WrittenFile {
            output: None,
            path: out.join("trial-balance-2026-10.csv"),
            rows: 2,
            size: 48,
            elapsed_ms: 3,
            warnings: Vec::new(),
        }),
        RunEvent::Finished { elapsed_ms: 1_000 },
    ]);
    script
}

#[test]
fn shows_the_runs_side_by_side_with_the_step_that_dominates_and_the_hints() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(spilling_run(&dir.path().join("out")));
    let mut harness = start_with_reports(dir.path(), COMBINED, launcher);
    open_and_fill(&mut harness, "2026-10-01");
    step_until_visible(&mut harness, "No run yet.");

    let finished_runs =
        |harness: &Harness<'_, DbRoverApp>| harness.query_all_by_label("finished").count();
    for run in 1..=2 {
        harness.get_by_label("Run").click();
        step_until_visible(&mut harness, "Last run");
        // A run shows itself in the run panel; the runs kept are a click away.
        harness.get_by_label_contains("Past runs").click();
        step_until(&mut harness, &format!("run {run} listed"), |harness| {
            finished_runs(harness) == run
        });
    }
    assert_eq!(
        harness.query_all_by_label("Source entries").count(),
        1,
        "as a row of the runs"
    );
    harness.get_by_label("Output 1");
    assert_eq!(harness.query_all_by_label("0.9 s · 2 rows").count(), 2);
    harness.get_by_label("Combine takes the most time in every run listed: 90% of the newest.");
    // In the runs' hints, and beside the file once it is checked again with the new run.
    step_until(
        &mut harness,
        "the hint by the runs and by the file",
        |harness| {
            harness
                .query_all_by_label_contains(
                    "Combine spilled 3.2 GB to disk, past its memory_limit.",
                )
                .count()
                == 2
        },
    );

    harness.get_by_label("Clear history").click();
    harness.run();
    harness.get_by_label("Delete").click();
    step_until_visible(&mut harness, "No run yet.");
    let history = HistoryFile::new(dir.path().join("history.sqlite"));
    let report = ReportName::new("trial-balance").unwrap();
    assert!(history.open().unwrap().runs(&report, 5).unwrap().is_empty());
}

/// [`COMBINED`]'s run, timed: `entries` waits four seconds on SQLite before its first row.
fn slow_first_row(out: &Path) -> Vec<TimedEvent> {
    let mut script = combined_start();
    for event in &mut script {
        match event {
            RunEvent::Started { sources, .. } => {
                for source in sources {
                    source.engine = Some("SQLite".to_owned());
                }
            }
            RunEvent::SourceDone {
                id,
                elapsed_ms,
                phases,
                ..
            } if id == "entries" => {
                *elapsed_ms = 4_000;
                *phases = Some(SourcePhases {
                    connect_ms: 10,
                    server_ms: 3_500,
                    transfer_ms: 400,
                    load_ms: 80,
                    type_ms: 10,
                });
            }
            _ => {}
        }
    }
    let at = [0, 4_100, 150];
    let mut timed: Vec<TimedEvent> = script
        .into_iter()
        .zip(at)
        .map(|(event, at_ms)| event.at(Duration::from_millis(at_ms)))
        .collect();
    timed.extend([
        TimedEvent {
            at_ms: 4_600,
            cpu_ms: None,
            event: RunEvent::CombineDone {
                rows: 2,
                bytes: 40,
                first_row_ms: Some(300),
                elapsed_ms: 450,
                peak_memory: None,
                peak_temp_size: None,
                query_ms: Some(400),
                plan: None,
            },
        },
        TimedEvent {
            at_ms: 4_620,
            cpu_ms: None,
            event: RunEvent::OutputWritten(WrittenFile {
                output: None,
                path: out.join("trial-balance-2026-10.csv"),
                rows: 2,
                size: 48,
                elapsed_ms: 20,
                warnings: Vec::new(),
            }),
        },
        TimedEvent {
            at_ms: 4_690,
            cpu_ms: None,
            event: RunEvent::WorkerUsage {
                peak_memory: Some(80 << 20),
            },
        },
        TimedEvent {
            at_ms: 4_700,
            cpu_ms: None,
            event: RunEvent::Finished { elapsed_ms: 4_700 },
        },
    ]);
    timed
}

#[test]
fn the_profile_draws_the_run_and_names_the_step_that_held_it() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::timed(slow_first_row(&dir.path().join("out")));
    let mut harness = start_with_reports(dir.path(), COMBINED, launcher);
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Profile").click();
    step_until_visible(&mut harness, "No run yet. Each run's profile shows here");

    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Last run");
    harness.get_by_label("Profile").click();
    step_until_visible(
        &mut harness,
        "Source entries held the run longest: 4.0 s of 4.7 s, 88% of it waiting on SQLite for \
         its first row.",
    );
    for lane in ["Source entries", "Source accounts", "Combine", "Output 1"] {
        harness.get_by_label(lane);
    }
    harness.get_by_label("Until the first row");
    harness.get_by_label("The worker held at most 83.9 MB in memory.");
}

#[test]
fn compares_two_runs_step_by_step() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(spilling_run(&dir.path().join("out")));
    let mut harness = start_with_reports(dir.path(), COMBINED, launcher);
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Compare").click();
    step_until_visible(&mut harness, "Two runs are needed to compare.");
    for run in 1..=2 {
        harness.get_by_label("Run").click();
        step_until_visible(&mut harness, "Last run");
        harness.get_by_label_contains("Past runs").click();
        step_until(&mut harness, &format!("run {run} listed"), |harness| {
            harness.query_all_by_label("finished").count() == run
        });
    }
    harness.get_by_label("Compare").click();
    step_until_visible(&mut harness, "Before");
    for label in ["After", "Source entries", "Combine", "Output 1"] {
        assert!(
            harness.query_all_by_label(label).next().is_some(),
            "{label}"
        );
    }
    // Two runs of one script: every number is the same.
    assert!(harness.query_all_by_label("same").count() >= 3);
    assert_eq!(harness.query_all_by_label_contains("▲").count(), 0);
}

#[test]
fn a_report_that_silences_a_hint_says_so_instead() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(spilling_run(&dir.path().join("out")));
    let quiet = COMBINED.replace(
        "params = { since = \"date\", until = \"date\" }",
        "params = { since = \"date\", until = \"date\" }\nquiet_hints = [\"combine-spilled\"]",
    );
    let mut harness = start_with_reports(dir.path(), &quiet, launcher);
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Last run");
    harness.get_by_label_contains("Past runs").click();
    step_until(&mut harness, "the run listed", |harness| {
        harness.query_all_by_label("finished").count() == 1
    });
    assert!(
        harness.query_by_label_contains("Combine spilled").is_none(),
        "the hint is silenced"
    );
    harness.get_by_label("1 hint is silenced by the file's quiet_hints.");
}

/// Types `text` where the cursor is in the report's file.
fn type_in_file(harness: &mut Harness<'_, DbRoverApp>, text: &str) {
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run();
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text(text);
    harness.run();
}

#[test]
fn edits_the_file_in_its_tab_then_saves_and_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(finished_run(&dir.path().join("out")));
    let requests = Arc::clone(&launcher.requests);
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, launcher);
    open_and_fill(&mut harness, "2026-10-01");

    type_in_file(&mut harness, "# Checked each month.\n");
    harness.get_by_label("Unsaved");
    harness.get_by_label("Save and run").click();
    step_until_visible(&mut harness, "Finished in 1.2 s: 2 rows read.");

    let file = dir.path().join("reports/trial-balance.toml");
    let saved = std::fs::read_to_string(file).unwrap();
    assert!(saved.contains("# Checked each month."), "{saved}");
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "run once, after the save"
    );
    assert!(
        harness.query_by_label("Save and run").is_none(),
        "nothing unsaved"
    );
}

#[test]
fn keep_mine_then_save_replaces_a_change_made_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));
    open_and_fill(&mut harness, "2026-10-01");
    type_in_file(&mut harness, "# Mine.\n");

    let theirs = TRIAL_BALANCE.replace("Debits and credits", "Credits and debits");
    change_on_disk(dir.path(), &theirs);
    step_until_visible(&mut harness, "Keep mine");
    let file = dir.path().join("reports/trial-balance.toml");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), theirs);

    harness.get_by_label("Keep mine").click();
    harness.run();
    harness.get_by_label("Save").click();
    step_until(&mut harness, "the file saved over", |_| {
        std::fs::read_to_string(&file).is_ok_and(|text| text.contains("# Mine."))
    });
}

#[test]
fn a_runs_hint_shows_by_its_table_and_quiet_writes_it_into_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = StandIn::new(spilling_run(&dir.path().join("out")));
    let mut harness = start_with_reports(dir.path(), COMBINED, launcher);
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the hint by its table", |harness| {
        harness.query_by_label("Quiet").is_some()
    });
    harness.get_by_label("Quiet").click();
    // The text changed, and isn't saved yet.
    step_until_visible(&mut harness, "Save and run");
    let text = harness
        .get_by_role(Role::MultilineTextInput)
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(
        text.contains("quiet_hints = [\"combine-spilled\"]"),
        "{text}"
    );
    // Checked again once typing would have paused.
    step_until(&mut harness, "the hint quiet", |harness| {
        harness.query_by_label("Quiet").is_none()
    });
}

#[test]
fn format_lays_out_the_file_and_its_sql_as_one_unsaved_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));
    open_and_fill(&mut harness, "2026-10-01");
    harness.get_by_label("Format").click();
    step_until_visible(&mut harness, "Save and run");
    let text = harness
        .get_by_role(Role::MultilineTextInput)
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(text.contains("query = \"\"\"\nSELECT\n"), "{text}");
    let saved = std::fs::read_to_string(dir.path().join("reports/trial-balance.toml")).unwrap();
    assert_eq!(saved, TRIAL_BALANCE, "nothing is written until saved");
}

/// Writes `text` to the report's file as another program would, with a later modification
/// time than the tab read.
fn change_on_disk(dir: &Path, text: &str) {
    let file = dir.join("reports/trial-balance.toml");
    std::fs::write(&file, text).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(later)
        .unwrap();
}

fn file_text(harness: &Harness<'_, DbRoverApp>) -> String {
    harness
        .get_by_role(Role::MultilineTextInput)
        .accesskit_node()
        .value()
        .unwrap_or_default()
}

#[test]
fn a_tab_with_nothing_unsaved_follows_its_file_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));
    open_and_fill(&mut harness, "2026-10-01");

    change_on_disk(
        dir.path(),
        &TRIAL_BALANCE.replace("Debits and credits", "Credits and debits"),
    );
    step_until(&mut harness, "the file's change shown", |harness| {
        file_text(harness).contains("Credits and debits")
    });
    assert!(
        harness.query_by_label("Save and run").is_none(),
        "nothing unsaved"
    );
}

#[test]
fn unsaved_text_asks_before_the_file_on_disk_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));
    open_and_fill(&mut harness, "2026-10-01");
    type_in_file(&mut harness, "# Mine.\n");

    change_on_disk(
        dir.path(),
        &TRIAL_BALANCE.replace("Debits and credits", "Credits and debits"),
    );
    step_until_visible(
        &mut harness,
        "trial-balance.toml changed on disk, and you have unsaved changes",
    );
    assert!(
        file_text(&harness).contains("# Mine."),
        "nothing replaced unasked"
    );
    assert!(harness.get_by_label("Save").accesskit_node().is_disabled());

    harness.get_by_label("Reload").click();
    step_until(&mut harness, "the file's text shown", |harness| {
        file_text(harness).contains("Credits and debits")
    });
    assert!(!file_text(&harness).contains("# Mine."));
}

#[test]
fn renaming_a_report_keeps_its_runs() {
    let dir = tempfile::tempdir().unwrap();
    let history = HistoryFile::new(dir.path().join("history.sqlite"));
    let name = |name: &str| ReportName::new(name).unwrap();
    history
        .open()
        .unwrap()
        .record_run(
            &dbrover_store::ReportRun {
                report: name("trial-balance"),
                started_at: jiff::Timestamp::UNIX_EPOCH,
                elapsed: Duration::from_millis(1_200),
                outcome: RunOutcome::Finished,
                params: std::collections::BTreeMap::new(),
                steps: Vec::new(),
                origin: dbrover_store::RunOrigin::Command,
                version: None,
                profile: Default::default(),
                id: None,
            },
            dbrover_store::DEFAULT_RUNS_KEPT,
        )
        .unwrap();
    let mut harness = start_with_reports(dir.path(), TRIAL_BALANCE, StandIn::new(Vec::new()));
    std::fs::write(dir.path().join("reports").join("taken.toml"), "").unwrap();

    harness.get_by_label("Trial balance").click_secondary();
    harness.run();
    harness.get_by_label("Rename…").click();
    harness.run();
    let rename_to = |harness: &mut Harness<'_, DbRoverApp>, to: &str| {
        harness
            .get_by_role_and_label(Role::TextInput, "File name")
            .focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "File name")
            .type_text(to);
        harness.key_press(egui::Key::Enter);
    };
    rename_to(&mut harness, "taken");
    step_until_visible(
        &mut harness,
        "Could not rename: there is already a report named taken",
    );
    rename_to(&mut harness, "monthly-balance");
    step_until(&mut harness, "the dialog closing", |harness| {
        harness.query_by_label("Rename report").is_none()
    });
    // The dialog closes on `ReportRenamed`, which arrives before the two things that follow
    // it: the explorer's new listing, and the history's thread moving the runs. Until the
    // listing arrives, the explorer still shows "Trial balance" as `trial-balance`, which has
    // no runs, so clicking it then waits for "finished" forever. That failed on macOS CI.
    let renamed = name("monthly-balance");
    step_until(
        &mut harness,
        "the renamed report listed, with its runs",
        |harness| {
            let reports = &harness.state().reports;
            reports.find(&renamed).is_some() && reports.runs(&renamed).is_some()
        },
    );

    let reports = dir.path().join("reports");
    assert!(reports.join("monthly-balance.toml").exists());
    assert!(!reports.join("trial-balance.toml").exists());
    let kept = history.open().unwrap();
    assert!(kept.runs(&name("trial-balance"), 5).unwrap().is_empty());
    assert_eq!(kept.runs(&renamed, 5).unwrap().len(), 1);
    step_until_visible(&mut harness, "Trial balance");
    harness.get_by_label("Trial balance").click();
    step_until_visible(&mut harness, "finished");
    // A run `dbrover report run` kept says so.
    harness.get_by_label("from the command line");
}
