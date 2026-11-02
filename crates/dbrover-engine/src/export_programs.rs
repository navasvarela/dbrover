//! Programs from `exports.toml` (W8d): filling one in for an export, checked against what the
//! user trusted, and running it with the rows streamed to its stdin as JSON Lines while its
//! stdout goes straight into the export's file. See `docs/exports.md`.
//!
//! An export writes its file on the blocking pool, a page at a time, so the program runs on
//! threads rather than tokio's tasks: one writes its stdin, one keeps the start of its stderr.
//! The writer reports each page written, which is how a program that stops taking input is
//! noticed and stopped after its timeout rather than holding the export forever.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dbrover_core::{
    DataSource, DataSourceFields, DataSourceId, TemplateValues, Variables, describe_error,
};
use dbrover_export::{ExportPipe, ExportProgram, ProgramSpec};
use dbrover_store::{TrustedCommand, TrustedCommands, driver_id};

use crate::programs::{Invocation, ProgramFailure, ProgramId, ProgramRunner, seconds};

/// stderr kept from a program, as H2's runs keep: an error message, not a log.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// How often to look whether a program that has had all its input has finished. Short enough
/// not to add to an export's time, long enough not to busy a thread of the blocking pool.
const EXIT_POLL: Duration = Duration::from_millis(10);

/// Copying the selected cells through a program from `exports.toml`: its output goes to the
/// clipboard rather than a file.
#[derive(Debug, Clone)]
pub struct ExportProgramRequest {
    pub program: ProgramId,
    pub data_source: DataSourceId,
    pub spec: ProgramSpec,
    /// The selected cells as JSON Lines.
    pub stdin: String,
}

/// Whether the user trusted `spec`'s program as it is now.
pub(crate) fn is_trusted(trusted: &TrustedCommands, spec: &ProgramSpec) -> bool {
    trusted.contains(&TrustedCommand::export(&spec.id, &spec.program))
}

/// `spec`'s program filled in for an export from `data_source`, ready to start when the file
/// is, through `runner`.
///
/// # Errors
///
/// [`ProgramFailure`] for a program that isn't trusted, or that can't be filled in.
pub(crate) fn prepare(
    spec: &ProgramSpec,
    trusted: &TrustedCommands,
    data_source: &DataSource,
    variables: &dyn Variables,
    runner: Arc<dyn ProgramRunner>,
) -> Result<Arc<dyn ExportProgram>, ProgramFailure> {
    if !is_trusted(trusted, spec) {
        return Err(ProgramFailure::NotTrusted);
    }
    Ok(Arc::new(Prepared {
        invocation: invocation(spec, data_source, variables, String::new())?,
        timeout: spec.program.timeout,
        runner,
    }))
}

/// Fills in `spec`'s command for rows from `data_source`: arguments from the placeholders, the
/// context in `DBROVER_*` variables, and `stdin` for a run that isn't streamed.
///
/// # Errors
///
/// [`ProgramFailure::Unfillable`] for a setting that doesn't resolve or a placeholder nothing
/// fills, such as `{table}` for rows of no one table.
pub(crate) fn invocation(
    spec: &ProgramSpec,
    data_source: &DataSource,
    variables: &dyn Variables,
    stdin: String,
) -> Result<Invocation, ProgramFailure> {
    let config = data_source
        .config
        .resolve(variables)
        .map_err(|error| ProgramFailure::Unfillable(describe_error(&error)))?;
    let fields = DataSourceFields::from(&config);
    let values = TemplateValues {
        schema: spec.table.as_ref().and_then(|table| table.schema.clone()),
        table: spec.table.as_ref().map(|table| table.name.clone()),
        data_source: data_source.name.clone(),
        ..TemplateValues::default()
    };
    let command = &spec.program.command;
    let arguments = command
        .render(&values, &fields)
        .map_err(|error| ProgramFailure::Unfillable(error.to_string()))?;
    let known = [
        ("DBROVER_DATA_SOURCE", Some(&data_source.name)),
        ("DBROVER_DATABASE", fields.database.as_ref()),
        ("DBROVER_SCHEMA", values.schema.as_ref()),
        ("DBROVER_TABLE", values.table.as_ref()),
    ];
    let mut environment: Vec<(String, String)> = known
        .into_iter()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.clone())))
        .collect();
    environment.extend([
        (
            "DBROVER_DRIVER".to_owned(),
            driver_id(data_source.config.driver()).to_owned(),
        ),
        ("DBROVER_FORMAT".to_owned(), "jsonl".to_owned()),
        ("DBROVER_EXPORT".to_owned(), spec.id.as_str().to_owned()),
    ]);
    Ok(Invocation {
        program: command.program().to_owned(),
        arguments,
        environment,
        stdin,
        directory: variables.home(),
    })
}

/// A program filled in for one export.
struct Prepared {
    invocation: Invocation,
    timeout: Duration,
    runner: Arc<dyn ProgramRunner>,
}

impl ExportProgram for Prepared {
    fn start(&self, path: &Path) -> io::Result<Box<dyn ExportPipe>> {
        let out = File::create(path)?;
        self.runner
            .start_export(self.invocation.clone(), out, self.timeout)
    }
}

