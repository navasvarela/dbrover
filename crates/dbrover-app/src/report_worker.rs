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

use dbrover_core::{BlobId, DataSource, describe_error};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_export::CustomFormat;
use dbrover_report::{
    ExplainRequest, Explanation, LimitDefaults, Machine, Plan, RunEvent, RunFailure, RunFolders,
    RunRequest, RunValues, Step, TimedEvent, read_report,
};
use dbrover_store::{AppPaths, CustomFile, CustomFileKind, DataSourceFile, os_keychain};

use crate::reports::PARENT_ENV;
use crate::{settings_folder, telemetry};

/// Exit codes, for whoever runs the worker by hand: the app reads the events instead.
const FINISHED: i32 = 0;
const FAILED: i32 = 1;

/// Runs the request on standard input, and returns the process's exit code.
pub fn run(config_dir: Option<&Path>) -> i32 {
    limits::die_with_parent(std::env::var(PARENT_ENV).ok().as_deref());
    limits::lower_priority();
    if let Err(error) = telemetry::init_stderr() {
        eprintln!("{error:#}");
    }
    let started = Instant::now();
    let mut out = Events::new(io::stdout().lock(), started).measuring(limits::processor_time_ms);
    // Kept to say, of a limit the run stops at, whether the file or Settings set it.
    let mut planned: Option<Plan> = None;
    let outcome = read_request()
        .map_err(|message| RunFailure::new(Step::Worker, message))
        .and_then(|request| {
            limits::limit_memory(request.memory_limit);
            let paths = settings_paths(config_dir)?;
            let (plan, version, values) = prepare(&paths, &request)?;
            limits::limit_processor_time(plan.processor_time());
            if let Some(explain) = &request.explain {
                tracing::info!(report = %plan.name, %version, "explaining the report");
                let explanation = explain_report(&paths, &plan, &values, explain, &request.work)?;
                // As for a run's end: whether anyone reads it changes nothing.
                let _ = out.send(&RunEvent::Explained(Box::new(explanation)));
                return Ok(());
            }
            tracing::info!(report = %plan.name, %version, "running the report");
            let folders = RunFolders {
                folder: &request.folder,
                staging: &request.staging,
                work: &request.work,
            };
            let started = out.send(&plan.started(version, machine()));
            if started.is_err() {
                abandon(folders);
            }
            let ran = run_report(&paths, &plan, &values, folders, &mut out);
            planned = Some(plan);
            ran
        });
    // Ends the run as much as the end itself: whether anyone reads it changes nothing.
    let _ = out.send(&RunEvent::WorkerUsage {
        peak_memory: limits::peak_memory(),
    });
    match outcome {
        Ok(()) => {
            // The run is over: whether anyone reads its end changes nothing.
            let _ = out.send(&RunEvent::Finished {
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
            FINISHED
        }
        Err(mut failure) => {
            if let Some(plan) = &planned {
                failure.place_limit(plan);
            }
            tracing::warn!("the report failed: {failure}");
            let _ = out.send(&RunEvent::Failed(failure));
            FAILED
        }
    }
}

/// Where the worker runs.
fn machine() -> Machine {
    Machine {
        cores: std::thread::available_parallelism()
            .ok()
            .and_then(|cores| u32::try_from(cores.get()).ok())
            .unwrap_or(1),
        dbrover: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// Writes events, a line each with its time in the run, flushed at once so the app sees each
/// as it happens; also `dbrover report run --json`'s output.
pub(crate) struct Events<W: Write> {
    out: W,
    /// When the run started, which each event's time counts from.
    started: Instant,
    /// The processor time the process has used, which each event says (RS3e).
    processor_time: fn() -> Option<u64>,
}

impl<W: Write> Events<W> {
    pub(crate) fn new(out: W, started: Instant) -> Self {
        Self {
            out,
            started,
            processor_time: || None,
        }
    }

    /// These events, each saying the processor time `processor_time` reads when it is sent.
    pub(crate) fn measuring(self, processor_time: fn() -> Option<u64>) -> Self {
        Self {
            processor_time,
            ..self
        }
    }

    /// Writes `event`, sent now.
    ///
    /// # Errors
    ///
    /// As [`Self::forward`].
    pub(crate) fn send(&mut self, event: &RunEvent) -> io::Result<()> {
        let mut timed = event.clone().at(self.started.elapsed());
        timed.cpu_ms = (self.processor_time)();
        self.forward(&timed)
    }

    /// Writes `event` with the time it already has, as the worker that sent it wrote it.
    ///
    /// # Errors
    ///
    /// When it can't be written: the reader has gone, or closed the pipe. Logged here; the
    /// worker stops its run, while `dbrover report run --json` runs on, its exit code saying
    /// how the run ended.
    pub(crate) fn forward(&mut self, event: &TimedEvent) -> io::Result<()> {
        let written = serde_json::to_string(event)
            .map_err(io::Error::other)
            .and_then(|line| writeln!(self.out, "{line}"))
            .and_then(|()| self.out.flush());
        if let Err(error) = &written {
            tracing::warn!("could not write an event: {error}");
        }
        written
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
fn prepare(
    paths: &AppPaths,
    request: &RunRequest,
) -> Result<(Plan, BlobId, RunValues), RunFailure> {
    let fail = |problems: Vec<String>| RunFailure::new(Step::Report, problems.join("\n"));
    let (plan, version) = plan(paths, &request.report, &request.defaults).map_err(fail)?;
    let values = plan.values(&request.values).map_err(fail)?;
    Ok((plan, version, values))
}

/// The report file at `report` read and planned against the settings folder's data sources
/// and presets, under `defaults` where it sets no limit of its own, with the Git blob id of
/// the text read, which names the version that runs.
///
/// # Errors
///
/// Every problem found, a sentence each: the file can't be read, has problems, or names what
/// the settings folder doesn't have.
pub(crate) fn plan(
    paths: &AppPaths,
    report: &Path,
    defaults: &LimitDefaults,
) -> Result<(Plan, BlobId), Vec<String>> {
    let text = std::fs::read_to_string(report)
        .map_err(|error| vec![format!("could not read {}: {error}", report.display())])?;
    let version = BlobId::of(text.as_bytes());
    let report = read_report(&text)?;
    let named = Named::load(paths)?;
    let plan = Plan::new(report, &named.data_sources, &named.presets, defaults)?;
    Ok((plan, version))
}

/// What a report names, as the settings folder has it: its data sources, and `exports.toml`'s
/// presets.
pub(crate) struct Named {
    pub(crate) data_sources: Vec<DataSource>,
    pub(crate) presets: Vec<CustomFormat>,
}

impl Named {
    pub(crate) fn load(paths: &AppPaths) -> Result<Self, Vec<String>> {
        let data_sources = DataSourceFile::new(paths.data_sources_file())
            .load()
            .map_err(|error| vec![describe_error(&error)])?;
        let presets = match paths.custom_files().read(CustomFileKind::Exports) {
            CustomFile::Exports(checked) => checked.value,
            // `read` answers with the kind it was asked for.
            _ => Vec::new(),
        };
        Ok(Self {
            data_sources,
            presets,
        })
    }
}

/// The runtime a run or an explanation works in: two threads, the worker's process having
/// one report to look after.
fn runtime() -> Result<tokio::runtime::Runtime, RunFailure> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("dbrover-report")
        .enable_all()
        .build()
        .map_err(|error| {
            RunFailure::new(
                Step::Worker,
                format!("could not start the runtime: {error}"),
            )
        })
}

/// Explains the report's sources on their data sources and its combine step over empty tables,
/// DuckDB spilling, if it must, in `work`, which is removed after.
fn explain_report(
    paths: &AppPaths,
    plan: &Plan,
    values: &RunValues,
    request: &ExplainRequest,
    work: &Path,
) -> Result<Explanation, RunFailure> {
    let explanation = runtime()?.block_on(async {
        let secrets = os_keychain();
        let tunnels = Tunnels::new(paths.ssh_known_hosts_file());
        let commands =
            PasswordCommands::load(&paths.trusted_commands_file(), Arc::new(ProcessRunner)).await;
        let access = SourceAccess {
            secrets: &secrets,
            tunnels: &tunnels,
            commands: &commands,
        };
        dbrover_report::explain(plan, values, &access, &request.tables, work).await
    });
    remove(work);
    Ok(explanation)
}

fn run_report(
    paths: &AppPaths,
    plan: &Plan,
    values: &RunValues,
    folders: RunFolders<'_>,
    out: &mut Events<impl Write>,
) -> Result<(), RunFailure> {
    runtime()?.block_on(async {
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
            if out.send(&event).is_err() {
                abandon(folders);
            }
        })
        .await
        .map(drop)
    })
}

/// Stops a run nobody reads the events of any more: the app or command that started it was
/// killed without killing it. Its staging folder and work folder go, as a cancelled run's do,
/// and nothing is put in place. Exits rather than unwinding the run: the sources' connections
/// and DuckDB go with the process.
fn abandon(folders: RunFolders<'_>) -> ! {
    tracing::warn!("nobody reads the run's events any more, so it stops");
    remove(folders.staging);
    remove(folders.work);
    std::process::exit(FAILED)
}

/// Removes a folder of the run's own, if it is there.
fn remove(folder: &Path) {
    if let Err(error) = std::fs::remove_dir_all(folder)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!("could not remove {}: {error}", folder.display());
    }
}

fn make_folder(folder: &Path) -> Result<(), RunFailure> {
    std::fs::create_dir_all(folder).map_err(|error| {
        RunFailure::new(
            Step::Worker,
            format!("could not make the folder {}: {error}", folder.display()),
        )
    })
}
