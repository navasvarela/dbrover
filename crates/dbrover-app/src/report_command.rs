//! `dbrover report run`: a report run from the command line, for cron, a scheduled task or a
//! CI job (`docs/reporting.md`, "RP6 — The command line").
//!
//! The command reads and plans the report as the app's tab does, so a wrong parameter or data
//! source stops it before anything starts, then runs it in `dbrover report-worker` through the
//! app's own [`WorkerRun`]: the same guardrails, the same killing at the run's time limit and
//! the same removal of a killed run's staging folder. It prints what happens and exits with a
//! code per outcome ([`Outcome`]).

mod args;
pub(crate) mod history;
mod human;
pub(crate) mod list;
mod outcome;
mod printer;
mod progress;
mod stopping;

use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;

use dbrover_engine::WorkerRun;
use dbrover_report::{Plan, RunEvent, RunFailure, RunRequest, Step, TypedValues};
use dbrover_store::{AppPaths, ReportSettings, RunOrigin, SettingsFile};
use tracing_appender::non_blocking::WorkerGuard;

pub use args::Request;

use self::args::{ListArgs, ReportArg, RunArgs, USAGE};
use self::history::RunHistory;
use self::outcome::Outcome;
use self::stopping::Stopping;
use crate::reports::{self, ProcessLauncher};
use crate::{report_worker, telemetry, version};

/// Does what `request` asks, and returns the process's exit code.
pub fn main(request: Request) -> i32 {
    match request {
        Request::Help => {
            println!("{}\n\n{USAGE}", version::describe());
            Outcome::Finished.code()
        }
        Request::Invalid(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            Outcome::CommandLine.code()
        }
        Request::Run(args) => run(&args).code(),
        Request::List(args) => list(&args).code(),
    }
}

/// Prints the reports in the settings folder's `reports/`.
fn list(args: &ListArgs) -> Outcome {
    let unusable = |heading: &str, problem: String| {
        eprintln!("{heading}\n  {problem}");
        Outcome::Report
    };
    let paths = match report_worker::settings_paths(args.config_dir.as_deref()) {
        Ok(paths) => paths,
        Err(failure) => return unusable("The settings folder can't be used:", failure.message),
    };
    let defaults = match report_settings(&paths) {
        Ok(settings) => reports::limit_defaults(&settings),
        Err(problems) => return unusable("settings.toml can't be read:", problems.join("\n  ")),
    };
    let listed = match list::reports(&paths, &defaults) {
        Ok(listed) => listed,
        Err(error) => {
            let folder = paths.report_files().dir().display().to_string();
            return unusable(&format!("{folder} can't be read:"), error.to_string());
        }
    };
    // As for a run's files: a closed standard output isn't the listing's failure.
    let _ = list::print(&mut io::stdout().lock(), &listed, args.style);
    Outcome::Finished
}

/// A run ready to start: the report planned, the worker's request, and how many of the
/// report's runs the history keeps.
pub(crate) struct Prepared {
    pub(crate) plan: Plan,
    pub(crate) request: RunRequest,
    pub(crate) runs_kept: NonZeroU32,
}

fn run(args: &RunArgs) -> Outcome {
    let mut printer = printer::for_style(args.style, &args.values, args.progress);
    let paths = match report_worker::settings_paths(args.config_dir.as_deref()) {
        Ok(paths) => paths,
        Err(failure) => {
            printer.not_started("The settings folder can't be used:", &[failure.message]);
            return Outcome::Report;
        }
    };
    // Lives until the run ends, so the log file has the last of it.
    let _log = start_logging(&paths);
    // Before the worker can start, so a stop signal from here on cancels the run rather than
    // ending the command with a worker behind it.
    let stopping = Stopping::listen();
    let report = report_path(&paths, &args.report);
    let prepared = match prepare(&paths, &report, &args.values, args.output_dir.as_deref()) {
        Ok(prepared) => prepared,
        Err(problems) => {
            printer.not_started(&format!("{} can't run:", report.display()), &problems);
            return Outcome::Report;
        }
    };
    let mut history = RunHistory::start(
        &paths,
        &report,
        args.values.clone(),
        prepared.runs_kept,
        RunOrigin::Command,
    );
    let outcome = run_worker(&paths, &prepared, &stopping, &mut |event| {
        printer.event(event);
        if let Some(history) = &mut history {
            history.event(event);
        }
    });
    tracing::info!("{} ended: {outcome:?}", report.display());
    if let Some(history) = history {
        keep(history);
    }
    outcome
}

/// Writes the run in the history. One that can't be written is a warning on standard error,
/// in either output style: the run itself ran, and its exit code says how.
fn keep(history: RunHistory) {
    if let Err(problem) = history.keep() {
        tracing::warn!("the run isn't kept in the history: {problem}");
        eprintln!("The run isn't kept in the history: {problem}");
    }
}

