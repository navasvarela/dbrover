//! The app's side of `dbrover report-worker` (`docs/reporting.md`, "The worker in RP1"):
//! starts the process, writes it the request, and turns what it prints into [`RunEvent`]s,
//! ending with one that [`RunEvent::ends_run`], whatever happens to the process.
//!
//! Three threads per run, each blocking on one thing:
//! - the worker's standard output, one event a line, until it closes, then the exit status;
//! - its standard error, a log line at a time, into the app's log;
//! - a watchdog that kills the worker at the run's time limit, unless the run ends first.
//!
//! Threads rather than tasks: the worker is a child process, whose pipes and exit status are
//! blocking std calls, and a run ties up nothing of the engine's runtime.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use dbrover_report::{LimitHit, RunEvent, RunFailure, RunRequest, Step};

/// Windows' `BELOW_NORMAL_PRIORITY_CLASS`: a report gives way to the app and everything else
/// the user is doing. On Linux and macOS the worker lowers its own priority.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// How many of the worker's last log lines are kept, to say why it stopped when it couldn't
/// say so itself (a panic, or memory that ran out).
const KEPT_LOG_LINES: usize = 5;

/// How often a worker that closed its output is asked whether it has exited. It exits right
/// after its last line, so this is waited once or twice.
const EXIT_POLL: Duration = Duration::from_millis(20);

/// Why the app stopped the worker, if it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Stop {
    No = 0,
    Cancelled = 1,
    TimedOut = 2,
}

impl Stop {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Cancelled,
            2 => Self::TimedOut,
            _ => Self::No,
        }
    }
}

/// A report running in a worker process.
pub struct WorkerRun {
    events: Receiver<RunEvent>,
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
    /// Dropped when the run ends, which wakes the watchdog to leave.
    _watchdog: Sender<()>,
}

impl WorkerRun {
    /// Starts `command` (the app's own executable, as `report-worker`) and sends it
    /// `request`. `timeout` is the whole run's limit; `wake` is called after every event, so
    /// the UI can look.
    ///
    /// # Errors
    ///
    /// If the process can't be started or sent the request.
    pub fn start(
        mut command: Command,
        request: &RunRequest,
        timeout: Duration,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::programs::CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let mut child = command.spawn()?;
        let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
        line.push('\n');
        let sent = match child.stdin.take() {
            // Dropping stdin after the request closes it: the worker reads one line.
            Some(mut stdin) => stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush()),
            None => Err(io::Error::other("the worker has no standard input")),
        };
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(child));
        let stop = Arc::new(AtomicU8::new(Stop::No as u8));
        if let Err(error) = sent {
            kill(&child);
            return Err(error);
        }
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            kill(&child);
            return Err(io::Error::other("the worker's output wasn't piped"));
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let (events, received) = mpsc::channel();
        let (watchdog, stopped) = mpsc::channel();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        spawn_log_reader(stderr, Arc::clone(&log));
        spawn_watchdog(stopped, timeout, Arc::clone(&child), Arc::clone(&stop));
        let reader = OutputReader {
            staging: request.staging.clone(),
            child: Arc::clone(&child),
            stop: Arc::clone(&stop),
            log,
            events,
            wake,
            timeout,
        };
        thread::Builder::new()
            .name("report-worker-out".to_owned())
            .spawn(move || reader.read(stdout))?;
        Ok(Self {
            events: received,
            child,
            stop,
            _watchdog: watchdog,
        })
    }

    /// The next event, if one has come.
    pub fn try_next(&self) -> Option<RunEvent> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting up to `wait` for it.
    pub fn next_within(&self, wait: Duration) -> Option<RunEvent> {
        self.events.recv_timeout(wait).ok()
    }

    /// The next event, waiting for it as long as the run lasts; `None` once the event that
    /// ends the run has been taken.
    pub fn wait_next(&self) -> Option<RunEvent> {
        self.events.recv().ok()
    }

    /// Stops the worker. The run ends with [`RunEvent::Cancelled`] unless it had ended.
    pub fn cancel(&self) {
        self.stopper().cancel();
    }

    /// What stops the run from another thread, such as one waiting for a signal.
    pub fn stopper(&self) -> RunStopper {
        RunStopper {
            child: Arc::clone(&self.child),
            stop: Arc::clone(&self.stop),
        }
    }
}

/// Cancels a [`WorkerRun`] from any thread, as [`WorkerRun::cancel`] does.
#[derive(Clone)]
pub struct RunStopper {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
}

