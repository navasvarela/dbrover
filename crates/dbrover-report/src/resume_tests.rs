//! Runs that keep their work folder and resume (`docs/reports-at-scale.md`, RS5), on sources in
//! memory: the manifest a run records, the folder it keeps past `resumable_above`, and what a
//! run resuming it reads and writes.

use std::path::Path;

use dbrover_core::DataSource;

use crate::partition_tests::{EVERY_THOUSAND, RANGE_KEYS, Ranged, by_range, files, typed};
use crate::{
    Manifest, Plan, RunEvent, RunFailure, RunFolders, RunIdentity, RunSummary, WorkKept, run,
};

/// A report by ranges of a thousand keys, a file a range and one appended to, kept past
/// `resumable_above`; its source fails a range that returns more than 1,500 rows.
fn report(resumable_above: &str) -> Plan {
    let outputs = "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"\n\n\
                   [[output]]\nformat = \"csv\"\nfile = \"events-{part_from}.csv\"";
    let partition = format!("{EVERY_THOUSAND}\nresumable_above = \"{resumable_above}\"");
    let text = crate::partition_tests::BY_RANGE
        .replace("{report}", &partition)
        .replace("{outputs}", outputs)
        .replace(
            "AND id < :part_to\"",
            "AND id < :part_to\"\nmax_rows = 1500",
        );
    crate::partition_tests::plan(&text).unwrap_or_else(|problems| panic!("{problems:?}"))
}

/// What a run did.
struct Ran {
    outcome: Result<RunSummary, RunFailure>,
    events: Vec<RunEvent>,
}

/// Runs `plan` over keys 0 to 2,500 with a manifest in `work`, as the worker starts a run.
async fn run_with_manifest(plan: &Plan, opener: &Ranged, folder: &Path, work: &Path) -> Ran {
    let values = plan.values(&typed(&RANGE_KEYS)).unwrap();
    let identity = RunIdentity::of(
        plan,
        Path::new("/reports/events.toml"),
        Path::new("/out"),
        "0123".to_owned(),
        &typed(&RANGE_KEYS),
    );
    Manifest::start(identity).write(work).unwrap();
    let staging = folder.join(".run");
    let mut events = Vec::new();
    let outcome = run(
        plan,
        &values,
        opener,
        RunFolders {
            folder,
            staging: &staging,
            work,
        },
        &mut |event| events.push(event),
    )
    .await;
    assert!(!staging.exists(), "the run leaves no staging folder");
    Ran { outcome, events }
}

fn failing_third() -> Ranged {
    Ranged {
        extra: Some(("2000".to_owned(), 1_200)),
        ..Ranged::default()
    }
}

