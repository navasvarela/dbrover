//! Running the programs command actions name (H2): filling in the command, writing the selected
//! cells to its stdin, and collecting what it writes, under a timeout and a stop. See
//! `docs/hackability.md`.
//!
//! A run is a future raced against its timeout and its stop. Whichever loses is dropped, and
//! the child process is spawned with `kill_on_drop`, so a timeout, a stop and the engine
//! shutting down all kill the program the same way.

use std::io;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dbrover_core::{
    DataSource, DataSourceFields, DataSourceId, TemplateValues, Variables, describe_error,
};
use dbrover_store::{
    ActionId, CommandAction, StdinFormat, TrustedCommand, TrustedCommands, TrustedCommandsFile,
    driver_id,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::Event;
use crate::config_files::blocking;
use crate::events::EventSink;

/// stdout kept from a program, chosen rather than measured: far more than anyone reads in a
/// viewer or a console, and little enough to hold in memory. Past it the program is stopped.
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;

/// stderr kept from a program: an error message, not a log. The rest is read and discarded,
/// so a chatty program never blocks on a full pipe.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// How long to keep reading a program's output once it has ended. What it wrote last is still
/// in the pipes, at most a pipe's buffer (64 KB on Linux and macOS), which takes far less than
/// this to read. A child of its own that it left running may hold the pipes open forever, and
/// this is how long DbRover waits for it before giving up on the rest.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);

/// Hides the console window Windows would otherwise open for a console program started from a
/// GUI app (`CREATE_NO_WINDOW` in the Win32 process creation flags).
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Identifies one run of a command action. Chosen by the UI, unique for the app's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProgramId(pub u64);

/// A command action to run on what it was chosen on.
#[derive(Debug, Clone)]
pub struct ProgramRequest {
    pub program: ProgramId,
    pub data_source: DataSourceId,
    pub action: ActionId,
    pub command: CommandAction,
    /// What fills the placeholders: the node or the cells the action was chosen on.
    pub values: TemplateValues,
}

/// What a program that succeeded wrote, as UTF-8 with invalid bytes replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramOutput {
    pub stdout: String,
    pub stderr: String,
    /// The program wrote more than DbRover keeps, and was stopped there.
    pub truncated: bool,
}

/// Why a program gave no output to use, as a sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProgramFailure {
    #[error("this command isn't trusted to run yet; choose its action again to be asked")]
    NotTrusted,
    #[error("the data source no longer exists")]
    NoDataSource,
    /// A placeholder nothing fills, or a data source setting that doesn't resolve.
    #[error("{0}")]
    Unfillable(String),
    #[error("{0}")]
    CouldNotStart(String),
    #[error("{program} failed ({status})")]
    Exited {
        program: String,
        /// How it ended, as the OS says it: `exit status: 3`, `signal: 9 (SIGKILL)`.
        status: String,
        stderr: String,
    },
    #[error("stopped after {} without finishing", seconds(*after))]
    TimedOut { after: Duration },
    #[error("stopped")]
    Stopped,
    #[error("could not read the program's output: {0}")]
    Output(String),
}

pub(crate) fn seconds(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        // `#` selects jiff's friendly format ("30s") over ISO 8601 ("PT30S").
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}

/// A program, its arguments and everything else it gets, filled in and ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub arguments: Vec<String>,
    /// Set on top of DbRover's own environment.
    pub environment: Vec<(String, String)>,
    pub stdin: String,
    pub directory: Option<PathBuf>,
}

/// Starts programs and collects what they write. The engine runs [`ProcessRunner`]; tests of
/// the app give it a scripted one, so they start no processes.
#[async_trait]
pub trait ProgramRunner: Send + Sync {
    /// Runs the program to its end. Dropping the future must stop the program: that is how
    /// timeouts and stops reach it.
    async fn run(&self, invocation: Invocation) -> Result<ProgramOutput, ProgramFailure>;

    /// Starts the program for an export from `exports.toml` (W8d), with its stdout going to
    /// `out`: the rows are written to the pipe it returns, which waits up to `timeout` for the
    /// program to take each page, and for it to finish. Dropping the pipe must stop the
    /// program.
    ///
    /// # Errors
    ///
    /// The program couldn't start.
    fn start_export(
        &self,
        invocation: Invocation,
        out: std::fs::File,
        timeout: Duration,
    ) -> io::Result<Box<dyn dbrover_export::ExportPipe>>;
}

/// Runs real programs with tokio's process support, directly, never through a shell.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

