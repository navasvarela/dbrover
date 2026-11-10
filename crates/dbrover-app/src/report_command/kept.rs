//! What the history keeps of a report's runs, on the command line (`docs/reports-at-scale.md`,
//! "Where it shows", RS1f): `dbrover report runs` lists them, `dbrover report compare` sets
//! two side by side, and `dbrover report run --profile` writes the run that just ended. All
//! read the runs the app keeps, and say them as its Profile and Compare do.

use std::io::{self, Write};
use std::path::Path;

use dbrover_core::describe_error;
use dbrover_report::{HintId, read_report};
use dbrover_store::{AppPaths, HistoryFile, ReportName, ReportRun, RunId, RunOrigin, RunOutcome};
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use super::args::{CompareArgs, OutputStyle, ReportArg, RunsArgs};
use super::history::listed_name;
use super::outcome::Outcome;
use crate::report_worker;
use crate::reports::compare::{self, Change, Comparison, Differs};
use crate::reports::hints::{self, HintAt};
use crate::reports::insights::seconds;
use crate::reports::profile;

/// The most runs read back: more than Settings › Reports lets the history keep.
const RUNS_READ: usize = 10_000;

/// How a run's start is written, in the system's time zone.
const STARTED_FORMAT: &str = "%Y-%m-%d %H:%M";

/// The format of the profile `--profile` writes and `runs --json` prints, for scripts that
/// read it; a change that removes or renames a field raises it.
const PROFILE_FORMAT: u32 = 1;

/// `dbrover report runs`.
pub fn runs(args: &RunsArgs) -> Outcome {
    let kept = match Kept::read(&args.report, args.config_dir.as_deref()) {
        Ok(kept) => kept,
        Err(problem) => return problem.report(),
    };
    let mut out = io::stdout().lock();
    // A closed standard output isn't the listing's failure.
    let _ = match args.style {
        OutputStyle::Human => print_runs(&mut out, &kept.runs),
        OutputStyle::JsonLines => kept
            .runs
            .iter()
            .try_for_each(|run| writeln!(out, "{}", profile_json(run, &kept.quiet))),
    };
    Outcome::Finished
}

/// `dbrover report compare`.
pub fn compare(args: &CompareArgs) -> Outcome {
    let kept = match Kept::read(&args.report, args.config_dir.as_deref()) {
        Ok(kept) => kept,
        Err(problem) => return problem.report(),
    };
    let find = |id: RunId| {
        kept.runs
            .iter()
            .find(|run| run.id == Some(id))
            .ok_or_else(|| {
                Problem(format!(
                    "{} keeps no run {id}; dbrover report runs {} lists the runs it keeps",
                    kept.name, kept.name
                ))
            })
    };
    let (before, after) = match (find(args.before), find(args.after)) {
        (Ok(before), Ok(after)) => (before, after),
        (Err(problem), _) | (_, Err(problem)) => return problem.report(),
    };
    let comparison = compare::compare(before, after);
    let mut out = io::stdout().lock();
    let _ = match args.style {
        OutputStyle::Human => print_comparison(&mut out, before, after, &comparison),
        OutputStyle::JsonLines => writeln!(out, "{}", comparison_json(before, after, &comparison)),
    };
    Outcome::Finished
}

/// Writes `run`'s profile to `file`, for `--profile`. `report` is the report's file, whose
/// `quiet_hints` the hints are marked by.
///
/// # Errors
///
/// When the file can't be written.
pub fn write_profile(file: &Path, run: &ReportRun, report: &Path) -> io::Result<()> {
    let quiet = quiet_hints(report);
    let text =
        serde_json::to_string_pretty(&profile_json(run, &quiet)).map_err(io::Error::other)?;
    std::fs::write(file, text + "\n")
}

/// Why the runs can't be read, as the command says it.
struct Problem(String);

impl Problem {
    fn report(self) -> Outcome {
        eprintln!("{}", self.0);
        Outcome::Report
    }
}

/// A report's kept runs, newest first, and the hints its file silences.
struct Kept {
    name: ReportName,
    runs: Vec<ReportRun>,
    quiet: Vec<HintId>,
}

impl Kept {
    fn read(report: &ReportArg, config_dir: Option<&Path>) -> Result<Self, Problem> {
        let paths = report_worker::settings_paths(config_dir).map_err(|failure| {
            Problem(format!(
                "The settings folder can't be used: {}",
                failure.message
            ))
        })?;
        let name = listed(&paths, report)?;
        let runs = HistoryFile::new(paths.history_file())
            .open()
            .and_then(|history| history.runs(&name, RUNS_READ))
            .map_err(|error| {
                Problem(format!(
                    "The history can't be read: {}",
                    describe_error(&error)
                ))
            })?;
        let quiet = quiet_hints(&paths.report_files().path(&name));
        Ok(Self { name, runs, quiet })
    }
}

