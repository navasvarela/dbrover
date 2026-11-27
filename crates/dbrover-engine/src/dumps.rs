//! Running a dump (W17, `docs/dump-restore.md`): the engine's own program, chosen for the
//! server's version, connected as the data source connects, its password in a file only the
//! user can read, writing to a `.partial` name that is renamed once it has ended well.
//!
//! A run, in order:
//! 1. choose the program, from the copies found and the server's version;
//! 2. reach the server as a connection would (settings, SSH tunnel, password), holding the
//!    tunnel open until the program ends;
//! 3. write the password file and the program's arguments;
//! 4. run the program on a thread of its own, reading its progress from stderr and, for plain
//!    SQL, writing its stdout to the file, until it ends or is cancelled;
//! 5. rename the `.partial` file, or remove it, remove the run's own files, and record the run
//!    in the history once.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dbrover_core::{
    DataSource, DumpEngine, DumpFormat, DumpRequest, DumpScope, ServerInfo, describe_error,
};
use dbrover_dump::{
    ArgumentError, DumpSpec, FoundProgram, Need, NoFit, Os, Program, ProgramVersion, Role,
    RunFiles, SecretError, Stdout, Version,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource};
use tokio::sync::oneshot;

use crate::Event;
use crate::dump_programs::ProgramPaths;
use crate::program_run::{
    ProgramRun, ProgramServices, RunFolder, RunInput, RunOutput, reach_server, remove_any, secret,
    size_of, target,
};

/// Identifies one dump or restore. Chosen by the UI, unique for the app's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DumpRunId(pub u64);

/// A dump to run.
#[derive(Debug, Clone)]
pub struct DumpRun {
    pub run: DumpRunId,
    pub request: DumpRequest,
    /// The server, as the explorer's connection reported it: which engine it is and its
    /// version decide the program.
    pub server: ServerInfo,
    pub programs: ProgramPaths,
}

/// How far a dump or a restore has got.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpProgress {
    /// Tables whose rows the program has started on.
    pub tables: u32,
    /// The one it is on.
    pub table: Option<String>,
    /// Written so far by a dump; read of the file so far by a restore of plain SQL.
    pub bytes: u64,
}

/// The program a dump or a restore started, as the status bar and the log show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpStart {
    pub program: PathBuf,
    pub version: ProgramVersion,
    /// The command, as **Copy command** gives it: never with the password.
    pub command: String,
    pub warning: Option<String>,
}

/// A dump that ended well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpDone {
    pub file: PathBuf,
    pub bytes: u64,
    pub elapsed: Duration,
}

