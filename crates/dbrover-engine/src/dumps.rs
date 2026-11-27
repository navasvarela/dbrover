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

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dbrover_core::{
    DataSource, DumpEngine, DumpFormat, DumpRequest, DumpScope, ServerInfo, describe_error,
};
use dbrover_dump::{
    ArgumentError, DumpCommand, DumpSpec, NoFit, Os, Program, ProgramVersion, Role, RunFiles,
    SecretError, Stdout, Target, Version,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource, SecretStore};
use tokio::sync::oneshot;

use crate::Event;
use crate::dump_programs::{ProgramPaths, ProgramVersions};
use crate::events::EventSink;
use crate::history::HistoryLog;
use crate::keep_awake::KeepAwake;
use crate::open::{Connector, OpenError, Reach, TypedSecrets, reach};
use crate::password_commands::PasswordCommands;
use crate::passwords::{Prompting, SessionPasswords};
use crate::tunnels::Tunnels;

/// Identifies one dump. Chosen by the UI, unique for the app's lifetime.
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

/// How far a dump has got.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpProgress {
    /// Tables whose rows the program has started on.
    pub tables: u32,
    /// The one it is on.
    pub table: Option<String>,
    /// Written so far.
    pub bytes: u64,
}

/// The program a dump started, as the status bar and the log show it.
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

/// Why a dump wrote no file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpFailure {
    #[error("the data source no longer exists")]
    NoDataSource,
    #[error("DbRover can't dump {0}")]
    NotDumpable(String),
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

/// What a dump needs of the engine.
#[derive(Clone)]
pub(crate) struct DumpServices {
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) passwords: SessionPasswords,
    pub(crate) tunnels: Tunnels,
    pub(crate) commands: PasswordCommands,
    pub(crate) events: EventSink,
    pub(crate) history: HistoryLog,
    pub(crate) versions: ProgramVersions,
    /// Where each run's password and filter files go: a folder in the data folder.
    pub(crate) run_files: PathBuf,
}

/// The folder in the data folder that runs' own files go in, emptied at each start.
pub(crate) const RUN_FILES_FOLDER: &str = "dump-runs";

/// How often the run looks whether its program has ended or been cancelled.
const POLL: Duration = Duration::from_millis(100);

/// How often progress is reported at most, so a dump of thousands of small tables doesn't
/// flood the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// stderr lines kept for a failure's message: enough for pg_dump's or mysqldump's error and
/// what led to it.
const STDERR_LINES_KEPT: usize = 20;

/// Runs `run` on `data_source` until it ends or `cancelled` fires, and reports it with
/// [`Event::DumpEnded`].
pub(crate) async fn run(
    run: DumpRun,
    data_source: DataSource,
    services: DumpServices,
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
    services: &DumpServices,
    cancelled: oneshot::Receiver<()>,
) -> Result<DumpDone, DumpFailure> {
    let engine = DumpEngine::of(&data_source.config, &run.server)
        .ok_or_else(|| DumpFailure::NotDumpable(run.server.to_string()))?;
    let server = Version::parse_leading(&run.server.version)
        .ok_or_else(|| DumpFailure::ServerVersion(run.server.version.clone()))?;
    let need = dbrover_dump::Need {
        engine,
        role: Role::Dump,
        server,
        archive_made_by: None,
    };
    let programs = Program::for_role(engine, Role::Dump);
    let found = services.versions.find(programs, &run.programs).await;
    let choice = dbrover_dump::choose(&found, &need, Os::current())?;
    let program = choice.program.clone();
    let warning = choice.warning.clone();

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
    let process = Process {
        run: run.run,
        engine,
        program: program.path.clone(),
        command,
        output: partial.clone(),
        directory: run.request.format == DumpFormat::Directory,
        events: services.events.clone(),
    };
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
    // Held until here: the program connects through it for as long as it runs.
    drop(reached);
    let ended = ended.unwrap_or_else(|error| Err(DumpFailure::Files(error.to_string())));
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

async fn reach_server(
    data_source: &DataSource,
    services: &DumpServices,
) -> Result<Reach, DumpFailure> {
    let connector = Connector {
        secrets: &services.secrets,
        tunnels: &services.tunnels,
        commands: &services.commands,
        prompting: Prompting::Ask(&services.passwords),
    };
    let reached = reach(data_source, &TypedSecrets::default(), connector).await;
    reached.map_err(|error| {
        tell_what_is_needed(data_source, &error, &services.events);
        DumpFailure::Connect(describe_error(&error))
    })
}

/// Asks the app for what connecting lacked, as a session does: the password, the SSH host
/// key's trust, the password command's trust. The app runs the dump again once it has them.
fn tell_what_is_needed(data_source: &DataSource, error: &OpenError, events: &EventSink) {
    let data_source = data_source.id;
    if let Some(key) = error.unknown_host_key() {
        events.send(Event::HostKeyUnknown {
            data_source,
            key: key.clone(),
        });
    }
    if let Some(command) = error.untrusted_password_command() {
        events.send(Event::PasswordCommandUntrusted {
            data_source,
            command: command.clone(),
        });
    }
    if let OpenError::PasswordNeeded { secret, reason } = error {
        events.send(Event::PasswordNeeded {
            data_source,
            secret: *secret,
            reason: reason.clone(),
        });
    }
}

fn target(reached: &Reach, database: &str) -> Result<Target, DumpFailure> {
    let server = reached
        .config
        .server()
        .ok_or_else(|| DumpFailure::NotDumpable(reached.config.to_string()))?;
    let port = server
        .port_number()
        .map_err(|error| DumpFailure::Connect(describe_error(&error)))?;
    Ok(Target {
        host: server.host.clone(),
        port,
        user: server.user.clone(),
        database: database.to_owned(),
        tls: server.tls.clone(),
        tunnel_port: reached
            .tunnel
            .as_ref()
            .map(|tunnel| tunnel.local_addr().port()),
    })
}

/// The password file's contents: the password, or for MySQL's clients an option file without
/// one, which still keeps them from reading `~/.my.cnf`.
fn secret(
    engine: DumpEngine,
    password: Option<&dbrover_core::Password>,
) -> Result<String, SecretError> {
    dbrover_dump::password_file(engine, password.map_or("", |password| password.expose()))
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

fn remove_any(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

/// A file's size, or the sum of a directory's files' (a directory dump's).
fn size_of(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| entry.metadata().ok())
                    .map(|meta| meta.len())
                    .sum()
            })
            .unwrap_or(0),
        Ok(meta) => meta.len(),
        Err(_) => 0,
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

/// The run's own folder for its password and filter files, created where only the user can
/// read it and removed, with them, when dropped.
struct RunFolder(tempfile::TempDir);

impl RunFolder {
    fn create(parent: &Path) -> Result<Self, DumpFailure> {
        let files_error = |error: io::Error| DumpFailure::Files(describe_error(&error));
        std::fs::create_dir_all(parent).map_err(files_error)?;
        // tempfile makes the folder readable by its owner only (0700) on macOS and Linux.
        let folder = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(parent)
            .map_err(files_error)?;
        Ok(Self(folder))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// Writes `contents` to `path`, readable by the user only: libpq refuses a password file
    /// anyone else can read.
    fn write_private(&self, path: &Path, contents: &str) -> Result<(), DumpFailure> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|error| DumpFailure::Files(describe_error(&error)))
    }
}

