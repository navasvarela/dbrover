//! Restoring a dump (W17, `docs/dump-restore.md`): the file read first, its engine checked
//! against the server's, a new database created when asked, then `pg_restore` or `psql` run
//! as the data source connects, the plain SQL sent on its stdin. Never into a read-only data
//! source. Asking first is the app's: the engine runs a restore it is sent.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::{
    DataSource, DumpEngine, RestoreRequest, RestoreTarget, SafetyLevel, ServerInfo, describe_error,
};
use dbrover_dump::{
    ArchiveContents, DumpFileKind, GZIP_MAGIC, Input, Program, RestoreSpec, Role, RunFiles, Version,
};
use dbrover_language::QueryLanguage;
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource};
use tokio::sync::oneshot;

use crate::Event;
use crate::dump_programs::ProgramPaths;
use crate::dumps::{DumpFailure, DumpRunId, DumpStart, choose_program, run_until_cancelled};
use crate::open::{Connector, TypedSecrets, open};
use crate::passwords::Prompting;
use crate::program_run::{
    ProgramRun, ProgramServices, RunFolder, RunInput, RunOutput, reach_server, secret, target,
};

/// A restore to run, once the user has said yes.
#[derive(Debug, Clone)]
pub struct RestoreRun {
    pub run: DumpRunId,
    pub request: RestoreRequest,
    /// The server, as the explorer's connection reported it.
    pub server: ServerInfo,
    pub programs: ProgramPaths,
}

/// A restore that ended well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreDone {
    pub database: String,
    /// Whether DbRover created the database for it.
    pub created: bool,
    pub elapsed: Duration,
}

/// What a dump file was found to be, for the Restore dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpFileFound {
    pub kind: DumpFileKind,
    /// An archive's contents and origin.
    pub contents: Option<ArchiveContents>,
    /// The file's size, or a directory archive's files'.
    pub bytes: u64,
}

/// How much of a file is read to tell what it is: a dump's header is in its first lines.
const SNIFF_BYTES: u64 = 8 * 1024;

/// Reads what `file` is: its kind from its first bytes, and for a PostgreSQL archive what
/// `pg_restore --list` says it holds, with the newest `pg_restore` found, since an older one
/// may not read it.
pub(crate) async fn inspect(
    file: &Path,
    services: &ProgramServices,
    paths: &ProgramPaths,
) -> Result<DumpFileFound, String> {
    let path = file.to_owned();
    let kind = tokio::task::spawn_blocking(move || sniff_file(&path))
        .await
        .map_err(|error| error.to_string())??;
    let contents = match kind {
        DumpFileKind::Custom | DumpFileKind::Directory => {
            Some(list_archive(file, services, paths).await?)
        }
        DumpFileKind::Sql { .. } => None,
    };
    Ok(DumpFileFound {
        kind,
        contents,
        bytes: crate::program_run::size_of(file),
    })
}

fn sniff_file(path: &Path) -> Result<DumpFileKind, String> {
    let unreadable =
        |error: io::Error| format!("can't read {}: {}", path.display(), describe_error(&error));
    if path.is_dir() {
        return if path.join("toc.dat").is_file() {
            Ok(DumpFileKind::Directory)
        } else {
            Err(format!(
                "{} isn't a dump: a folder without toc.dat",
                path.display()
            ))
        };
    }
    let mut start = Vec::new();
    File::open(path)
        .map_err(unreadable)?
        .take(SNIFF_BYTES)
        .read_to_end(&mut start)
        .map_err(unreadable)?;
    let gzip = start.starts_with(GZIP_MAGIC);
    if gzip {
        start.clear();
        let file = File::open(path).map_err(unreadable)?;
        // A gzipped file's first bytes once unzipped; a stream cut short there is fine.
        let _ = flate2::read::GzDecoder::new(BufReader::new(file))
            .take(SNIFF_BYTES)
            .read_to_end(&mut start);
    }
    dbrover_dump::sniff(&start, gzip)
        .ok_or_else(|| format!("{} isn't a dump DbRover can restore", path.display()))
}