/// Why a dump wrote no file, or a restore didn't finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpFailure {
    #[error("the data source no longer exists")]
    NoDataSource,
    #[error("DbRover can't dump or restore {0}")]
    NotDumpable(String),
    #[error("\"{0}\" is read-only, so nothing can be restored into it")]
    ReadOnly(String),
    /// The dump file can't be read, or isn't a dump.
    #[error("{0}")]
    File(String),
    #[error("the server's version, {0:?}, isn't one DbRover reads")]
    ServerVersion(String),
    #[error(transparent)]
    NoProgram(#[from] NoFit),
    #[error(transparent)]
    Arguments(#[from] ArgumentError),
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The data source couldn't be reached; the app has been told what it needs, if anything.
    #[error("{0}")]
    Connect(String),
    #[error("{} already exists; choose another name", .0.display())]
    Exists(PathBuf),
    #[error("{0}")]
    Files(String),
    #[error("could not start {program}: {reason}")]
    CouldNotStart { program: String, reason: String },
    #[error("{program} failed ({status}): {stderr}")]
    Exited {
        program: String,
        status: String,
        /// Its last lines, which say why far better than its exit status.
        stderr: String,
    },
    #[error("cancelled")]
    Cancelled,
}

/// Runs `run` on `data_source` until it ends or `cancelled` fires, and reports it with
/// [`Event::DumpEnded`].
pub(crate) async fn run(
    run: DumpRun,
    data_source: DataSource,
    services: ProgramServices,
    cancelled: oneshot::Receiver<()>,
) {
    let id = run.run;
    let events = services.events.clone();
    let result = dump(run, &data_source, &services, cancelled).await;
    match &result {
        Ok(done) => tracing::info!(
            data_source = %data_source.name,
            "dumped to {} ({} bytes)",
            done.file.display(),
            done.bytes
        ),
        Err(failure) => tracing::warn!(data_source = %data_source.name, "dump: {failure}"),
    }
    events.send(Event::DumpEnded { run: id, result });
}

async fn dump(
    run: DumpRun,
    data_source: &DataSource,
    services: &ProgramServices,
    cancelled: oneshot::Receiver<()>,
) -> Result<DumpDone, DumpFailure> {
    let engine = DumpEngine::of(&data_source.config, &run.server)
        .ok_or_else(|| DumpFailure::NotDumpable(run.server.to_string()))?;
    let server = Version::parse_leading(&run.server.version)
        .ok_or_else(|| DumpFailure::ServerVersion(run.server.version.clone()))?;
    let choice = choose_program(services, engine, Role::Dump, server, None, &run.programs).await?;
    let (program, warning) = (choice.program, choice.warning);

    let final_path = run.request.file.clone();
    let partial = partial_path(&final_path);
    refuse_existing(&final_path, &partial)?;
    let reached = reach_server(data_source, services).await?;
    let target = target(&reached, &run.request.database)?;
    let folder = RunFolder::create(&services.run_files)?;
    let files = RunFiles {
        secret: folder.path().join("secret"),
        filter: folder.path().join("filter"),
        output: partial.clone(),
    };
    let spec = DumpSpec {
        engine,
        program: program.version.version,
        server,
        scope: &run.request.scope,
        format: run.request.format,
        options: run.request.options,
    };
    let command = dbrover_dump::dump_command(&spec, &target, &files)?;
    folder.write_private(&files.secret, &secret(engine, reached.password.as_ref())?)?;
    if let Some(filter) = &command.filter {
        folder.write_private(&files.filter, filter)?;
    }
    let shown = dbrover_dump::command_line(&program.path, &command.shown);
    services.events.send(Event::DumpStarted {
        run: run.run,
        start: DumpStart {
            program: program.path.clone(),
            version: program.version,
            command: shown.clone(),
            warning,
        },
    });
    tracing::info!(data_source = %data_source.name, "running {shown}");

    let started = Instant::now();
    let executed_at = jiff::Timestamp::now();
    let output = match command.stdout {
        Stdout::Dump { gzip } => RunOutput::Stdout {
            path: partial.clone(),
            gzip,
        },
        Stdout::Unused => RunOutput::Written(partial.clone()),
    };
    let events = services.events.clone();
    let id = run.run;
    let process = ProgramRun {
        program: program.path.clone(),
        arguments: command.arguments,
        environment: command.environment,
        input: RunInput::Nothing,
        output,
        table: table_reader(engine),
        report: Box::new(move |progress| events.send(Event::DumpProgress { run: id, progress })),
    };
    let ended = run_until_cancelled(process, cancelled).await;
    // Held until here: the program connects through it for as long as it runs.
    drop(reached);
    let result = finish(ended, &partial, &final_path, started.elapsed());
    drop(folder);
    services.history.record(history_entry(
        data_source,
        &run.request,
        shown,
        executed_at,
        &result,
    ));
    result
}

/// The program for `role`, chosen for the server's version from the copies found.
pub(crate) struct ChosenProgram {
    pub(crate) program: FoundProgram,
    pub(crate) warning: Option<String>,
}

pub(crate) async fn choose_program(
    services: &ProgramServices,
    engine: DumpEngine,
    role: Role,
    server: Version,
    archive_made_by: Option<Version>,
    paths: &ProgramPaths,
) -> Result<ChosenProgram, DumpFailure> {
    let need = Need {
        engine,
        role,
        server,
        archive_made_by,
    };
    let found = services
        .versions
        .find(Program::for_role(engine, role), paths)
        .await;
    let choice = dbrover_dump::choose(&found, &need, Os::current())?;
    Ok(ChosenProgram {
        program: choice.program.clone(),
        warning: choice.warning,
    })
}

/// Runs `process` on a thread of its own until it ends, or stops it when `cancelled` fires.
pub(crate) async fn run_until_cancelled(
    process: ProgramRun,
    cancelled: oneshot::Receiver<()>,
) -> Result<(), DumpFailure> {
    let cancel = Arc::new(AtomicBool::new(false));
    let task_cancel = Arc::clone(&cancel);
    let mut task = tokio::task::spawn_blocking(move || process.run(&task_cancel));
    let ended = tokio::select! {
        ended = &mut task => ended,
        _ = cancelled => {
            cancel.store(true, Ordering::SeqCst);
            task.await
        }
    };
    ended.unwrap_or_else(|error| Err(DumpFailure::Files(error.to_string())))
}

/// How a dump program's stderr names the table it has started on.
fn table_reader(engine: DumpEngine) -> fn(&str) -> Option<String> {
    match engine {
        DumpEngine::Postgres => |line| dbrover_dump::dumped_table(DumpEngine::Postgres, line),
        DumpEngine::Mysql | DumpEngine::MariaDb => {
            |line| dbrover_dump::dumped_table(DumpEngine::Mysql, line)
        }
    }
}

/// `shop.dump` is written as `shop.dump.partial`, and a directory `shop` as `shop.partial`.
pub(crate) fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    path.with_file_name(name)
}

