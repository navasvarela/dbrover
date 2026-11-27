//! What dumps and restores share (W17, `docs/dump-restore.md`): reaching the server as a
//! connection would, the run's own folder for its password file, and running the engine's
//! program on threads of its own, its stderr read for progress, a file streamed to its stdin or
//! from its stdout, until it ends or is cancelled.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dbrover_core::{DataSource, DumpEngine, Password, describe_error};
use dbrover_dump::{DefinerFilter, LineFilter, OwnerFilter, SecretError, Target};
use dbrover_store::SecretStore;

use crate::Event;
use crate::dump_programs::ProgramVersions;
use crate::dumps::{DumpFailure, DumpProgress};
use crate::events::EventSink;
use crate::history::HistoryLog;
use crate::keep_awake::KeepAwake;
use crate::open::{Connector, OpenError, Reach, TypedSecrets, reach};
use crate::password_commands::PasswordCommands;
use crate::passwords::{Prompting, SessionPasswords};
use crate::tunnels::Tunnels;

/// What a dump or a restore needs of the engine.
#[derive(Clone)]
pub(crate) struct ProgramServices {
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) passwords: SessionPasswords,
    pub(crate) tunnels: Tunnels,
    pub(crate) commands: PasswordCommands,
    pub(crate) events: EventSink,
    pub(crate) history: HistoryLog,
    pub(crate) versions: ProgramVersions,
    /// The commands the user trusted: a dump program's path set by hand runs only once
    /// trusted.
    pub(crate) trusted: tokio::sync::watch::Receiver<dbrover_store::TrustedCommands>,
    /// Where each run's password and filter files go: a folder in the data folder.
    pub(crate) run_files: PathBuf,
}

/// The folder in the data folder that runs' own files go in, emptied at each start.
const RUN_FILES_FOLDER: &str = "dump-runs";

/// How often a run looks whether its program has ended or been cancelled.
const POLL: Duration = Duration::from_millis(100);

/// How often progress is reported at most, so a dump of thousands of small tables doesn't
/// flood the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// stderr lines kept for a failure's message: enough for the program's error and what led to
/// it.
const STDERR_LINES_KEPT: usize = 20;