async fn list_archive(
    file: &Path,
    services: &ProgramServices,
    paths: &ProgramPaths,
) -> Result<ArchiveContents, String> {
    let found = services.versions.find(&[Program::PgRestore], paths).await;
    let newest = found
        .iter()
        .filter(|copy| copy.version.family == DumpEngine::Postgres)
        .max_by_key(|copy| copy.version.version)
        .ok_or_else(|| {
            dbrover_dump::install_hint(DumpEngine::Postgres, 17, dbrover_dump::Os::current())
        })?;
    let mut command = tokio::process::Command::new(&newest.path);
    command
        .arg("--list")
        .arg(file)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(crate::programs::CREATE_NO_WINDOW);
    let output = command
        .output()
        .await
        .map_err(|error| format!("could not run pg_restore: {}", describe_error(&error)))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(ArchiveContents::parse(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Runs `run` on `data_source` until it ends or `cancelled` fires, and reports it with
/// [`Event::RestoreEnded`].
pub(crate) async fn run(
    run: RestoreRun,
    data_source: DataSource,
    services: ProgramServices,
    cancelled: oneshot::Receiver<()>,
) {
    let id = run.run;
    let result = restore(&run, &data_source, &services, cancelled).await;
    match &result {
        Ok(done) => {
            tracing::info!(data_source = %data_source.name, "restored into {}", done.database)
        }
        Err(failure) => tracing::warn!(data_source = %data_source.name, "restore: {failure}"),
    }
    services
        .events
        .send(Event::RestoreEnded { run: id, result });
}

async fn restore(
    run: &RestoreRun,
    data_source: &DataSource,
    services: &ProgramServices,
    cancelled: oneshot::Receiver<()>,
) -> Result<RestoreDone, DumpFailure> {
    if data_source.safety == SafetyLevel::ReadOnly {
        return Err(DumpFailure::ReadOnly(data_source.name.clone()));
    }
    let engine = DumpEngine::of(&data_source.config, &run.server)
        .ok_or_else(|| DumpFailure::NotDumpable(run.server.to_string()))?;
    let server = Version::parse_leading(&run.server.version)
        .ok_or_else(|| DumpFailure::ServerVersion(run.server.version.clone()))?;
    let found = inspect(&run.request.file, services, &run.programs)
        .await
        .map_err(DumpFailure::File)?;
    if !found.kind.restores_into(engine) {
        return Err(DumpFailure::Arguments(
            dbrover_dump::ArgumentError::OtherEngine {
                file: found.kind.engine().unwrap_or(engine),
                server: engine,
            },
        ));
    }
    let role = if found.kind.is_archive() {
        Role::RestoreArchive
    } else {
        Role::RunSql
    };
    let made_by = found
        .contents
        .as_ref()
        .and_then(|contents| contents.made_by);
    let chosen = choose_program(services, engine, role, server, made_by, &run.programs).await?;
    let program = chosen.program;

    let created = match &run.request.target {
        RestoreTarget::NewDatabase(name) => {
            create_database(data_source, services, name).await?;
            true
        }
        RestoreTarget::Existing(_) => false,
    };
    let reached = reach_server(data_source, services).await?;
    let target = target(&reached, run.request.target.database())?;
    let folder = RunFolder::create(&services.run_files)?;
    let files = RunFiles {
        secret: folder.path().join("secret"),
        filter: folder.path().join("filter"),
        output: PathBuf::new(),
    };
    let spec = RestoreSpec {
        engine,
        program: program.version.version,
        kind: found.kind,
        file: &run.request.file,
        tables: run.request.tables.as_deref(),
        options: run.request.options,
        existing: !created,
    };
    let command = dbrover_dump::restore_command(&spec, &target, &files)?;
    folder.write_private(&files.secret, &secret(engine, reached.password.as_ref())?)?;
    let shown = dbrover_dump::command_line(&program.path, &command.shown);
    services.events.send(Event::DumpStarted {
        run: run.run,
        start: DumpStart {
            program: program.path.clone(),
            version: program.version,
            command: shown.clone(),
            warning: chosen.warning,
        },
    });
    tracing::info!(data_source = %data_source.name, "running {shown}");

    let started = Instant::now();
    let executed_at = jiff::Timestamp::now();
    let input = match command.input {
        Input::Named => RunInput::Nothing,
        Input::Stdin { gzip, filter } => RunInput::File {
            path: run.request.file.clone(),
            gzip,
            filter,
        },
    };
    let events = services.events.clone();
    let id = run.run;
    let process = ProgramRun {
        program: program.path.clone(),
        arguments: command.arguments,
        environment: command.environment,
        input,
        output: RunOutput::Nothing,
        // MySQL's client says nothing per table; a plain restore's progress is the bytes sent.
        table: dbrover_dump::restored_table,
        report: Box::new(move |progress| events.send(Event::DumpProgress { run: id, progress })),
    };
    let ended = run_until_cancelled(process, cancelled).await;
    drop(reached);
    drop(folder);
    let result = ended.map(|()| RestoreDone {
        database: run.request.target.database().to_owned(),
        created,
        elapsed: started.elapsed(),
    });
    services.history.record(history_entry(
        data_source,
        &run.request,
        shown,
        executed_at,
        &result,
    ));
    result
}

/// Creates the database a restore goes into, on a connection of its own as the data source
/// connects. One that exists already is refused by the server, and nothing is restored.
async fn create_database(
    data_source: &DataSource,
    services: &ProgramServices,
    name: &str,
) -> Result<(), DumpFailure> {
    let connector = Connector {
        secrets: &services.secrets,
        tunnels: &services.tunnels,
        commands: &services.commands,
        prompting: Prompting::Ask(&services.passwords),
    };
    let mut connection = open(data_source, &TypedSecrets::default(), connector)
        .await
        .map_err(|error| DumpFailure::Connect(describe_error(&error)))?;
    let quoting = QueryLanguage::from(data_source.config.driver())
        .dialect()
        .map(|dialect| dialect.quoting())
        .ok_or_else(|| DumpFailure::NotDumpable(data_source.config.to_string()))?;
    let statement = format!("CREATE DATABASE {}", quoting.identifier(name));
    tracing::info!(data_source = %data_source.name, "{statement}");
    let created = async {
        let cursor = connection.execute(&statement).await?;
        cursor.finish().await
    }
    .await;
    created.map(|_| ()).map_err(|error| {
        DumpFailure::Connect(format!(
            "could not create the database {name}: {}",
            describe_error(&error)
        ))
    })
}

fn history_entry(
    data_source: &DataSource,
    request: &RestoreRequest,
    command: String,
    executed_at: jiff::Timestamp,
    result: &Result<RestoreDone, DumpFailure>,
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
        source: HistorySource::Restore {
            restored: summary(request, result),
        },
    }
}

/// *Restored /dumps/shop.dump into a new database shop_restored*, or what it would have been.
fn summary(request: &RestoreRequest, result: &Result<RestoreDone, DumpFailure>) -> String {
    let into = match &request.target {
        RestoreTarget::NewDatabase(name) => format!("a new database {name}"),
        RestoreTarget::Existing(name) => format!("the database {name}"),
    };
    let tables = request
        .tables
        .as_ref()
        .map(|tables| format!(" ({} tables)", tables.len()))
        .unwrap_or_default();
    let what = format!("{}{tables} into {into}", request.file.display());
    match result {
        Ok(_) => format!("Restored {what}"),
        Err(DumpFailure::Cancelled) => format!("Cancelled the restore of {what}"),
        Err(failure) => format!("Failed to restore {what}: {failure}"),
    }
}