/// The name the history keeps `report`'s runs under: only a report in the reports folder has
/// runs kept.
fn listed(paths: &AppPaths, report: &ReportArg) -> Result<ReportName, Problem> {
    match report {
        ReportArg::Name(name) => Ok(name.clone()),
        ReportArg::Path(path) => {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            listed_name(paths.report_files().dir(), &path).ok_or_else(|| {
                Problem(format!(
                    "{} isn't in the reports folder, {}, and only a report there has its runs \
                     kept",
                    path.display(),
                    paths.report_files().dir().display()
                ))
            })
        }
    }
}

/// The hints `report`'s file silences; none when it can't be read.
pub(super) fn quiet_hints(report: &Path) -> Vec<HintId> {
    std::fs::read_to_string(report)
        .ok()
        .and_then(|text| read_report(&text).ok())
        .map(|report| report.quiet_hints)
        .unwrap_or_default()
}

fn started(run: &ReportRun) -> String {
    run.started_at
        .to_zoned(TimeZone::system())
        .strftime(STARTED_FORMAT)
        .to_string()
}

fn millis(run: &ReportRun) -> u64 {
    u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn outcome(run: &ReportRun) -> &'static str {
    match run.outcome {
        RunOutcome::Finished => "finished",
        RunOutcome::Failed(_) => "failed",
        RunOutcome::Cancelled => "cancelled",
    }
}

fn origin(run: &ReportRun) -> &'static str {
    match run.origin {
        RunOrigin::App => "app",
        RunOrigin::Command => "command",
        RunOrigin::Mcp => "mcp",
    }
}

fn id_text(run: &ReportRun) -> String {
    run.id.map_or_else(|| "-".to_owned(), |id| id.to_string())
}

/// A run a line, and under it the step that held it longest.
fn print_runs(out: &mut impl Write, runs: &[ReportRun]) -> io::Result<()> {
    if runs.is_empty() {
        return writeln!(out, "No run is kept.");
    }
    for run in runs {
        let version = run.version.map_or_else(String::new, |version| {
            format!(", version {}", version.short())
        });
        writeln!(
            out,
            "{}  {}  {} in {}, from the {}{version}",
            id_text(run),
            started(run),
            outcome(run),
            seconds(millis(run)),
            match run.origin {
                RunOrigin::App => "app",
                RunOrigin::Command => "command line",
                RunOrigin::Mcp => "MCP",
            }
        )?;
        if let RunOutcome::Failed(error) = &run.outcome {
            writeln!(out, "    Failed at {}: {}", error.step, error.message)?;
        }
        if let Some(bottleneck) = profile::bottleneck(run) {
            writeln!(out, "    {bottleneck}")?;
        }
    }
    Ok(())
}

fn print_comparison(
    out: &mut impl Write,
    before: &ReportRun,
    after: &ReportRun,
    comparison: &Comparison,
) -> io::Result<()> {
    writeln!(
        out,
        "Run {} ({}) and run {} ({})",
        id_text(before),
        started(before),
        id_text(after),
        started(after)
    )?;
    match comparison.file_changed {
        Some(true) => writeln!(out, "The report's file changed between the two runs.")?,
        Some(false) => writeln!(out, "Both ran the same report file.")?,
        None => {}
    }
    let rows: Vec<(String, &Change)> = comparison
        .run
        .iter()
        .map(|change| ("Run".to_owned(), change))
        .chain(comparison.steps.iter().flat_map(|step| {
            step.changes
                .iter()
                .map(|change| (step.step.to_string(), change))
        }))
        .collect();
    let width = rows.iter().map(|(step, _)| step.len()).max().unwrap_or(0);
    let mut last_step = "";
    for (step, change) in &rows {
        let shown = if step == last_step { "" } else { step.as_str() };
        last_step = step;
        let mark = if change.marked() { "  !" } else { "" };
        writeln!(
            out,
            "{shown:width$}  {:<12} {:>12} → {:<12} {}{mark}",
            change.what,
            change.before_text(),
            change.after_text(),
            change.describe_ratio()
        )?;
    }
    for (heading, differs) in [
        ("Parameters that differ:", &comparison.params),
        ("Limits that differ:", &comparison.limits),
    ] {
        if differs.is_empty() {
            continue;
        }
        writeln!(out, "{heading}")?;
        for differ in differs {
            writeln!(
                out,
                "  {}: {} → {}",
                differ.name,
                differ.before.as_deref().unwrap_or("—"),
                differ.after.as_deref().unwrap_or("—")
            )?;
        }
    }
    Ok(())
}