/// Reaches the data source's server as a connection would, asking the app for what is
/// missing; the app runs the dump or restore again once it has it.
pub(crate) async fn reach_server(
    data_source: &DataSource,
    services: &ProgramServices,
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
/// key's trust, the password command's trust.
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

/// Where the program connects, for `database`, through the tunnel `reached` holds.
pub(crate) fn target(reached: &Reach, database: &str) -> Result<Target, DumpFailure> {
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
pub(crate) fn secret(
    engine: DumpEngine,
    password: Option<&Password>,
) -> Result<String, SecretError> {
    dbrover_dump::password_file(engine, password.map_or("", |password| password.expose()))
}

/// Removes a file or a folder, if there is one.
pub(crate) fn remove_any(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// A file's size, or the sum of a directory's files' (a directory dump's).
pub(crate) fn size_of(path: &Path) -> u64 {
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

/// The run's own folder for its password and filter files, created where only the user can
/// read it and removed, with them, when dropped.
pub(crate) struct RunFolder(tempfile::TempDir);

impl RunFolder {
    pub(crate) fn create(parent: &Path) -> Result<Self, DumpFailure> {
        let files_error = |error: io::Error| DumpFailure::Files(describe_error(&error));
        std::fs::create_dir_all(parent).map_err(files_error)?;
        // tempfile makes the folder readable by its owner only (0700) on macOS and Linux.
        let folder = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(parent)
            .map_err(files_error)?;
        Ok(Self(folder))
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }

    /// Writes `contents` to `path`, readable by the user only: libpq refuses a password file
    /// anyone else can read.
    pub(crate) fn write_private(&self, path: &Path, contents: &str) -> Result<(), DumpFailure> {
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

/// What the program reads on its standard input.
pub(crate) enum RunInput {
    Nothing,
    /// A dump file, sent as it is read, gunzipped when `gzip`, through `filter`.
    File {
        path: PathBuf,
        gzip: bool,
        filter: LineFilter,
    },
}

/// Where the program's dump goes.
pub(crate) enum RunOutput {
    /// Nowhere DbRover writes: a restore's.
    Nothing,
    /// The program writes it, to this file or folder, whose size is the progress.
    Written(PathBuf),
    /// Its standard output, written by DbRover to this file, gzipped when `gzip`.
    Stdout { path: PathBuf, gzip: bool },
}

/// The program to run, and what it reads and writes.
pub(crate) struct ProgramRun {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<String>,
    pub(crate) environment: Vec<(String, String)>,
    pub(crate) input: RunInput,
    pub(crate) output: RunOutput,
    /// The table a line of the program's stderr says it has started on.
    pub(crate) table: fn(&str) -> Option<String>,
    /// Reports progress, at most every [`PROGRESS_EVERY`].
    pub(crate) report: Box<dyn Fn(DumpProgress) + Send>,
}

impl ProgramRun {
    /// Runs the program to its end, or until `cancel` is set.
    pub(crate) fn run(self, cancel: &AtomicBool) -> Result<(), DumpFailure> {
        let mut child = self.start()?;
        let bytes = Arc::new(AtomicU64::new(0));
        let streams = self.stream(&mut child, &bytes)?;
        let progress = Arc::new(Mutex::new(DumpProgress::default()));
        let tail = Arc::new(Mutex::new(Vec::new()));
        let reader = self.read_stderr(&mut child, Arc::clone(&progress), Arc::clone(&tail));
        let status = self.wait(&mut child, cancel, &bytes, &progress);
        let streamed = streams.map(|stream| stream.join());
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
        match streamed {
            Some(Ok(Err(error))) => Err(DumpFailure::Files(describe_error(&error))),
            Some(Err(_)) => Err(DumpFailure::Files("the dump's stream failed".to_owned())),
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
            .args(&self.arguments)
            .stdin(match self.input {
                RunInput::File { .. } => Stdio::piped(),
                RunInput::Nothing => Stdio::null(),
            })
            .stdout(match self.output {
                RunOutput::Stdout { .. } => Stdio::piped(),
                RunOutput::Nothing | RunOutput::Written(_) => Stdio::null(),
            })
            .stderr(Stdio::piped());
        // The data source's settings, and nothing a profile left in the environment.
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("PG") || name_text.starts_with("MYSQL") {
                command.env_remove(&name);
            }
        }
        command.envs(self.environment.iter().map(|(name, value)| (name, value)));
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

    /// Starts streaming the dump file to the program's stdin, or its stdout to the dump file,
    /// counting the bytes in `bytes`.
    fn stream(
        &self,
        child: &mut Child,
        bytes: &Arc<AtomicU64>,
    ) -> Result<Option<JoinHandle<io::Result<()>>>, DumpFailure> {
        let files_error = |error: io::Error| DumpFailure::Files(describe_error(&error));
        let bytes = Arc::clone(bytes);
        match (&self.input, &self.output) {
            (RunInput::File { path, gzip, filter }, _) => {
                let stdin = child.stdin.take().ok_or_else(|| {
                    DumpFailure::Files("the program can't be sent the dump".into())
                })?;
                let file = File::open(path).map_err(files_error)?;
                let (gzip, filter) = (*gzip, *filter);
                Ok(Some(std::thread::spawn(move || {
                    send(file, stdin, gzip, filter, &bytes)
                })))
            }
            (RunInput::Nothing, RunOutput::Stdout { path, gzip }) => {
                let stdout = child.stdout.take().ok_or_else(|| {
                    DumpFailure::Files("the program's output isn't readable".into())
                })?;
                let file = File::create_new(path).map_err(files_error)?;
                let gzip = *gzip;
                Ok(Some(std::thread::spawn(move || {
                    copy_out(stdout, file, gzip, &bytes)
                })))
            }
            (RunInput::Nothing, RunOutput::Nothing | RunOutput::Written(_)) => Ok(None),
        }
    }

    /// Reads stderr's lines: the tables started, logged, and the last few kept for an error.
    fn read_stderr(
        &self,
        child: &mut Child,
        progress: Arc<Mutex<DumpProgress>>,
        tail: Arc<Mutex<Vec<String>>>,
    ) -> Option<JoinHandle<()>> {
        let stderr = child.stderr.take()?;
        let table = self.table;
        Some(std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            while matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
                let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                line.clear();
                if let Some(name) = table(&text) {
                    let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                    progress.tables += 1;
                    progress.table = Some(name);
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
                now.bytes = match &self.output {
                    RunOutput::Written(path) => size_of(path),
                    RunOutput::Stdout { .. } | RunOutput::Nothing => bytes.load(Ordering::Relaxed),
                };
                if now != last {
                    (self.report)(now.clone());
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
            tracing::warn!("could not stop the program's processes: {error}");
        }
    }
    if let Err(error) = child.kill() {
        tracing::debug!("the program had already ended: {error}");
    }
}

/// Writes a program's stdout to `file`, gzipped or not.
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

/// Sends a dump file to a program's stdin, gunzipped when `gzip`, through `filter`, counting
/// the file's bytes read. Closing stdin at the end is how the program knows the dump is over.
fn send(
    file: File,
    stdin: ChildStdin,
    gzip: bool,
    filter: LineFilter,
    bytes: &AtomicU64,
) -> io::Result<()> {
    let counted = Counted { inner: file, bytes };
    let mut to = io::BufWriter::new(stdin);
    if gzip {
        let gunzipped = flate2::read::GzDecoder::new(counted);
        lines(BufReader::new(gunzipped), &mut to, filter)?;
    } else {
        lines(BufReader::new(counted), &mut to, filter)?;
    }
    to.flush()
}

/// Copies a dump line by line, through `filter`.
fn lines(mut from: impl BufRead, to: &mut impl Write, filter: LineFilter) -> io::Result<()> {
    let mut owners = OwnerFilter::default();
    let mut definers = DefinerFilter;
    let mut line = Vec::new();
    loop {
        line.clear();
        if from.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let kept = match filter {
            LineFilter::Nothing => Some(Cow::Borrowed(line.as_slice())),
            LineFilter::Owners => owners.apply(&line),
            LineFilter::Definers => Some(definers.apply(&line)),
        };
        if let Some(kept) = kept {
            to.write_all(&kept)?;
        }
    }
}

/// A reader that counts what it reads.
struct Counted<'a, R> {
    inner: R,
    bytes: &'a AtomicU64,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.bytes
            .fetch_add(u64::try_from(read).unwrap_or(u64::MAX), Ordering::Relaxed);
        Ok(read)
    }
}