/// The folder runs' own files go in: [`RUN_FILES_FOLDER`] in the data folder, beside
/// `trusted_commands.toml`.
pub(crate) fn run_files_folder(trusted: &dbrover_store::TrustedCommandsFile) -> PathBuf {
    trusted
        .path()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join(RUN_FILES_FOLDER)
}

/// Removes what earlier runs left in `folder`, which only a crash leaves.
pub(crate) fn remove_leftovers(folder: &Path) {
    match std::fs::remove_dir_all(folder) {
        Ok(()) => tracing::info!("removed files a dump left in {}", folder.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!("could not remove {}: {error}", folder.display()),
    }
}

/// The program, running on threads of its own.
struct Process {
    run: DumpRunId,
    engine: DumpEngine,
    program: PathBuf,
    command: DumpCommand,
    output: PathBuf,
    directory: bool,
    events: EventSink,
}

impl Process {
    /// Runs the program to its end, or until `cancel` is set.
    fn run(self, cancel: &AtomicBool) -> Result<(), DumpFailure> {
        let mut child = self.start()?;
        let bytes = Arc::new(AtomicU64::new(0));
        let writer = match self.command.stdout {
            Stdout::Dump { gzip } => {
                Some(self.write_stdout(&mut child, gzip, Arc::clone(&bytes))?)
            }
            Stdout::Unused => None,
        };
        let progress = Arc::new(Mutex::new(DumpProgress::default()));
        let tail = Arc::new(Mutex::new(Vec::new()));
        let reader = self.read_stderr(&mut child, Arc::clone(&progress), Arc::clone(&tail));
        let status = self.wait(&mut child, cancel, &bytes, &progress);
        let written = writer.map(|writer| writer.join());
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(DumpFailure::Cancelled);
        }
        let status = status.map_err(|error| DumpFailure::Files(describe_error(&error)))?;
        if !status.success() {
            let stderr = tail
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .join("\n");
            return Err(DumpFailure::Exited {
                program: self.program_name(),
                status: status.to_string(),
                stderr,
            });
        }
        match written {
            Some(Ok(Err(error))) => Err(DumpFailure::Files(format!(
                "could not write the dump: {}",
                describe_error(&error)
            ))),
            Some(Err(_)) => Err(DumpFailure::Files("the dump's writer failed".to_owned())),
            Some(Ok(Ok(()))) | None => Ok(()),
        }
    }

    fn program_name(&self) -> String {
        self.program.file_name().map_or_else(
            || self.program.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    fn start(&self) -> Result<Child, DumpFailure> {
        let mut command = std::process::Command::new(&self.program);
        command
            .args(&self.command.arguments)
            .stdin(Stdio::null())
            .stdout(match self.command.stdout {
                Stdout::Dump { .. } => Stdio::piped(),
                Stdout::Unused => Stdio::null(),
            })
            .stderr(Stdio::piped());
        // The data source's settings, and nothing a profile left in the environment.
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("PG") || name_text.starts_with("MYSQL") {
                command.env_remove(&name);
            }
        }
        command.envs(
            self.command
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // A group of its own, so Cancel stops a parallel pg_dump's workers too.
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::programs::CREATE_NO_WINDOW);
        }
        command.spawn().map_err(|error| DumpFailure::CouldNotStart {
            program: self.program.display().to_string(),
            reason: describe_error(&error),
        })
    }

    /// Writes the program's stdout to the `.partial` file, gzipped or not, counting bytes.
    fn write_stdout(
        &self,
        child: &mut Child,
        gzip: bool,
        bytes: Arc<AtomicU64>,
    ) -> Result<JoinHandle<io::Result<()>>, DumpFailure> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DumpFailure::Files("the program's output isn't readable".to_owned()))?;
        let file = File::create_new(&self.output)
            .map_err(|error| DumpFailure::Files(describe_error(&error)))?;
        Ok(std::thread::spawn(move || {
            copy_out(stdout, file, gzip, &bytes)
        }))
    }

    /// Reads stderr's lines: the tables started, logged, and the last few kept for an error.
    fn read_stderr(
        &self,
        child: &mut Child,
        progress: Arc<Mutex<DumpProgress>>,
        tail: Arc<Mutex<Vec<String>>>,
    ) -> Option<JoinHandle<()>> {
        let stderr = child.stderr.take()?;
        let engine = self.engine;
        Some(std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            while matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
                let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                line.clear();
                if let Some(table) = dbrover_dump::dumped_table(engine, &text) {
                    let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                    progress.tables += 1;
                    progress.table = Some(table);
                }
                tracing::debug!("{text}");
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                tail.push(text);
                if tail.len() > STDERR_LINES_KEPT {
                    tail.remove(0);
                }
            }
        }))
    }

    /// Waits for the program, reporting progress, stopping it if cancelled, and keeping the
    /// machine awake once it has run a while.
    fn wait(
        &self,
        child: &mut Child,
        cancel: &AtomicBool,
        bytes: &AtomicU64,
        progress: &Mutex<DumpProgress>,
    ) -> io::Result<ExitStatus> {
        let started = Instant::now();
        let keep_awake = KeepAwake::system();
        let mut kept_awake = false;
        let mut reported = Instant::now();
        let mut last = DumpProgress::default();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if cancel.load(Ordering::SeqCst) {
                stop(child);
                return child.wait();
            }
            let due = keep_awake
                .as_ref()
                .filter(|keep_awake| !kept_awake && started.elapsed() >= keep_awake.after);
            if let Some(keep_awake) = due {
                kept_awake = true;
                if let Err(error) = keep_awake.command(child.id()).spawn() {
                    tracing::warn!("could not keep the machine awake: {error}");
                }
            }
            if reported.elapsed() >= PROGRESS_EVERY {
                reported = Instant::now();
                let mut now = progress
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                now.bytes = match self.command.stdout {
                    Stdout::Dump { .. } => bytes.load(Ordering::Relaxed),
                    Stdout::Unused if self.directory => size_of(&self.output),
                    Stdout::Unused => std::fs::metadata(&self.output).map_or(0, |meta| meta.len()),
                };
                if now != last {
                    self.events.send(Event::DumpProgress {
                        run: self.run,
                        progress: now.clone(),
                    });
                    last = now;
                }
            }
            std::thread::sleep(POLL);
        }
    }
}

/// Stops the program and, on macOS and Linux, every process in its group.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};
        if let Err(error) = kill_process_group(Pid::from_child(child), Signal::KILL) {
            tracing::warn!("could not stop the dump's processes: {error}");
        }
    }
    if let Err(error) = child.kill() {
        tracing::debug!("the dump's program had already ended: {error}");
    }
}

fn copy_out(stdout: impl Read, file: File, gzip: bool, bytes: &AtomicU64) -> io::Result<()> {
    let file = io::BufWriter::new(file);
    if gzip {
        let mut gzipped = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        pump(stdout, &mut gzipped, bytes)?;
        gzipped.finish()?.flush()
    } else {
        let mut plain = file;
        pump(stdout, &mut plain, bytes)?;
        plain.flush()
    }
}

/// Copies `from` to `to`, counting what was read in `bytes`.
fn pump(mut from: impl Read, to: &mut impl Write, bytes: &AtomicU64) -> io::Result<()> {
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = from.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        to.write_all(&chunk[..read])?;
        bytes.fetch_add(u64::try_from(read).unwrap_or(u64::MAX), Ordering::Relaxed);
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
