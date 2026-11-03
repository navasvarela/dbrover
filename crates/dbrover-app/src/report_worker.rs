//! `dbrover report-worker`: runs one report for the app in a process of its own, so a report
//! that runs out of memory or time is killed without touching the app, its consoles or their
//! transactions (`docs/reporting.md`, "The worker in RP1").
//!
//! It reads one [`RunRequest`] from standard input, writes [`RunEvent`]s to standard output as
//! JSON Lines, ending with [`RunEvent::Finished`] or [`RunEvent::Failed`], logs to standard
//! error, and exits. Nothing else may write to standard output.

mod limits;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use dbrover_core::describe_error;
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{
    LimitDefaults, Logic, Plan, RunEvent, RunFailure, RunFolders, RunRequest, RunValues, Step,
    read_report,
};
use dbrover_store::{AppPaths, CustomFile, CustomFileKind, DataSourceFile, os_keychain};

use crate::{settings_folder, telemetry};

/// Exit codes, for whoever runs the worker by hand: the app reads the events instead.
const FINISHED: i32 = 0;
const FAILED: i32 = 1;

/// Runs the request on standard input, and returns the process's exit code.
pub fn run(config_dir: Option<&Path>) -> i32 {
    limits::lower_priority();
    if let Err(error) = telemetry::init_stderr() {
        eprintln!("{error:#}");
    }
    let started = Instant::now();
    let mut out = Events::new(io::stdout().lock());
    let outcome = read_request()
        .map_err(|message| RunFailure::new(Step::Worker, message))
        .and_then(|request| {
            limits::limit_memory(request.memory_limit);
            let paths = settings_paths(config_dir)?;
            let (plan, values) = prepare(&paths, &request)?;
            limits::limit_processor_time(plan.processor_time());
            out.send(&RunEvent::Started {
                report: plan.name.clone(),
                sources: plan.source_starts(),
                combine: matches!(plan.logic, Logic::Combine(_)),
            });
            let folders = RunFolders {
                folder: &request.folder,
                staging: &request.staging,
            };
            run_report(&paths, &plan, &values, folders, &mut out)
        });
    match outcome {
        Ok(()) => {
            out.send(&RunEvent::Finished {
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
            FINISHED
        }
        Err(failure) => {
            tracing::warn!("the report failed: {failure}");
            out.send(&RunEvent::Failed(failure));
            FAILED
        }
    }
}

/// Writes events, a line each, flushed at once so the app sees each as it happens; also
/// `dbrover report run --json`'s output.
pub(crate) struct Events<W: Write> {
    out: W,
}

impl<W: Write> Events<W> {
    pub(crate) fn new(out: W) -> Self {
        Self { out }
    }

    pub(crate) fn send(&mut self, event: &RunEvent) {
        let written = serde_json::to_string(event)
            .map_err(io::Error::other)
            .and_then(|line| writeln!(self.out, "{line}"))
            .and_then(|()| self.out.flush());
        if let Err(error) = written {
            // The reader has gone, or closed the pipe: nobody is left to tell.
            tracing::warn!("could not write an event: {error}");
        }
    }
}

fn read_request() -> Result<RunRequest, String> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("could not read the request: {error}"))?;
    serde_json::from_str(&line).map_err(|error| format!("the request isn't one: {error}"))
}

/// The settings folder `config_dir` names, else the one the app uses, as `dbrover report run`
/// finds it too.
pub(crate) fn settings_paths(config_dir: Option<&Path>) -> Result<AppPaths, RunFailure> {
    let fail = |message: String| RunFailure::new(Step::Worker, message);
    let default = AppPaths::discover().ok_or_else(|| {
        fail("the system reports no home folder, so DbRover can't find its files".to_owned())
    })?;
    let decision = settings_folder::decide(default, config_dir)
        .map_err(|error| fail(describe_error(&error)))?;
    if let Some(unavailable) = decision.unavailable() {
        return Err(fail(describe_error(unavailable)));
    }
    Ok(decision.prepare(Vec::new()).paths().clone())
}

/// The report read, planned against the saved data sources and presets, and the request's
/// values checked.
fn prepare(paths: &AppPaths, request: &RunRequest) -> Result<(Plan, RunValues), RunFailure> {
    let fail = |problems: Vec<String>| RunFailure::new(Step::Report, problems.join("\n"));
    let plan = plan(paths, &request.report, &request.defaults).map_err(fail)?;
    let values = plan.values(&request.values).map_err(fail)?;
    Ok((plan, values))
}

/// The report file at `report` read and planned against the settings folder's data sources
/// and presets, under `defaults` where it sets no limit of its own.
///
/// # Errors
///
/// Every problem found, a sentence each: the file can't be read, has problems, or names what
/// the settings folder doesn't have.
pub(crate) fn plan(
    paths: &AppPaths,
    report: &Path,
    defaults: &LimitDefaults,
) -> Result<Plan, Vec<String>> {
    let text = std::fs::read_to_string(report)
        .map_err(|error| vec![format!("could not read {}: {error}", report.display())])?;
    let report = read_report(&text)?;
    let data_sources = DataSourceFile::new(paths.data_sources_file())
        .load()
        .map_err(|error| vec![describe_error(&error)])?;
    let presets = match paths.custom_files().read(CustomFileKind::Exports) {
        CustomFile::Exports(checked) => checked.value,
        // `read` answers with the kind it was asked for.
        _ => Vec::new(),
    };
    Plan::new(report, &data_sources, &presets, defaults)
}

fn run_report(
    paths: &AppPaths,
    plan: &Plan,
    values: &RunValues,
    folders: RunFolders<'_>,
    out: &mut Events<impl Write>,
) -> Result<(), RunFailure> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("dbrover-report")
        .enable_all()
        .build()
        .map_err(|error| {
            RunFailure::new(
                Step::Worker,
                format!("could not start the runtime: {error}"),
            )
        })?;
    runtime.block_on(async {
        make_folder(folders.folder)?;
        let secrets = os_keychain();
        let tunnels = Tunnels::new(paths.ssh_known_hosts_file());
        let commands =
            PasswordCommands::load(&paths.trusted_commands_file(), Arc::new(ProcessRunner)).await;
        let access = SourceAccess {
            secrets: &secrets,
            tunnels: &tunnels,
            commands: &commands,
        };
        dbrover_report::run(plan, values, &access, folders, &mut |event| {
            out.send(&event);
        })
        .await
        .map(drop)
    })
}

fn make_folder(folder: &Path) -> Result<(), RunFailure> {
    std::fs::create_dir_all(folder).map_err(|error| {
        RunFailure::new(
            Step::Worker,
            format!("could not make the folder {}: {error}", folder.display()),
        )
    })
}
