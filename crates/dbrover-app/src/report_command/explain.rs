//! `dbrover report explain`: a report planned without running it (`docs/reports-at-scale.md`,
//! RS2d), in `dbrover report-worker` as a run is, so its sources are reached with a run's
//! guards. It prints each source's plan and estimate, and the combine step's plan, as text or
//! one JSON object.

use std::io::{self, Write};
use std::path::Path;

use dbrover_core::describe_error;
use dbrover_report::{ExplainRequest, Explanation, RunEvent};
use dbrover_store::{AppPaths, HistoryFile, ReportName, ReportRun};
use serde_json::{Value, json};

use super::args::{ExplainArgs, OutputStyle};
use super::kept;
use super::outcome::Outcome;
use super::stopping::Stopping;
use super::{prepare, report_path, run_worker, start_logging};
use crate::report_worker;
use crate::reports::explanation;
use crate::reports::hints::Hint;

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
    let runs = kept_runs(&paths, &report);
    prepared.request.explain = Some(ExplainRequest {
        tables: explanation::known_tables(&runs),
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
        let quiet = kept::quiet_hints(&report);
        let hints: Vec<Hint> = explanation::hints(&explanation, &runs)
            .into_iter()
            .filter(|hint| !quiet.contains(&hint.id))
            .collect();
        // As for a run's files: a closed standard output isn't the explanation's failure.
        let _ = print(&mut io::stdout().lock(), &explanation, &hints, args.style);
    }
    outcome
}

/// The report's runs the history keeps, newest first: their tables plan the combine step, and
/// their rows weigh its hints. None when it keeps no run of it, or can't be read, which leaves
/// a combine over undeclared columns unexplained, and says why.
fn kept_runs(paths: &AppPaths, report: &Path) -> Vec<ReportRun> {
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
        Ok(runs) => runs,
        Err(error) => {
            tracing::warn!("the history can't be read: {}", describe_error(&error));
            Vec::new()
        }
    }
}

fn print(
    out: &mut impl Write,
    explanation: &Explanation,
    hints: &[Hint],
    style: OutputStyle,
) -> io::Result<()> {
    match style {
        OutputStyle::Human => out.write_all(explanation::text(explanation, hints).as_bytes()),
        OutputStyle::JsonLines => {
            let mut value = serde_json::to_value(explanation).map_err(io::Error::other)?;
            let hints: Vec<Value> = hints
                .iter()
                .map(|hint| {
                    json!({
                        "id": hint.id.as_str(),
                        "at": kept::hint_at(&hint.at),
                        "text": hint.text,
                    })
                })
                .collect();
            if let Value::Object(object) = &mut value {
                object.insert("hints".to_owned(), Value::Array(hints));
            }
            writeln!(out, "{value}")
        }
    }
}