/// A run's whole profile, as `--profile` writes it and `runs --json` prints it a line each.
pub fn profile_json(run: &ReportRun, quiet: &[HintId]) -> Value {
    let hints: Vec<Value> = hints::hints(std::slice::from_ref(run))
        .into_iter()
        .map(|hint| {
            json!({
                "id": hint.id.as_str(),
                "at": hint_at(&hint.at),
                "text": hint.text,
                "quiet": quiet.contains(&hint.id),
            })
        })
        .collect();
    let (failed_at, failure) = match &run.outcome {
        RunOutcome::Failed(error) => (Some(error.step.clone()), Some(error.message.clone())),
        RunOutcome::Finished | RunOutcome::Cancelled => (None, None),
    };
    json!({
        "format": PROFILE_FORMAT,
        "id": run.id,
        "report": run.report.as_str(),
        "started_at": run.started_at.to_string(),
        "elapsed_ms": millis(run),
        "outcome": outcome(run),
        "failed_at": failed_at,
        "failure": failure,
        "origin": origin(run),
        "params": run.params,
        "version": run.version.map(|version| version.to_string()),
        "machine": run.profile,
        "steps": run.steps,
        "bottleneck": profile::bottleneck(run).map(|bottleneck| bottleneck.to_string()),
        "hints": hints,
    })
}

pub(super) fn hint_at(at: &HintAt) -> String {
    match at {
        HintAt::Report => "report".to_owned(),
        HintAt::Source(id) => format!("source {id}"),
        HintAt::Combine => "combine".to_owned(),
        HintAt::Output(number) => format!("output {number}"),
    }
}

fn change_json(change: &Change) -> Value {
    json!({
        "what": change.what,
        "before": change.before,
        "after": change.after,
        "change": change.ratio(),
        "marked": change.marked(),
    })
}

fn differs_json(differs: &[Differs]) -> Vec<Value> {
    differs
        .iter()
        .map(|differ| json!({"name": differ.name, "before": differ.before, "after": differ.after}))
        .collect()
}

fn comparison_json(before: &ReportRun, after: &ReportRun, comparison: &Comparison) -> Value {
    json!({
        "before": before.id,
        "after": after.id,
        "file_changed": comparison.file_changed,
        "run": comparison.run.iter().map(change_json).collect::<Vec<_>>(),
        "steps": comparison.steps.iter().map(|step| json!({
            "step": step.step.to_string(),
            "changes": step.changes.iter().map(change_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "params": differs_json(&comparison.params),
        "limits": differs_json(&comparison.limits),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::BlobId;
    use dbrover_store::{SourceMetrics, SourcePhases, StepMetrics};
    use jiff::Timestamp;

    use super::*;

    fn run(id: i64, elapsed_ms: u64, server_ms: u64) -> ReportRun {
        ReportRun {
            id: Some(RunId(id)),
            report: ReportName::new("valuation").unwrap(),
            started_at: Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_millis(elapsed_ms + 100),
            outcome: RunOutcome::Finished,
            params: [("since".to_owned(), "2026-10-01".to_owned())].into(),
            steps: vec![StepMetrics::Source(SourceMetrics {
                id: "tx".to_owned(),
                data_source: "ledger".to_owned(),
                rows: 10,
                bytes: 100,
                first_row_ms: Some(server_ms),
                elapsed_ms,
                batches: None,
                started_ms: Some(0),
                engine: Some("PostgreSQL".to_owned()),
                waits_for: None,
                phases: Some(SourcePhases {
                    connect_ms: 0,
                    server_ms,
                    transfer_ms: elapsed_ms - server_ms,
                    load_ms: 0,
                    type_ms: 0,
                }),
                limits: None,
                columns: Vec::new(),
                scanned_bytes: None,
            })],
            origin: RunOrigin::Command,
            version: Some(BlobId::of(b"[report]\n")),
            profile: Default::default(),
        }
    }

    #[test]
    fn lists_each_run_with_the_step_that_held_it() {
        let mut out = Vec::new();
        print_runs(&mut out, &[run(12, 9_000, 8_000)]).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("12  ")
                && lines[0].contains("finished in 9.1 s, from the command line, version "),
            "{text}"
        );
        assert_eq!(
            lines[1],
            "    Source tx held the run longest: 9.0 s of 9.1 s, 89% of it waiting on PostgreSQL \
             for its first row."
        );
    }

    #[test]
    fn writes_a_profile_with_its_hints_and_whether_the_file_silences_them() {
        let slow = run(12, 9_000, 8_000);
        let profile = profile_json(&slow, &[HintId::SlowFirstRow]);
        assert_eq!(profile["format"], 1);
        assert_eq!(profile["id"], 12);
        assert_eq!(profile["steps"][0]["phases"]["server_ms"], 8_000);
        assert_eq!(profile["hints"][0]["id"], "slow-first-row");
        assert_eq!(profile["hints"][0]["at"], "source tx");
        assert_eq!(profile["hints"][0]["quiet"], true);
    }

    #[test]
    fn prints_two_runs_side_by_side_with_large_changes_marked() {
        let (before, after) = (run(11, 1_000, 500), run(12, 3_000, 2_500));
        let mut out = Vec::new();
        print_comparison(
            &mut out,
            &before,
            &after,
            &compare::compare(&before, &after),
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Both ran the same report file."), "{text}");
        let time = text
            .lines()
            .find(|line| line.starts_with("Source tx"))
            .unwrap_or_default();
        assert!(
            time.contains("1.0 s → 3.0 s") && time.ends_with("+200%  !"),
            "{text}"
        );
    }
}