#[async_trait]
impl ProgramRunner for ProcessRunner {
    async fn run(&self, invocation: Invocation) -> Result<ProgramOutput, ProgramFailure> {
        let mut child = command(&invocation)
            .spawn()
            .map_err(|error| start_failure(&invocation.program, &error))?;
        // A task of its own, so a program that never reads its input can't hold up reading its
        // output; aborted once the run is over, however it ends.
        let _writer = AbortOnDrop(
            tokio::spawn(write_stdin(child.stdin.take(), invocation.stdin.clone())).abort_handle(),
        );
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (status, truncated) = {
            let reads = read_output(stdout, stderr, &mut out, &mut err);
            let mut reads = std::pin::pin!(reads);
            tokio::select! {
                status = child.wait() => {
                    let truncated = match tokio::time::timeout(DRAIN_AFTER_EXIT, &mut reads).await {
                        Ok(read) => read.map_err(output_failure)?,
                        Err(_) => false,
                    };
                    (status, truncated)
                }
                read = &mut reads => {
                    let truncated = read.map_err(output_failure)?;
                    if truncated {
                        // Nothing more can be done about a program that won't be killed; the
                        // wait below then ends when it does.
                        let _ = child.start_kill();
                    }
                    (child.wait().await, truncated)
                }
            }
        };
        let status = status.map_err(output_failure)?;
        let output = ProgramOutput {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            truncated,
        };
        if truncated || status.success() {
            Ok(output)
        } else {
            Err(exited(&invocation.program, status, output.stderr))
        }
    }

    fn start_export(
        &self,
        invocation: Invocation,
        out: std::fs::File,
        timeout: Duration,
    ) -> io::Result<Box<dyn dbrover_export::ExportPipe>> {
        Ok(Box::new(crate::export_programs::ProcessPipe::start(
            invocation, out, timeout,
        )?))
    }
}