/// A program running for an export, as a real process.
pub(crate) struct ProcessPipe {
    child: Child,
    program: String,
    timeout: Duration,
    /// Pages for the writer thread; dropped to end the program's input.
    pages: Option<Sender<Vec<u8>>>,
    /// The writer's word on each page: written, or why not.
    written: Receiver<io::Result<()>>,
    /// Pages sent that the writer hasn't reported on yet.
    in_flight: usize,
    /// The program closed its stdin, or ended: what it didn't read it didn't need, and its
    /// exit status says whether it worked.
    closed: bool,
    writer: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    /// Waited for, so dropping the pipe has nothing left to stop.
    ended: bool,
}

impl ProcessPipe {
    /// Starts `invocation` with its stdout going to `out`.
    ///
    /// # Errors
    ///
    /// The program couldn't start: one not found names the `PATH` it was looked for in.
    pub(crate) fn start(invocation: Invocation, out: File, timeout: Duration) -> io::Result<Self> {
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.arguments)
            .envs(
                invocation
                    .environment
                    .iter()
                    .map(|(name, value)| (name, value)),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::from(out))
            .stderr(Stdio::piped());
        if let Some(directory) = &invocation.directory {
            command.current_dir(directory);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::programs::CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| {
            io::Error::other(crate::programs::start_failure(&invocation.program, &error))
        })?;
        let (pages, waiting) = mpsc::channel();
        let (report, written) = mpsc::channel();
        let writer = child
            .stdin
            .take()
            .map(|stdin| std::thread::spawn(move || write_pages(stdin, &waiting, &report)));
        let stderr = child
            .stderr
            .take()
            .map(|stderr| std::thread::spawn(move || keep_start_of(stderr)));
        tracing::info!("running {} for an export", invocation.program);
        // At debug: the arguments may name tables and files.
        tracing::debug!(
            "{} arguments: {:?}",
            invocation.program,
            invocation.arguments
        );
        Ok(Self {
            child,
            program: invocation.program,
            timeout,
            pages: Some(pages),
            written,
            in_flight: 0,
            closed: false,
            writer,
            stderr,
            ended: false,
        })
    }

    /// Waits for the writer's word on the page in flight, if there is one.
    fn wait_for_writer(&mut self) -> io::Result<()> {
        if self.in_flight == 0 {
            return Ok(());
        }
        match self.written.recv_timeout(self.timeout) {
            Ok(Ok(())) => {}
            // A broken pipe: the program stopped reading.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => self.closed = true,
            Err(RecvTimeoutError::Timeout) => {
                return Err(io::Error::other(format!(
                    "{} took no more rows for {}, so it was stopped",
                    self.program,
                    seconds(self.timeout)
                )));
            }
        }
        self.in_flight -= 1;
        Ok(())
    }

    /// Waits for the program to end, for up to the timeout.
    fn wait_for_exit(&mut self) -> io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.ended = true;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(ProgramFailure::TimedOut {
                    after: self.timeout,
                }));
            }
            std::thread::sleep(EXIT_POLL);
        }
    }
}

impl ExportPipe for ProcessPipe {
    fn send(&mut self, input: &[u8]) -> io::Result<()> {
        self.wait_for_writer()?;
        if self.closed {
            return Ok(());
        }
        let sent = self
            .pages
            .as_ref()
            .is_some_and(|pages| pages.send(input.to_vec()).is_ok());
        if sent {
            self.in_flight += 1;
        } else {
            self.closed = true;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.wait_for_writer()?;
        // The writer ends with its channel, closing stdin, so the program sees the end.
        self.pages = None;
        if let Some(writer) = self.writer.take() {
            // A writer that panicked wrote less; the program's exit status says whether that
            // mattered.
            let _ = writer.join();
        }
        let status = self.wait_for_exit()?;
        let stderr = self
            .stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        if status.success() {
            tracing::info!("{} finished the export", self.program);
            return Ok(());
        }
        let failure = ProgramFailure::Exited {
            program: self.program.clone(),
            status: status.to_string(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        };
        tracing::warn!("{failure}");
        Err(io::Error::other(with_stderr(&failure)))
    }
}

impl Drop for ProcessPipe {
    /// An export cancelled, failed or finished early stops its program.
    fn drop(&mut self) {
        self.pages = None;
        if !self.ended {
            // Nothing more can be done about a program that won't be killed.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A failure as a sentence, with what the program wrote on stderr after it.
fn with_stderr(failure: &ProgramFailure) -> String {
    match failure {
        ProgramFailure::Exited { stderr, .. } if !stderr.trim().is_empty() => {
            format!("{failure}: {}", stderr.trim())
        }
        _ => failure.to_string(),
    }
}

/// Writes each page to the program's stdin as it comes, reporting each; stops at the first
/// that can't be written, and closes stdin when the pages end.
fn write_pages(mut stdin: ChildStdin, pages: &Receiver<Vec<u8>>, report: &Sender<io::Result<()>>) {
    for page in pages {
        let written = stdin.write_all(&page).and_then(|()| stdin.flush());
        let failed = written.is_err();
        if report.send(written).is_err() || failed {
            return;
        }
    }
}

/// Reads stderr to its end, keeping its first [`MAX_STDERR_BYTES`], so a chatty program never
/// blocks on a full pipe.
fn keep_start_of(mut stderr: ChildStderr) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0; 8192];
    while let Ok(read) = stderr.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
    kept
}
