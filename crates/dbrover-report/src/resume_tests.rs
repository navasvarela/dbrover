//! Runs that keep their work folder and resume (`docs/reports-at-scale.md`, RS5), on sources in
//! memory: the manifest a run records, the folder it keeps past `resumable_above`, and what a
//! run resuming it reads and writes.

use std::path::Path;

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