fn command(invocation: &Invocation) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&invocation.program);
    command
        .args(&invocation.arguments)
        .envs(
            invocation
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(directory) = &invocation.directory {
        command.current_dir(directory);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// A program that can't start. One that isn't found names the `PATH` it was looked for in: a
/// Mac app started from the Finder gets a shorter one than a terminal has.
pub(crate) fn start_failure(program: &str, error: &io::Error) -> ProgramFailure {
    let is_a_name = !program.contains(['/', '\\']);
    if error.kind() == io::ErrorKind::NotFound && is_a_name {
        let path = std::env::var("PATH").unwrap_or_default();
        return ProgramFailure::CouldNotStart(format!(
            "{program} was not found in DbRover's PATH ({path}); write its full path in the \
             command"
        ));
    }
    ProgramFailure::CouldNotStart(format!(
        "could not start {program}: {}",
        describe_error(error)
    ))
}

fn exited(program: &str, status: ExitStatus, stderr: String) -> ProgramFailure {
    ProgramFailure::Exited {
        program: program.to_owned(),
        status: status.to_string(),
        stderr,
    }
}

fn output_failure(error: io::Error) -> ProgramFailure {
    ProgramFailure::Output(describe_error(&error))
}

/// Aborts a task when dropped.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Reads stdout and stderr together until both end, or until stdout passes its limit, which
/// returns `true` at once so the program can be stopped: waiting for stderr to end then would
/// wait on a program blocked writing the stdout nobody reads.
async fn read_output(
    stdout: Option<impl AsyncRead + Unpin>,
    stderr: Option<impl AsyncRead + Unpin>,
    out: &mut Vec<u8>,
    err: &mut Vec<u8>,
) -> io::Result<bool> {
    let mut stdout_read = std::pin::pin!(read_up_to(stdout, out, MAX_STDOUT_BYTES));
    let mut stderr_read = std::pin::pin!(read_keeping_first(stderr, err, MAX_STDERR_BYTES));
    let mut stderr_ended = false;
    loop {
        tokio::select! {
            truncated = &mut stdout_read => {
                if truncated? {
                    return Ok(true);
                }
                if !stderr_ended {
                    stderr_read.await?;
                }
                return Ok(false);
            }
            ended = &mut stderr_read, if !stderr_ended => {
                ended?;
                stderr_ended = true;
            }
        }
    }
}

/// Writes the program's input, then closes stdin so it sees the end.
async fn write_stdin(pipe: Option<ChildStdin>, text: String) {
    let Some(mut pipe) = pipe else {
        return;
    };
    // A program that ends, or closes stdin, before reading all of it makes this fail. What it
    // didn't read it didn't need, and its exit status says whether it worked.
    if let Err(error) = pipe.write_all(text.as_bytes()).await {
        tracing::debug!("the program stopped reading its input: {error}");
    }
}

/// Reads `pipe` into `buffer` until it ends, or until it holds more than `limit` bytes, which
/// returns `true` with the first `limit` kept.
async fn read_up_to(
    pipe: Option<impl AsyncRead + Unpin>,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<bool> {
    let Some(mut pipe) = pipe else {
        return Ok(false);
    };
    let mut chunk = vec![0; 8192];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(false);
        }
        let room = limit.saturating_sub(buffer.len());
        if read > room {
            buffer.extend_from_slice(&chunk[..room]);
            return Ok(true);
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Reads `pipe` to its end, keeping its first `limit` bytes.
async fn read_keeping_first(
    pipe: Option<impl AsyncRead + Unpin>,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<()> {
    let Some(mut pipe) = pipe else {
        return Ok(());
    };
    let mut chunk = vec![0; 8192];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        let room = limit.saturating_sub(buffer.len());
        buffer.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// Fills in the command for `request` on `data_source`: arguments from the placeholders, the
/// data source's fields from its settings with `${NAME}` variables resolved, the context in
/// `DBROVER_*` variables, and the selected cells on stdin.
///
/// Nothing here reads a password: the settings hold none, and the engine's passwords aren't
/// given to it.
///
/// # Errors
///
/// [`ProgramFailure::Unfillable`] for a setting that doesn't resolve or a placeholder nothing
/// fills.
pub(crate) fn invocation(
    request: &ProgramRequest,
    data_source: &DataSource,
    variables: &dyn Variables,
) -> Result<Invocation, ProgramFailure> {
    let config = data_source
        .config
        .resolve(variables)
        .map_err(|error| ProgramFailure::Unfillable(describe_error(&error)))?;
    let fields = DataSourceFields::from(&config);
    let values = &request.values;
    let template = &request.command.command;
    let arguments = template
        .render(values, &fields)
        .map_err(|error| ProgramFailure::Unfillable(error.to_string()))?;
    let stdin = match (request.command.stdin, &values.cells) {
        (StdinFormat::None, _) | (_, None) => String::new(),
        (format, Some(cells)) => format.encode(cells),
    };
    Ok(Invocation {
        program: template.program().to_owned(),
        arguments,
        environment: environment(request, data_source, &fields),
        stdin,
        directory: variables.home(),
    })
}

fn environment(
    request: &ProgramRequest,
    data_source: &DataSource,
    fields: &DataSourceFields,
) -> Vec<(String, String)> {
    let values = &request.values;
    let known = [
        ("DBROVER_DATA_SOURCE", Some(&data_source.name)),
        (
            "DBROVER_DATABASE",
            values.database.as_ref().or(fields.database.as_ref()),
        ),
        ("DBROVER_SCHEMA", values.schema.as_ref()),
        ("DBROVER_TABLE", values.table.as_ref()),
    ];
    let mut environment: Vec<(String, String)> = known
        .into_iter()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.clone())))
        .collect();
    environment.push((
        "DBROVER_DRIVER".to_owned(),
        driver_id(data_source.config.driver()).to_owned(),
    ));
    environment.push((
        "DBROVER_FORMAT".to_owned(),
        request.command.stdin.name().to_owned(),
    ));
    environment
}

/// Runs a filled-in program until it ends, its timeout passes or it is stopped, and reports
/// how it went.
pub(crate) async fn run(
    program: ProgramId,
    invocation: Invocation,
    timeout: Duration,
    stopped: oneshot::Receiver<()>,
    runner: Arc<dyn ProgramRunner>,
    events: EventSink,
) {
    tracing::info!("running {}", invocation.program);
    // At debug: the arguments may hold the values of selected cells.
    tracing::debug!(
        "{} arguments: {:?}",
        invocation.program,
        invocation.arguments
    );
    let name = invocation.program.clone();
    let result = tokio::select! {
        result = runner.run(invocation) => result,
        () = tokio::time::sleep(timeout) => Err(ProgramFailure::TimedOut { after: timeout }),
        // A stop, or the engine shutting down, which drops the sender.
        _ = stopped => Err(ProgramFailure::Stopped),
    };
    match &result {
        Ok(output) => tracing::info!(
            "{name} finished, writing {} bytes{}",
            output.stdout.len(),
            if output.truncated { ", cut short" } else { "" }
        ),
        Err(failure) => tracing::warn!("{name}: {failure}"),
    }
    events.send(Event::ProgramFinished { program, result });
}

/// The commands the user trusted, as `trusted_commands.toml` holds them. A file that can't be
/// read trusts nothing, so every command asks again.
pub(crate) async fn load_trusted(file: &TrustedCommandsFile) -> TrustedCommands {
    let task_file = file.clone();
    match blocking(move || task_file.load()).await {
        Ok(trusted) => trusted,
        Err(error) => {
            tracing::error!("{error}; every command asks before it runs");
            TrustedCommands::default()
        }
    }
}

/// Saves the trusted commands. One that can't be saved is still trusted until DbRover quits.
pub(crate) async fn save_trusted(file: &TrustedCommandsFile, trusted: &TrustedCommands) {
    let task_file = file.clone();
    let task_trusted = trusted.clone();
    if let Err(error) = blocking(move || task_file.save(&task_trusted)).await {
        tracing::error!("{error}");
    }
}

/// Whether the engine may run `request`: only what the user trusted, checked here as well as
/// in the UI, so a slip there can't skip the question.
pub(crate) fn is_trusted(trusted: &TrustedCommands, request: &ProgramRequest) -> bool {
    trusted.contains(&TrustedCommand::of(&request.action, &request.command))
}
