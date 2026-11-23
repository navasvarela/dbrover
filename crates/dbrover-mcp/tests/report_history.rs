//! The read-only report tools, driven as a client would (RS8c, RS8d, RS8e): what a report's
//! kept runs say, read by the server from the history DbRover already keeps; two runs
//! compared; a report planned without running it; and every one of them refused for a report
//! the project isn't shown.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use dbrover_core::{BlobId, ConnectionConfig, ProjectId, SqliteConfig};
use dbrover_store::{
    CombineMetrics, HistoryFile, OutputMetrics, Project, ProjectSlug, Projects, ReportName,
    ReportRun, RunOrigin, RunOutcome, RunProfile, SourceMetrics, SourcePhases, StepMetrics,
};
use jiff::Timestamp;
use serde_json::json;
use support::{SLUG, Server, data_source, failure, ok};

fn main() {
    support::run("report_history", answers_what_a_reports_runs_measured);
}

/// A source that read `rows` rows in `elapsed_ms`, `server_ms` of it waiting on its engine.
fn source(id: &str, rows: u64, elapsed_ms: u64, server_ms: u64) -> SourceMetrics {
    SourceMetrics {
        id: id.to_owned(),
        data_source: "lake".to_owned(),
        rows,
        bytes: rows * 20,
        first_row_ms: Some(server_ms),
        elapsed_ms,
        batches: None,
        started_ms: Some(0),
        engine: Some("SQLite".to_owned()),
        waits_for: None,
        phases: Some(SourcePhases {
            connect_ms: 0,
            server_ms,
            transfer_ms: elapsed_ms.saturating_sub(server_ms),
            load_ms: 0,
            type_ms: 0,
        }),
        limits: None,
        columns: Vec::new(),
        scanned_bytes: None,
        parts: Vec::new(),
        handed_over: None,
    }
}

fn combine(rows: u64, elapsed_ms: u64) -> CombineMetrics {
    CombineMetrics {
        rows,
        bytes: rows * 30,
        first_row_ms: Some(1),
        elapsed_ms,
        peak_memory: Some(1 << 28),
        peak_temp_size: Some(0),
        started_ms: Some(0),
        query_ms: None,
        limits: None,
        plan: None,
        parts: Vec::new(),
    }
}

/// A finished run of `report` whose one source read 200,000 rows for the 12 the output writes,
/// which is `source-reads-more`'s case, and whose source spent `server_ms` of its time before
/// its first row, which is `slow-first-row`'s.
fn run(
    report: &str,
    started_at: Timestamp,
    elapsed_ms: u64,
    server_ms: u64,
    file: &str,
) -> ReportRun {
    ReportRun {
        id: None,
        report: ReportName::new(report).unwrap(),
        started_at,
        elapsed: Duration::from_millis(elapsed_ms),
        outcome: RunOutcome::Finished,
        params: BTreeMap::from([("since".to_owned(), "2026-10-01".to_owned())]),
        steps: vec![
            StepMetrics::Source(source("lines", 200_000, elapsed_ms - 200, server_ms)),
            StepMetrics::Combine(combine(12, 100)),
            StepMetrics::Output(OutputMetrics {
                number: 1,
                file: "sales.csv".to_owned(),
                rows: 12,
                size: 360,
                elapsed_ms: 20,
                started_ms: None,
                ended_ms: None,
            }),
        ],
        origin: RunOrigin::Command,
        version: Some(BlobId::of(file.as_bytes())),
        profile: RunProfile {
            reports: Vec::new(),
            peak_memory: Some(300 << 20),
            cores: Some(8),
            dbrover: Some("0.34.0".to_owned()),
            ..RunProfile::default()
        },
    }
}

/// Keeps three runs of `sales` in the history the server will read, newest last written, and
/// gives back their ids in the order they were kept.
fn keep_runs(dir: &Path) -> Vec<i64> {
    let history = HistoryFile::new(dir.join("history.sqlite")).open().unwrap();
    let kept = NonZeroU32::new(50).unwrap();
    let base = Timestamp::constant(1_762_000_000, 0);
    // The newest run ran a changed report file, which is what `file_changed` reports.
    for (minutes, elapsed_ms, server_ms, file) in [
        (0, 4_000, 100, "first"),
        (10, 4_200, 120, "first"),
        (20, 30_000, 28_000, "edited"),
    ] {
        let started = base + Duration::from_secs(minutes * 60);
        history
            .record_run(&run("sales", started, elapsed_ms, server_ms, file), kept)
            .unwrap();
    }
    history
        .runs(&ReportName::new("sales").unwrap(), 50)
        .unwrap()
        .iter()
        .map(|run| run.id.unwrap().0)
        .collect()
}