fn start_logging(paths: &AppPaths) -> Option<WorkerGuard> {
    telemetry::init_file(&paths.logs_dir(), telemetry::REPORT_LOG_PREFIX)
        .inspect_err(|error| eprintln!("The run isn't logged: {error:#}"))
        .ok()
}

/// The report's file: by its name in `reports/`, or the path given, from the current folder.
fn report_path(paths: &AppPaths, report: &ReportArg) -> PathBuf {
    match report {
        ReportArg::Name(name) => paths.report_files().path(name),
        ReportArg::Path(path) => absolute(path),
    }
}

/// Reads and plans the report under the settings folder's limits, and checks `values` as the
/// worker will, so that what is wrong is said before anything starts. The files go in
/// `output_dir`, else the current folder.
pub(crate) fn prepare(
    paths: &AppPaths,
    report: &Path,
    values: &TypedValues,
    output_dir: Option<&Path>,
) -> Result<Prepared, Vec<String>> {
    let settings = report_settings(paths)?;
    let defaults = reports::limit_defaults(&settings);
    let plan = report_worker::plan(paths, report, &defaults)?;
    let mut problems = unknown_parameters(&plan, values);
    if let Err(more) = plan.values(values) {
        problems.extend(more);
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let folder = output_dir.map_or_else(current_folder, absolute);
    let request = RunRequest {
        report: report.to_owned(),
        values: values.clone(),
        staging: folder.join(reports::staging_name()),
        folder,
        defaults,
        memory_limit: None,
    };
    Ok(Prepared {
        plan,
        request,
        runs_kept: reports::runs_kept(&settings),
    })
}

/// Settings › Reports as `settings.toml` keeps it: the limits' defaults, which the app sends
/// with each run, and the runs kept.
pub(crate) fn report_settings(paths: &AppPaths) -> Result<ReportSettings, Vec<String>> {
    SettingsFile::new(paths.settings_file())
        .load()
        .map(|settings| settings.reports)
        .map_err(|error| vec![dbrover_core::describe_error(&error)])
}

/// A sentence for each value given for a parameter the report doesn't take: a typo there
/// would otherwise leave the parameter meant missing, and say so less clearly.
fn unknown_parameters(plan: &Plan, values: &TypedValues) -> Vec<String> {
    let known: Vec<&str> = plan
        .params
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    let takes = if known.is_empty() {
        "it takes none".to_owned()
    } else {
        format!("it takes {}", known.join(", "))
    };
    values
        .keys()
        .filter(|name| !known.contains(&name.as_str()))
        .map(|name| format!("the report takes no parameter {name:?}; {takes}"))
        .collect()
}

fn current_folder() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `path` from the current folder, which the worker, started from it, would resolve it from
/// too; kept absolute so the paths printed can be used from anywhere.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// Starts `dbrover report-worker` from this executable on the request, as the app does.
pub(crate) fn start_worker(paths: &AppPaths, prepared: &Prepared) -> io::Result<WorkerRun> {
    let launcher = ProcessLauncher {
        executable: std::env::current_exe()?,
        settings_folder: paths.folders().settings.clone(),
    };
    let mut command = launcher.command();
    in_own_process_group(&mut command);
    WorkerRun::start(command, &prepared.request, prepared.plan.timeout, || {})
}

/// On Unix, the worker gets a process group of its own, so a Ctrl-C in the terminal reaches
/// only the command, which cancels the run. In the command's group, the worker would die of
/// the same SIGINT, and the run would end as a worker that stopped without saying why or as
/// cancelled, whichever the command saw first. Windows starts the worker without a console
/// (`CREATE_NO_WINDOW`), which keeps Ctrl-C from it already.
fn in_own_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Starts the worker, hands `on_event` each of the run's events, and returns how it ended.
fn run_worker(
    paths: &AppPaths,
    prepared: &Prepared,
    stopping: &Stopping,
    on_event: &mut dyn FnMut(&RunEvent),
) -> Outcome {
    let run = match start_worker(paths, prepared) {
        Ok(run) => run,
        Err(error) => {
            let failure =
                RunFailure::new(Step::Worker, format!("could not start the worker: {error}"));
            on_event(&RunEvent::Failed(failure));
            return Outcome::Failed;
        }
    };
    stopping.started(run.stopper());
    while let Some(mut event) = run.wait_next() {
        // The worker places its own limits; the run's time limit is enforced from here.
        if let RunEvent::Failed(failure) = &mut event {
            failure.place_limit(&prepared.plan);
        }
        on_event(&event);
        if let Some(outcome) = Outcome::of_end(&event) {
            return outcome;
        }
    }
    // `WorkerRun` always ends a run with an event of its own; this is a bug, not a run's end.
    let failure = RunFailure::new(Step::Worker, "the run's events stopped before it ended");
    on_event(&RunEvent::Failed(failure));
    Outcome::Failed
}