fn refuse_existing(final_path: &Path, partial: &Path) -> Result<(), DumpFailure> {
    if final_path.exists() {
        return Err(DumpFailure::Exists(final_path.to_owned()));
    }
    // A partial file is a run's that crashed: nobody else writes that name.
    remove_any(partial).map_err(|error| DumpFailure::Files(describe_error(&error)))
}

/// Renames the `.partial` file once the program ended well; removes it otherwise.
fn finish(
    ended: Result<(), DumpFailure>,
    partial: &Path,
    final_path: &Path,
    elapsed: Duration,
) -> Result<DumpDone, DumpFailure> {
    let renamed = ended.and_then(|()| {
        std::fs::rename(partial, final_path).map_err(|error| {
            DumpFailure::Files(format!(
                "could not rename the dump: {}",
                describe_error(&error)
            ))
        })
    });
    match renamed {
        Ok(()) => Ok(DumpDone {
            file: final_path.to_owned(),
            bytes: size_of(final_path),
            elapsed,
        }),
        Err(failure) => {
            if let Err(error) = remove_any(partial) {
                tracing::warn!("could not remove {}: {error}", partial.display());
            }
            Err(failure)
        }
    }
}

fn history_entry(
    data_source: &DataSource,
    request: &DumpRequest,
    command: String,
    executed_at: jiff::Timestamp,
    result: &Result<DumpDone, DumpFailure>,
) -> HistoryEntry {
    let (outcome, duration) = match result {
        Ok(done) => (HistoryOutcome::Ok, Some(done.elapsed)),
        Err(DumpFailure::Cancelled) => (HistoryOutcome::Cancelled, None),
        Err(_) => (HistoryOutcome::Error, None),
    };
    HistoryEntry {
        data_source: data_source.id,
        executed_at,
        sql: command,
        duration,
        rows: None,
        outcome,
        source: HistorySource::Dump {
            dumped: summary(request, result),
        },
    }
}

/// *Dumped shop (2 tables, custom) to /dumps/shop.dump, 142 MB*, or what it would have been.
fn summary(request: &DumpRequest, result: &Result<DumpDone, DumpFailure>) -> String {
    let scope = match &request.scope {
        DumpScope::Database => String::new(),
        DumpScope::Schemas(schemas) => format!(", {} schemas", schemas.len()),
        DumpScope::Tables(tables) => format!(", {} tables", tables.len()),
    };
    let format = match request.format {
        DumpFormat::Custom => "custom",
        DumpFormat::Directory => "directory",
        DumpFormat::Sql => "plain SQL",
        DumpFormat::SqlGz => "plain SQL, gzipped",
    };
    let what = format!(
        "{} ({format}{scope}) to {}",
        request.database,
        request.file.display()
    );
    match result {
        Ok(done) => format!("Dumped {what}, {}", dbrover_core::ByteSize(done.bytes)),
        Err(DumpFailure::Cancelled) => format!("Cancelled the dump of {what}"),
        Err(failure) => format!("Failed to dump {what}: {failure}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_beside_the_final_name() {
        assert_eq!(
            partial_path(Path::new("/d/shop.dump")),
            Path::new("/d/shop.dump.partial")
        );
        assert_eq!(
            partial_path(Path::new("/d/shop")),
            Path::new("/d/shop.partial")
        );
    }
}