fn work_kept(events: &[RunEvent]) -> Option<WorkKept> {
    events.iter().find_map(|event| match event {
        RunEvent::WorkKept(kept) => Some(kept.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn records_each_finished_range_and_keeps_the_folder_past_resumable_above() {
    let folder = tempfile::tempdir().unwrap();
    let work = folder.path().join("runs").join("run-1");
    let ran = run_with_manifest(&report("1 KiB"), &failing_third(), folder.path(), &work).await;
    assert!(ran.outcome.is_err());
    let manifest = Manifest::read(&work).unwrap().unwrap();
    let ranges: Vec<(u64, &str, &str)> = manifest
        .ranges
        .iter()
        .map(|range| (range.number, range.from.as_str(), range.to.as_str()))
        .collect();
    assert_eq!(ranges, [(1, "0", "1000"), (2, "1000", "2000")]);
    assert_eq!(manifest.ranges[1].rows, 1_000);
    let files: Vec<String> = manifest.ranges[1]
        .files
        .iter()
        .filter_map(|file| Some(file.path.file_name()?.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(files, ["events-1000.csv"]);
    assert!(manifest.finished_bytes() > 1024);
    assert!(manifest.kept_at().is_some());
    assert_eq!(
        work_kept(&ran.events),
        Some(WorkKept {
            folder: "run-1".to_owned(),
            ranges: 2,
            sources: 0,
            read: manifest.finished_bytes(),
        })
    );
    let ends_with_kept = matches!(ran.events.last(), Some(RunEvent::WorkKept(_)));
    assert!(
        ends_with_kept,
        "kept is the last the run says before its end"
    );
    for kept in ["parts/rows-00001.jsonl", "parts/rows-00002.jsonl"] {
        assert!(work.join(kept).exists(), "{kept}");
    }
}

#[tokio::test]
async fn removes_the_folder_of_a_run_under_resumable_above_or_that_finished() {
    let folder = tempfile::tempdir().unwrap();
    let work = folder.path().join("run-1");
    let ran = run_with_manifest(&report("1 GiB"), &failing_third(), folder.path(), &work).await;
    assert!(ran.outcome.is_err());
    assert!(!work.exists(), "a run that read less isn't worth keeping");
    assert_eq!(work_kept(&ran.events), None);

    let finished = tempfile::tempdir().unwrap();
    let work = finished.path().join("run-2");
    let ran = run_with_manifest(&report("1 KiB"), &Ranged::default(), finished.path(), &work).await;
    ran.outcome.unwrap();
    assert!(!work.exists(), "a run that finished has nothing to resume");
    assert_eq!(
        files(finished.path()),
        [
            "events-0.csv",
            "events-1000.csv",
            "events-2000.csv",
            "events.csv"
        ]
    );
}

#[test]
fn reads_resumable_above_from_the_file_and_settings() {
    assert_eq!(report("2 MiB").resumable_above, crate::ByteSize(2 << 20));
    let defaulted = by_range(
        EVERY_THOUSAND,
        "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"",
    )
    .unwrap();
    assert_eq!(defaulted.resumable_above, crate::DEFAULT_RESUMABLE_ABOVE);
}

/// The report of [`report`], with a Parquet file every range appends to, and a chart.
fn report_with_parquet(resumable_above: &str) -> Plan {
    let outputs = "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"\n\n\
                   [[output]]\nformat = \"csv\"\nfile = \"events-{part_from}.csv\"\n\n\
                   [[output]]\nformat = \"parquet\"\nfile = \"events.parquet\"\n\n\
                   [[chart]]\nfile = \"events.svg\"\nkind = \"lines\"\nx = \"id\"\ny = \"doubled\"";
    let partition = format!("{EVERY_THOUSAND}\nresumable_above = \"{resumable_above}\"");
    let text = crate::partition_tests::BY_RANGE
        .replace("{report}", &partition)
        .replace("{outputs}", outputs)
        .replace(
            "AND id < :part_to\"",
            "AND id < :part_to\"\nmax_rows = 1500",
        );
    crate::partition_tests::plan(&text).unwrap_or_else(|problems| panic!("{problems:?}"))
}

/// The ranges `opener` was sent, by their first key.
fn read_ranges(opener: &Ranged) -> Vec<String> {
    opener
        .sent
        .borrow()
        .iter()
        .filter_map(|sent| sent.from.clone())
        .collect()
}

#[tokio::test]
async fn a_resumed_run_reads_the_unfinished_ranges_only_and_writes_what_an_uninterrupted_run_does()
{
    let plan = report_with_parquet("1 KiB");
    let whole = tempfile::tempdir().unwrap();
    let ran = run_with_manifest(
        &plan,
        &Ranged::default(),
        whole.path(),
        &whole.path().join("run"),
    )
    .await;
    ran.outcome.unwrap();

    let folder = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let work = runs.path().join("run-1");
    let ran = run_with_manifest(&plan, &failing_third(), folder.path(), &work).await;
    assert!(ran.outcome.is_err());
    assert!(work_kept(&ran.events).is_some());

    let identity = RunIdentity::of(
        &plan,
        Path::new("/reports/events.toml"),
        Path::new("/out"),
        "0123".to_owned(),
        &typed(&RANGE_KEYS),
    );
    Manifest::resume(&work, &identity).unwrap();
    // A worker killed in its third range would have left that range's DuckDB behind.
    std::fs::write(work.join("workspace.duckdb"), "left by a killed worker").unwrap();
    let values = plan.values(&typed(&RANGE_KEYS)).unwrap();
    let staging = folder.path().join(".run");
    let opener = Ranged::default();
    let mut events = Vec::new();
    let summary = run(
        &plan,
        &values,
        &opener,
        RunFolders {
            folder: folder.path(),
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await
    .unwrap();
    assert_eq!(
        read_ranges(&opener),
        ["2000"],
        "only the third range is read"
    );
    let resumed = events.iter().find_map(|event| match event {
        RunEvent::Resumed(resumed) => Some(resumed.clone()),
        _ => None,
    });
    let resumed = resumed.unwrap();
    assert_eq!((resumed.ranges, resumed.of), (vec![1, 2], 3));
    assert_eq!(resumed.files.len(), 2);
    let parts: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::PartDone(part) => Some(part.number),
            _ => None,
        })
        .collect();
    assert_eq!(parts, [3]);
    assert_eq!(summary.rows, 2_500, "every range's rows, the kept ones too");
    assert_eq!(summary.files.len(), 6);
    assert!(
        !work.exists(),
        "a resumed run that finished removes its folder"
    );

    assert_eq!(files(folder.path()), files(whole.path()));
    for name in [
        "events.csv",
        "events-0.csv",
        "events-1000.csv",
        "events-2000.csv",
        "events.svg",
    ] {
        assert_eq!(
            std::fs::read(folder.path().join(name)).unwrap(),
            std::fs::read(whole.path().join(name)).unwrap(),
            "{name} as an uninterrupted run wrote it"
        );
    }
    assert_eq!(
        crate::partition_tests::parquet_rows(&folder.path().join("events.parquet")).await,
        crate::partition_tests::parquet_rows(&whole.path().join("events.parquet")).await,
    );
}

#[test]
fn refuses_to_resume_a_run_of_another_report_version_values_or_data_source() {
    let plan = report("1 KiB");
    let identity = |values: &[(&str, &str)]| {
        RunIdentity::of(
            &plan,
            Path::new("/reports/events.toml"),
            Path::new("/out"),
            "0123".to_owned(),
            &typed(values),
        )
    };
    let kept = identity(&RANGE_KEYS);
    let mut other_version = kept.clone();
    other_version.version = "4567".to_owned();
    let mut other_dbrover = kept.clone();
    other_dbrover.dbrover = "0.1.0".to_owned();
    let mut other_target = kept.clone();
    other_target.data_sources[0].target = "Postgres db.example.com:5432/events".to_owned();
    let mut other_report = kept.clone();
    other_report.report = Path::new("/reports/other.toml").to_owned();
    let cases = [
        (identity(&RANGE_KEYS), None),
        (
            identity(&[("first", "0"), ("last", "3000")]),
            Some("last was 2500 when it ran, and is 3000 now"),
        ),
        (other_version, Some("its version was 0123, and is 4567 now")),
        (other_dbrover, Some("it ran on DbRover")),
        (other_target, Some("the source events read")),
        (other_report, Some("it ran /reports/events.toml")),
    ];
    for (now, change) in cases {
        let work = tempfile::tempdir().unwrap();
        let mut manifest = Manifest::start(kept.clone());
        manifest.mark_kept();
        manifest.write(work.path()).unwrap();
        let resumed = Manifest::resume(work.path(), &now);
        match change {
            None => assert!(resumed.unwrap().kept_at.is_none()),
            Some(change) => {
                let changes = resumed.unwrap_err();
                assert!(
                    changes.iter().any(|said| said.contains(change)),
                    "{change}: {changes:?}"
                );
            }
        }
    }
    let running = tempfile::tempdir().unwrap();
    Manifest::start(kept.clone()).write(running.path()).unwrap();
    let refused = Manifest::resume(running.path(), &kept).unwrap_err();
    assert!(refused[0].contains("holds no run to resume"), "{refused:?}");
}

/// Two sources read whole into the combine step's workspace, then joined.
const TWO_SOURCES: &str = r#"
[report]
name = "Two sources"
params = { part_from = "integer", part_to = "integer" }
resumable_above = "1 KiB"

[[source]]
id = "orders"
data_source = "events"
query = "SELECT id, amount FROM events WHERE id >= :part_from AND id < :part_to"

[[source]]
id = "refunds"
data_source = "events"
query = "SELECT id, amount FROM refunds WHERE id >= :part_from AND id < :part_to"

[combine]
query = "SELECT o.id, o.amount + r.amount AS total FROM orders o JOIN refunds r USING (id) ORDER BY o.id"

[[output]]
format = "csv"
file = "totals.csv"
"#;

/// [`TWO_SOURCES`] planned under `defaults`, on `data_sources`.
fn two_sources(defaults: &crate::LimitDefaults, data_sources: &[DataSource]) -> Plan {
    let report = crate::read_report(TWO_SOURCES).unwrap();
    Plan::new(report, data_sources, &[], defaults).unwrap()
}

/// Runs `plan` with keys 0 to 2,000 in `folder`, its work folder `work`.
async fn run_two(plan: &Plan, opener: &Ranged, folder: &Path, work: &Path, resume: bool) -> Ran {
    let values = [("part_from", "0"), ("part_to", "2000")];
    let identity = RunIdentity::of(
        plan,
        Path::new("/reports/two.toml"),
        Path::new("/out"),
        "0123".to_owned(),
        &typed(&values),
    );
    if resume {
        Manifest::resume(work, &identity).unwrap();
    } else {
        Manifest::start(identity).write(work).unwrap();
    }
    let staging = folder.join(".run");
    let mut events = Vec::new();
    let outcome = run(
        plan,
        &plan.values(&typed(&values)).unwrap(),
        opener,
        RunFolders {
            folder,
            staging: &staging,
            work,
        },
        &mut |event| events.push(event),
    )
    .await;
    Ran { outcome, events }
}

/// RS5e: a run that loaded both its sources and failed after, at an output's limit from
/// Settings, keeps its workspace; raised in Settings, the run resumes without reading either
/// source again, and writes what an uninterrupted run writes.
#[tokio::test]
async fn a_resumed_run_takes_the_sources_its_kept_workspace_loaded() {
    let data_sources = crate::partition_tests::data_sources();
    let roomy = crate::LimitDefaults::default();
    let tight = crate::LimitDefaults {
        output: crate::OutputLimits {
            max_size: crate::ByteSize(100),
        },
        ..roomy
    };
    let whole = tempfile::tempdir().unwrap();
    let ran = run_two(
        &two_sources(&roomy, &data_sources),
        &Ranged::default(),
        whole.path(),
        &whole.path().join("work"),
        false,
    )
    .await;
    ran.outcome.unwrap();

    let folder = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let work = runs.path().join("run-1");
    let ran = run_two(
        &two_sources(&tight, &data_sources),
        &Ranged::default(),
        folder.path(),
        &work,
        false,
    )
    .await;
    let failure = ran.outcome.unwrap_err();
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("max_size".to_owned())
    );
    let kept = work_kept(&ran.events).unwrap();
    assert_eq!((kept.ranges, kept.sources), (0, 2));
    assert!(
        work.join("workspace.duckdb").exists(),
        "the workspace is kept"
    );

    let opener = Ranged::default();
    let ran = run_two(
        &two_sources(&roomy, &data_sources),
        &opener,
        folder.path(),
        &work,
        true,
    )
    .await;
    ran.outcome.unwrap();
    assert!(read_ranges(&opener).is_empty(), "no source is read again");
    let resumed = ran.events.iter().find_map(|event| match event {
        RunEvent::Resumed(resumed) => Some(resumed.sources.clone()),
        _ => None,
    });
    assert_eq!(
        resumed,
        Some(vec!["orders".to_owned(), "refunds".to_owned()])
    );
    let sources_done = ran
        .events
        .iter()
        .filter(|event| matches!(event, RunEvent::SourceDone { .. }))
        .count();
    assert_eq!(sources_done, 2, "each kept source still says it is done");
    assert_eq!(
        std::fs::read(folder.path().join("totals.csv")).unwrap(),
        std::fs::read(whole.path().join("totals.csv")).unwrap()
    );
    assert!(!work.exists());
}

/// RS5g: every query may read `:as_of`, the run's start in UTC, which a run resuming it binds
/// again; a report with a parameter of that name binds its own value instead.
#[test]
fn binds_as_of_to_the_runs_start_unless_the_report_names_it() {
    let text = TWO_SOURCES.replace(
        "WHERE id >= :part_from AND id < :part_to\"\n\n[[source]]",
        "WHERE id >= :part_from AND id < :part_to AND :as_of IS NOT NULL\"\n\n[[source]]",
    );
    let data_sources = crate::partition_tests::data_sources();
    let plan = |text: &str| {
        Plan::new(
            crate::read_report(text).unwrap(),
            &data_sources,
            &[],
            &crate::LimitDefaults::default(),
        )
    };
    let planned = plan(&text).unwrap();
    assert!(planned.binds_as_of());
    let mut values = planned
        .values(&typed(&[("part_from", "0"), ("part_to", "10")]))
        .unwrap();
    let now = planned.as_of(&values).unwrap().to_owned();
    assert_eq!(now.len(), "2026-11-12 09:30:00".len(), "{now}");
    planned.resume_as_of(&mut values, "2026-11-01 08:00:00");
    assert_eq!(planned.as_of(&values), Some("2026-11-01 08:00:00"));

    let named = text.replace(
        "params = { part_from = \"integer\", part_to = \"integer\" }",
        "params = { part_from = \"integer\", part_to = \"integer\", as_of = \"timestamp\" }",
    );
    let planned = plan(&named).unwrap();
    assert!(!planned.binds_as_of());
    let mut values = planned
        .values(&typed(&[
            ("part_from", "0"),
            ("part_to", "10"),
            ("as_of", "2025-01-01 00:00:00"),
        ]))
        .unwrap();
    planned.resume_as_of(&mut values, "2026-11-01 08:00:00");
    assert_eq!(
        planned.as_of(&values),
        None,
        "the report's own as_of isn't the run's start"
    );
    assert_eq!(
        values.parameters.get("as_of"),
        Some(&Some("2025-01-01 00:00:00".to_owned())),
        "the report's own value"
    );
}
