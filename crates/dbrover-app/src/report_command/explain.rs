//! `dbrover report explain`: a report planned without running it (`docs/reports-at-scale.md`,
//! RS2d), in `dbrover report-worker` as a run is, so its sources are reached with a run's
//! guards. It prints each source's plan and estimate, and the combine step's plan, as text or
//! one JSON object.

use std::io::{self, Write};
use std::path::Path;

use dbrover_core::describe_error;
use dbrover_report::{ExplainRequest, Explanation, KnownTable, RunEvent};
use dbrover_store::{AppPaths, HistoryFile, ReportName};

use super::args::{ExplainArgs, OutputStyle};
use super::outcome::Outcome;
use super::stopping::Stopping;
use super::{prepare, report_path, run_worker, start_logging};
use crate::report_worker;
use crate::reports::explanation;

/// The runs read to find the newest finished one: enough to pass the failed and cancelled runs
/// a report gathers while its author works on it.
const RUNS_READ: usize = 20;

pub fn explain(args: &ExplainArgs) -> Outcome {
    let paths = match report_worker::settings_paths(args.config_dir.as_deref()) {
        Ok(paths) => paths,
        Err(failure) => {
            eprintln!("The settings folder can't be used:\n  {}", failure.message);
            return Outcome::Report;
        }
    };
    let _log = start_logging(&paths);
    let stopping = Stopping::listen();
    let report = report_path(&paths, &args.report);
    let mut prepared = match prepare(&paths, &report, &args.values, None) {
        Ok(prepared) => prepared,
        Err(problems) => {
            eprintln!("{} can't be explained:", report.display());
            for problem in problems {
                eprintln!("  {problem}");
            }
            return Outcome::Report;
        }
    };
    prepared.request.explain = Some(ExplainRequest {
        tables: known_tables(&paths, &report),
    });
    let mut explained: Option<Explanation> = None;
    let outcome = run_worker(
        &paths,
        &prepared,
        &stopping,
        &mut |timed| match &timed.event {
            RunEvent::Explained(explanation) => explained = Some((**explanation).clone()),
            RunEvent::Failed(failure) => eprintln!("{failure}"),
            RunEvent::Cancelled => eprintln!("Cancelled."),
            _ => {}
        },
    );
    if let Some(explanation) = explained {
        // As for a run's files: a closed standard output isn't the explanation's failure.
        let _ = print(&mut io::stdout().lock(), &explanation, args.style);
    }
    outcome
}

/// The tables the history's newest finished run of the report loaded; none when it keeps no
/// run of it, or can't be read, which leaves the combine unexplained and says why.
fn known_tables(paths: &AppPaths, report: &Path) -> Vec<KnownTable> {
    let Some(name) = report
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(ReportName::new)
    else {
        return Vec::new();
    };
    match HistoryFile::new(paths.history_file())
        .open()
        .and_then(|history| history.runs(&name, RUNS_READ))
    {
        Ok(runs) => explanation::known_tables(&runs),
        Err(error) => {
            tracing::warn!("the history can't be read: {}", describe_error(&error));
            Vec::new()
        }
    }
}

fn print(out: &mut impl Write, explanation: &Explanation, style: OutputStyle) -> io::Result<()> {
    match style {
        OutputStyle::Human => out.write_all(explanation::text(explanation).as_bytes()),
        OutputStyle::JsonLines => {
            let text = serde_json::to_string(explanation).map_err(io::Error::other)?;
            writeln!(out, "{text}")
        }
    }
}
