//! `dbrover report run`: a report run from the command line, for cron, a scheduled task or a
//! CI job (`docs/reporting.md`, "RP6 — The command line").
//!
//! The command reads and plans the report as the app's tab does, so a wrong parameter or data
//! source stops it before anything starts, then runs it in `dbrover report-worker` through the
//! app's own [`WorkerRun`]: the same guardrails, the same killing at the run's time limit and
//! the same removal of a killed run's staging folder. It prints what happens and exits with a
//! code per outcome ([`Outcome`]).

mod args;
mod human;
mod outcome;
mod stopping;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use dbrover_engine::WorkerRun;
use dbrover_report::{LimitDefaults, Plan, RunEvent, RunFailure, RunRequest, Step, TypedValues};
use dbrover_store::{AppPaths, SettingsFile};
use tracing_appender::non_blocking::WorkerGuard;

pub use args::Request;

use self::args::{ReportArg, RunArgs, USAGE};
use self::human::HumanLines;
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
    }
}

/// A run ready to start: the report planned, and the worker's request.
struct Prepared {
    plan: Plan,
    request: RunRequest,
}

fn run(args: &RunArgs) -> Outcome {
    let mut human = HumanLines::new(
        io::stdout().lock(),
        io::stderr().lock(),
        args.values.clone(),
    );
    let paths = match report_worker::settings_paths(args.config_dir.as_deref()) {
        Ok(paths) => paths,
        Err(failure) => {
            human.problems("The settings folder can't be used:", &[failure.message]);
            return Outcome::Report;
        }
    };
    // Lives until the run ends, so the log file has the last of it.
    let _log = start_logging(&paths);
    // Before the worker can start, so a stop signal from here on cancels the run rather than
    // ending the command with a worker behind it.
    let stopping = Stopping::listen();
    let report = report_path(&paths, &args.report);
    let prepared = match prepare(&paths, &report, args) {
        Ok(prepared) => prepared,
        Err(problems) => {
            human.problems(&format!("{} can't run:", report.display()), &problems);
            return Outcome::Report;
        }
    };
    let run = match start_worker(&paths, &prepared) {
        Ok(run) => run,
        Err(error) => {
            let failure =
                RunFailure::new(Step::Worker, format!("could not start the worker: {error}"));
            human.event(&RunEvent::Failed(failure));
            return Outcome::Failed;
        }
    };
    stopping.started(run.stopper());
    let outcome = follow(&run, &mut human);
    tracing::info!("{} ended: {outcome:?}", report.display());
    outcome
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

/// Reads and plans the report under the settings folder's limits, and checks the values as
/// the worker will, so that what is wrong is said before anything starts.
fn prepare(paths: &AppPaths, report: &Path, args: &RunArgs) -> Result<Prepared, Vec<String>> {
    let defaults = limit_defaults(paths)?;
    let plan = report_worker::plan(paths, report, &defaults)?;
    let mut problems = unknown_parameters(&plan, &args.values);
    if let Err(more) = plan.values(&args.values) {
        problems.extend(more);
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let folder = args
        .output_dir
        .as_deref()
        .map_or_else(current_folder, absolute);
    let request = RunRequest {
        report: report.to_owned(),
        values: args.values.clone(),
        staging: folder.join(reports::staging_name()),
        folder,
        defaults,
        memory_limit: None,
    };
    Ok(Prepared { plan, request })
}

/// The limits' defaults as Settings › Reports keeps them in `settings.toml`, as the app sends
/// them with each run.
fn limit_defaults(paths: &AppPaths) -> Result<LimitDefaults, Vec<String>> {
    let settings = SettingsFile::new(paths.settings_file())
        .load()
        .map_err(|error| vec![dbrover_core::describe_error(&error)])?;
    Ok(reports::limit_defaults(&settings.reports))
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
fn start_worker(paths: &AppPaths, prepared: &Prepared) -> io::Result<WorkerRun> {
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

/// Prints each of the run's events, and returns how the run ended.
fn follow(run: &WorkerRun, human: &mut HumanLines<impl Write, impl Write>) -> Outcome {
    while let Some(event) = run.wait_next() {
        human.event(&event);
        if let Some(outcome) = Outcome::of_end(&event) {
            return outcome;
        }
    }
    // `WorkerRun` always ends a run with an event of its own; this is a bug, not a run's end.
    let failure = RunFailure::new(Step::Worker, "the run's events stopped before it ended");
    human.event(&RunEvent::Failed(failure));
    Outcome::Failed
}