async fn answers_what_a_reports_runs_measured() {
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let path = dir.path().join("lake.db");
    rusqlite::Connection::open(&path).unwrap();
    let lake = data_source(
        "lake",
        project_id,
        ConnectionConfig::Sqlite(SqliteConfig {
            path,
            read_only: false,
        }),
    );
    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    support::write_files(
        dir.path(),
        &[lake],
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &HashMap::new(),
    );
    // Newest first, as the history answers and the tools do.
    let ids = keep_runs(dir.path());

    let server = Server::start(dir.path()).await;
    let client = &server.client;

    // Both tools are there for a project that may not *run* a report: reading what its runs
    // measured is not running one.
    let tools = server.tools().await;
    assert!(tools.contains(&"report_runs".to_owned()), "{tools:?}");
    assert!(tools.contains(&"report_profile".to_owned()), "{tools:?}");
    assert!(!tools.contains(&"run_report".to_owned()), "{tools:?}");

    let listed = ok(client, "report_runs", json!({"report": "sales"})).await;
    assert_eq!(listed["kept"], 3, "{listed}");
    assert_eq!(listed["runs"].as_array().unwrap().len(), 3, "{listed}");
    assert_eq!(listed["runs"][0]["id"], ids[0], "newest first: {listed}");
    assert_eq!(listed["runs"][0]["outcome"], "finished", "{listed}");
    assert_eq!(listed["runs"][0]["origin"], "command", "{listed}");
    assert_eq!(listed["runs"][0]["params"]["since"], "2026-10-01");
    assert_eq!(listed["quiet_hints"][0], "source-reads-more", "{listed}");
    // The 30-second run's source took far longer than its median over the three, and the
    // source is what dominates every one of them.
    assert_eq!(
        listed["runs"][0]["slower_than_usual"][0]["step"], "Source lines",
        "{listed}"
    );
    assert_eq!(
        listed["dominant_throughout"]["step"], "Source lines",
        "{listed}"
    );
    assert!(
        listed["runs"][1]["slower_than_usual"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{listed}"
    );

    // `limit` takes fewer, and is clamped rather than refused.
    let one = ok(
        client,
        "report_runs",
        json!({"report": "sales", "limit": 1}),
    )
    .await;
    assert_eq!(one["runs"].as_array().unwrap().len(), 1, "{one}");
    assert_eq!(one["kept"], 3, "the count is of what is kept: {one}");
    let clamped = ok(
        client,
        "report_runs",
        json!({"report": "sales", "limit": 10_000}),
    )
    .await;
    assert_eq!(clamped["runs"].as_array().unwrap().len(), 3, "{clamped}");

    // The newest run by default, and any run by its id.
    let newest = ok(client, "report_profile", json!({"report": "sales"})).await;
    assert_eq!(newest["id"], ids[0], "{newest}");
    assert_eq!(newest["format"], 1, "{newest}");
    assert_eq!(newest["machine"]["cores"], 8, "{newest}");
    assert_eq!(newest["steps"].as_array().unwrap().len(), 3, "{newest}");
    assert!(
        newest["bottleneck"].as_str().unwrap().contains("lines"),
        "{newest}"
    );
    let hints: Vec<&str> = newest["hints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hint| hint["id"].as_str().unwrap())
        .collect();
    assert!(hints.contains(&"source-reads-more"), "{newest}");
    assert!(hints.contains(&"slow-first-row"), "{newest}");
    // Silenced by the report's `quiet_hints`, and answered marked rather than left out.
    let silenced = newest["hints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hint| hint["id"] == "source-reads-more")
        .unwrap();
    assert_eq!(silenced["quiet"], true, "{silenced}");
    assert_eq!(silenced["at"], "source lines", "{silenced}");

    // The funnel is the point of the profile for an assistant: 200,000 rows read, 12 written.
    assert_eq!(
        newest["funnel"]["sources"][0]["source"], "lines",
        "{newest}"
    );
    assert_eq!(
        newest["funnel"]["sources"][0]["returned_rows"], 200_000,
        "{newest}"
    );
    assert_eq!(newest["funnel"]["written_rows"], 12, "{newest}");

    let older = ok(
        client,
        "report_profile",
        json!({"report": "sales", "run": ids[2]}),
    )
    .await;
    assert_eq!(older["id"], ids[2], "{older}");

    // A run the history doesn't have says how many it has, so a client lists them again.
    let missing = failure(
        client,
        "report_profile",
        json!({"report": "sales", "run": 999_999}),
    )
    .await;
    assert_eq!(missing["kind"], "invalid_arguments", "{missing}");
    assert!(
        missing["message"]
            .as_str()
            .is_some_and(|message| message.contains("no kept run 999999")),
        "{missing}"
    );

    // A report with no run yet lists none, and has no profile to answer with.
    let none = ok(client, "report_runs", json!({"report": "broken"})).await;
    assert_eq!(none["kept"], 0, "{none}");
    assert!(none["runs"].as_array().unwrap().is_empty(), "{none}");
    let never = failure(client, "report_profile", json!({"report": "broken"})).await;
    assert_eq!(never["kind"], "invalid_arguments", "{never}");
    assert!(
        never["message"]
            .as_str()
            .is_some_and(|message| message.contains("has kept no run yet")),
        "{never}"
    );

    // A report of another project is refused in `run_report`'s words, naming only the reports
    // this project is shown, so the answer never says whether `elsewhere` exists.
    for (tool, args) in [
        ("report_runs", json!({"report": "elsewhere"})),
        ("report_profile", json!({"report": "elsewhere"})),
        (
            "compare_report_runs",
            json!({"report": "elsewhere", "before": 1, "after": 2}),
        ),
        (
            "explain_report",
            json!({"report": "elsewhere", "params": {"since": "2026-10-01"}}),
        ),
    ] {
        let refused = failure(client, tool, args).await;
        let message = refused["message"].as_str().unwrap();
        assert!(message.contains("has no report \"elsewhere\""), "{refused}");
        assert!(message.contains("sales"), "{refused}");
        assert!(!message.contains("warehouse"), "{refused}");
        // The reports it is shown, never the data source the asked-for report reads, nor
        // anything that says whether that report exists at all.
        assert!(!message.contains("no reports at all"), "{refused}");
    }

    assert!(
        tools.contains(&"compare_report_runs".to_owned()),
        "{tools:?}"
    );
    // The newest run took 30 s against the oldest's 4 s, and its source held the difference.
    // The run ids are the pair the client is given, and the changes are `compare`'s own, which
    // the tab's Compare and `dbrover report compare` show from the same function.
    let compared = ok(
        client,
        "compare_report_runs",
        json!({"report": "sales", "before": ids[2], "after": ids[0]}),
    )
    .await;
    assert_eq!(compared["before"], ids[2], "{compared}");
    assert_eq!(compared["after"], ids[0], "{compared}");
    assert_eq!(
        compared["file_changed"], true,
        "the file was edited: {compared}"
    );
    let elapsed = compared["run"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["what"] == "time")
        .unwrap();
    assert_eq!(elapsed["before"], 4_000, "{compared}");
    assert_eq!(elapsed["after"], 30_000, "{compared}");
    assert_eq!(elapsed["marked"], true, "past half again: {compared}");
    let source = compared["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["step"] == "Source lines")
        .unwrap();
    assert!(
        source["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["what"] == "time" && change["marked"] == true),
        "{compared}"
    );
    // Both runs ran for the same `since`, so nothing differs in the parameters.
    assert!(
        compared["params"].as_array().unwrap().is_empty(),
        "{compared}"
    );

    let unchanged = ok(
        client,
        "compare_report_runs",
        json!({"report": "sales", "before": ids[2], "after": ids[1]}),
    )
    .await;
    assert_eq!(
        unchanged["file_changed"], false,
        "the same file ran: {unchanged}"
    );

    // Explaining is a read: it is offered though this project may not run reports, and the
    // estimate over the data source's limit is reported rather than enforced — the refusal it
    // describes is the one `run_report` would give, which is what the client is being told.
    assert!(tools.contains(&"explain_report".to_owned()), "{tools:?}");
    let explained = ok(
        client,
        "explain_report",
        json!({"report": "sales", "params": {"since": "2026-10-01"}}),
    )
    .await;
    assert_eq!(explained["sources"][0]["id"], "lines", "{explained}");
    assert!(
        explained["sources"][0]["statement"]
            .as_str()
            .unwrap()
            .contains("2026-10-01"),
        "the parameters are written into the statement: {explained}"
    );
    assert_eq!(
        explained["sources"][0]["plan"]["kind"], "text",
        "{explained}"
    );
    assert_eq!(
        explained["sources"][0]["estimate"]["bytes"], 2_000_000_000_u64,
        "{explained}"
    );
    assert_eq!(
        explained["sources"][0]["estimate"]["limit_bytes"], 1_000_000_000_u64,
        "the limit comes beside the estimate: {explained}"
    );
    assert_eq!(
        explained["sources"][0]["estimate"]["refused"], true,
        "{explained}"
    );
    assert_eq!(explained["combine"]["kind"], "planned", "{explained}");
    assert!(explained["hints"].is_array(), "{explained}");
    // A report whose parameters are missing is the client's mistake to fix, not a refusal.
    let without = failure(client, "explain_report", json!({"report": "sales"})).await;
    assert_eq!(without["kind"], "invalid_arguments", "{without}");

    // A run id of another report's, or of none, is refused the same way a profile's is.
    let missing = failure(
        client,
        "compare_report_runs",
        json!({"report": "sales", "before": ids[0], "after": 999_999}),
    )
    .await;
    assert_eq!(missing["kind"], "invalid_arguments", "{missing}");
    server.stop().await;
}