impl RunStopper {
    /// Stops the worker. The run ends with [`RunEvent::Cancelled`] unless it had ended.
    pub fn cancel(&self) {
        let _ = self.stop.compare_exchange(
            Stop::No as u8,
            Stop::Cancelled as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        kill(&self.child);
    }
}

impl Drop for WorkerRun {
    /// A run nobody watches any more is stopped, so closing its tab never leaves a worker
    /// behind. Killing one that has exited does nothing.
    fn drop(&mut self) {
        kill(&self.child);
    }
}

fn kill(child: &Mutex<Child>) {
    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
    // A worker that has exited can't be killed, which is what was wanted.
    let _ = child.kill();
}

fn spawn_log_reader(stderr: ChildStderr, log: Arc<Mutex<Vec<String>>>) {
    let read = move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            tracing::info!(target: "dbrover::report_worker", "{line}");
            let mut kept = log.lock().unwrap_or_else(PoisonError::into_inner);
            if kept.len() == KEPT_LOG_LINES {
                kept.remove(0);
            }
            kept.push(line);
        }
    };
    if let Err(error) = thread::Builder::new()
        .name("report-worker-log".to_owned())
        .spawn(read)
    {
        tracing::warn!("could not read the report worker's log: {error}");
    }
}

/// Kills the worker once `timeout` passes, unless `stopped` says the run ended first.
fn spawn_watchdog(
    stopped: Receiver<()>,
    timeout: Duration,
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
) {
    let watch = move || {
        if let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(timeout) {
            let first = stop.compare_exchange(
                Stop::No as u8,
                Stop::TimedOut as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            if first.is_ok() {
                kill(&child);
            }
        }
    };
    if let Err(error) = thread::Builder::new()
        .name("report-worker-watchdog".to_owned())
        .spawn(watch)
    {
        tracing::warn!("could not watch the report worker's time: {error}");
    }
}

/// Reads the worker's events until it exits, then says how the run ended if the worker
/// didn't.
struct OutputReader {
    /// The run's staging folder, which a worker that ended without saying so may have left.
    staging: PathBuf,
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
    log: Arc<Mutex<Vec<String>>>,
    events: Sender<RunEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    timeout: Duration,
}

impl OutputReader {
    fn read(self, stdout: ChildStdout) {
        let mut ended = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<RunEvent>(&line) {
                Ok(event) => {
                    ended |= event.ends_run();
                    self.send(event);
                }
                Err(error) => {
                    tracing::warn!(
                        "the report worker printed something that isn't an event ({error}): {line}"
                    );
                }
            }
        }
        // Standard output closed: the worker is exiting, or was killed.
        let status = self.wait();
        if ended {
            return;
        }
        self.remove_staging();
        let event = match Stop::from_u8(self.stop.load(Ordering::SeqCst)) {
            Stop::Cancelled => RunEvent::Cancelled,
            Stop::TimedOut => RunEvent::Failed(timed_out(self.timeout)),
            Stop::No => {
                let log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
                RunEvent::Failed(RunFailure::new(Step::Worker, stopped(status, &log)))
            }
        };
        self.send(event);
    }

    /// Removes what a killed worker had started writing, before the run's end is sent, so the
    /// output folder is as it was by the time anyone looks.
    fn remove_staging(&self) {
        match std::fs::remove_dir_all(&self.staging) {
            Ok(()) => tracing::info!("removed {}", self.staging.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                "could not remove the report's staging folder {}: {error}",
                self.staging.display()
            ),
        }
    }

    /// The worker's exit status, polled rather than waited for, so **Cancel** can still take
    /// the lock to kill a worker that closed its output but keeps running.
    fn wait(&self) -> io::Result<ExitStatus> {
        loop {
            let exited = self
                .child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_wait();
            match exited {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => thread::sleep(EXIT_POLL),
                Err(error) => return Err(error),
            }
        }
    }

    fn send(&self, event: RunEvent) {
        // The receiver is gone once the app stops watching the run, which is fine.
        let _ = self.events.send(event);
        (self.wake)();
    }
}

fn timed_out(timeout: Duration) -> RunFailure {
    let value = match jiff::SignedDuration::try_from(timeout) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", timeout.as_secs()),
    };
    RunFailure::at_limit(
        Step::Worker,
        format!("the run passed its time limit of {value} and was stopped"),
        LimitHit {
            key: "timeout".to_owned(),
            value,
            raise_in: "[report]".to_owned(),
            set_in: None,
        },
    )
}

/// Why a worker that ended without saying so stopped, with the last thing it logged.
fn stopped(status: io::Result<ExitStatus>, log: &[String]) -> String {
    let how = match status {
        Ok(status) => describe_exit(status),
        Err(error) => format!("the worker's end couldn't be read: {error}"),
    };
    match log.last() {
        Some(last) => format!("{how}; its last words: {last}"),
        None => how,
    }
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return match signal {
                // SIGKILL: from outside, such as Linux's out-of-memory killer.
                9 => "the worker was killed (SIGKILL), as the system does when memory runs out"
                    .to_owned(),
                // SIGABRT: Rust aborts when an allocation fails, as under RLIMIT_DATA.
                6 => "the worker stopped (SIGABRT), as it does when memory runs out".to_owned(),
                // SIGXCPU: RLIMIT_CPU.
                24 => "the worker used all the processor time it was given (SIGXCPU)".to_owned(),
                other => format!("the worker was stopped by signal {other}"),
            };
        }
    }
    match status.code() {
        Some(code) => format!("the worker stopped with exit code {code} without saying why"),
        None => "the worker stopped without saying why".to_owned(),
    }
}
