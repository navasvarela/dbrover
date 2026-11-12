//! The app's side of `dbrover report-worker` (`docs/reporting.md`, "The worker in RP1"):
//! starts the process, writes it the request, and turns what it prints into [`RunEvent`]s,
//! ending with one that [`RunEvent::ends_run`], whatever happens to the process.
//!
//! Three threads per run, each blocking on one thing:
//! - the worker's standard output, one event a line, until it closes, then the exit status;
//! - its standard error, a log line at a time, into the app's log;
//! - a watchdog that wakes each second until the run ends, and kills the worker at the run's
//!   time limit, once its folders hold more than its disk budget (RS3b), or once it has gone
//!   without progress for its stall timeout (RS3e); a run that goes on long enough, it keeps
//!   the machine awake for (RS3e).
//!
//! Threads rather than tasks: the worker is a child process, whose pipes and exit status are
//! blocking std calls, and a run ties up nothing of the engine's runtime.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use dbrover_report::{
    DiskBudget, LimitHit, RunEvent, RunFailure, RunRequest, Step, TimedEvent, WorkKept,
    keep_work_folder,
};

use crate::keep_awake::KeepAwake;
use crate::report_activity::Activity;
use crate::report_disk::folder_size;

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

/// How often the watchdog looks at a run: soon enough after a limit that a run past its disk
/// budget writes little more, and seldom enough that adding up its folders costs nothing.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// Why the app stopped the worker, if it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Stop {
    No = 0,
    Cancelled = 1,
    TimedOut = 2,
    /// Its folders held more than its disk budget.
    OutOfDisk = 3,
    /// It went without progress for its stall timeout.
    Stalled = 4,
}

impl Stop {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Cancelled,
            2 => Self::TimedOut,
            3 => Self::OutOfDisk,
            4 => Self::Stalled,
            _ => Self::No,
        }
    }
}

/// What the app watches a run for, and stops it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWatch {
    /// The whole run's time limit.
    pub timeout: Duration,
    /// How long the run may go without progress (`report_activity`).
    pub stall_timeout: Duration,
    /// What the run's work folder and staging folder may hold together; `None` watches no
    /// disk, as when explaining a report, which writes no file.
    pub disk: Option<DiskBudget>,
    /// What keeps the machine awake during a long run; `None` lets it sleep.
    pub keep_awake: Option<KeepAwake>,
    /// What the finished units of a worker this side killed must have read for its work
    /// folder to be kept, so the run can resume (RS5c); `None` never keeps it, as when
    /// explaining a report.
    pub resumable_above: Option<u64>,
}

/// What the watchdog needs of a run: its limits, the folders it writes, its progress, and how
/// to stop it.
struct Watched {
    watch: RunWatch,
    folders: [PathBuf; 2],
    activity: Arc<Mutex<Activity>>,
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
    /// What the folders held when the watchdog stopped the run for its disk.
    disk_used: Arc<AtomicU64>,
}

/// A report running in a worker process.
pub struct WorkerRun {
    events: Receiver<TimedEvent>,
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
    /// Dropped when the run ends, which wakes the watchdog to leave.
    _watchdog: Sender<()>,
}

impl WorkerRun {
    /// Starts `command` (the app's own executable, as `report-worker`) and sends it
    /// `request`, watching it for `watch`'s limits; `wake` is called after every event, so the
    /// UI can look.
    ///
    /// # Errors
    ///
    /// If the process can't be started or sent the request.
    pub fn start(
        mut command: Command,
        request: &RunRequest,
        watch: RunWatch,
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
        let started = Instant::now();
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
        let disk_used = Arc::new(AtomicU64::new(0));
        let activity = Arc::new(Mutex::new(Activity::new(started)));
        spawn_log_reader(stderr, Arc::clone(&log));
        spawn_watchdog(
            stopped,
            Watched {
                watch: watch.clone(),
                folders: [request.work.clone(), request.staging.clone()],
                activity: Arc::clone(&activity),
                child: Arc::clone(&child),
                stop: Arc::clone(&stop),
                disk_used: Arc::clone(&disk_used),
            },
            started,
        );
        let reader = OutputReader {
            activity,
            run_ended: watchdog.clone(),
            folders: [request.staging.clone(), request.work.clone()],
            child: Arc::clone(&child),
            stop: Arc::clone(&stop),
            log,
            events,
            wake,
            watch,
            disk_used,
            started,
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
    pub fn try_next(&self) -> Option<TimedEvent> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting up to `wait` for it.
    pub fn next_within(&self, wait: Duration) -> Option<TimedEvent> {
        self.events.recv_timeout(wait).ok()
    }

    /// The next event, waiting for it as long as the run lasts; `None` once the event that
    /// ends the run has been taken.
    pub fn wait_next(&self) -> Option<TimedEvent> {
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

/// Looks at the run every [`WATCH_EVERY`] until `stopped` says it ended, kills the worker at
/// the first limit it passes, and keeps the machine awake once the run has gone on long enough.
fn spawn_watchdog(stopped: Receiver<()>, watched: Watched, started: Instant) {
    let watch = move || {
        let mut awake = KeptAwake::default();
        // Until the run ends, or the run that started the watchdog is gone.
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(WATCH_EVERY) {
            let elapsed = started.elapsed();
            if let Some(stop) = watched.passed(elapsed) {
                watched.stop(stop);
                break;
            }
            if let Some(keep_awake) = &watched.watch.keep_awake
                && elapsed >= keep_awake.after
            {
                awake.start(keep_awake, &watched.child);
            }
        }
        awake.stop();
    };
    if let Err(error) = thread::Builder::new()
        .name("report-worker-watchdog".to_owned())
        .spawn(watch)
    {
        tracing::warn!("could not watch the report worker: {error}");
    }
}

/// The program keeping the machine awake for a run, once started.
#[derive(Default)]
struct KeptAwake {
    started: bool,
    program: Option<Child>,
}

impl KeptAwake {
    /// Starts `keep_awake` on the worker, the first time only.
    fn start(&mut self, keep_awake: &KeepAwake, worker: &Mutex<Child>) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let pid = worker.lock().unwrap_or_else(PoisonError::into_inner).id();
        match keep_awake.command(pid).spawn() {
            Ok(program) => {
                tracing::info!("keeping the machine awake while the report runs");
                self.program = Some(program);
            }
            Err(error) => tracing::warn!("could not keep the machine awake: {error}"),
        }
    }

    /// Ends the program, which would end with the worker anyway.
    fn stop(self) {
        if let Some(mut program) = self.program {
            // One that has ended already can't be killed, which is what was wanted.
            let _ = program.kill();
            let _ = program.wait();
        }
    }
}

impl Watched {
    /// The limit the run has passed after `elapsed`, if it has.
    fn passed(&self, elapsed: Duration) -> Option<Stop> {
        if elapsed >= self.watch.timeout {
            return Some(Stop::TimedOut);
        }
        let idle = self
            .activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .idle(Instant::now());
        if idle >= self.watch.stall_timeout {
            return Some(Stop::Stalled);
        }
        let budget = self.watch.disk?;
        let used: u64 = self.folders.iter().map(|folder| folder_size(folder)).sum();
        if used > budget.bytes() {
            self.disk_used.store(used, Ordering::SeqCst);
            return Some(Stop::OutOfDisk);
        }
        None
    }

    /// Kills the worker for `stop`, unless the run was stopped already.
    fn stop(&self, stop: Stop) {
        let first = self.stop.compare_exchange(
            Stop::No as u8,
            stop as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        if first.is_ok() {
            kill(&self.child);
        }
    }
}

/// Reads the worker's events until it exits, then says how the run ended if the worker
/// didn't.
struct OutputReader {
    /// The run's last progress, which each event may move on.
    activity: Arc<Mutex<Activity>>,
    /// Tells the watchdog the worker has exited, so it stops watching, and lets the machine
    /// sleep again, even while the run's events are still being read.
    run_ended: Sender<()>,
    /// The run's staging folder and work folder, which a worker that ended without saying so
    /// may have left.
    folders: [PathBuf; 2],
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicU8>,
    log: Arc<Mutex<Vec<String>>>,
    events: Sender<TimedEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    watch: RunWatch,
    disk_used: Arc<AtomicU64>,
    /// When the worker was started, for the time of an event this side makes.
    started: Instant,
}

impl OutputReader {
    fn read(self, stdout: ChildStdout) {
        let mut ended = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<TimedEvent>(&line) {
                Ok(timed) => {
                    ended |= timed.event.ends_run();
                    self.activity
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .observe(&timed, Instant::now());
                    self.send(timed);
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
        // The watchdog may have left already, which is all this asks.
        let _ = self.run_ended.send(());
        if ended {
            return;
        }
        if let Some(kept) = self.keep_or_remove_folders() {
            self.send(RunEvent::WorkKept(kept).at(self.started.elapsed()));
        }
        let event = match Stop::from_u8(self.stop.load(Ordering::SeqCst)) {
            Stop::Cancelled => RunEvent::Cancelled,
            Stop::TimedOut => RunEvent::Failed(timed_out(self.watch.timeout)),
            Stop::Stalled => RunEvent::Failed(stalled(self.watch.stall_timeout)),
            Stop::OutOfDisk => {
                let used = self.disk_used.load(Ordering::SeqCst);
                match self.watch.disk {
                    Some(budget) => RunEvent::Failed(budget.exceeded(used)),
                    None => RunEvent::Failed(RunFailure::new(
                        Step::Worker,
                        "the run used more disk than it may, and was stopped",
                    )),
                }
            }
            Stop::No => {
                let log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
                RunEvent::Failed(RunFailure::new(Step::Worker, stopped(status, &log)))
            }
        };
        self.send(event.at(self.started.elapsed()));
    }

    /// Removes what a killed worker had started writing, before the run's end is sent, so the
    /// output folder is as it was but for the files a partitioned run's finished ranges put in
    /// place by the time anyone looks. The work folder goes too, unless its manifest says the
    /// run had finished enough to be worth resuming: then it is kept, and said.
    fn keep_or_remove_folders(&self) -> Option<WorkKept> {
        let [staging, work] = &self.folders;
        remove_folder(staging);
        let kept = self.watch.resumable_above.and_then(|above| {
            keep_work_folder(work, above, false)
                .inspect_err(|error| {
                    tracing::warn!("could not keep the run's work folder: {error}");
                })
                .ok()
                .flatten()
        });
        match kept {
            Some(kept) => {
                tracing::info!("kept {} to resume the run", work.display());
                Some(kept)
            }
            None => {
                remove_folder(work);
                None
            }
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

    fn send(&self, event: TimedEvent) {
        // The receiver is gone once the app stops watching the run, which is fine.
        let _ = self.events.send(event);
        (self.wake)();
    }
}

fn remove_folder(folder: &Path) {
    match std::fs::remove_dir_all(folder) {
        Ok(()) => tracing::info!("removed {}", folder.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            "could not remove the report's folder {}: {error}",
            folder.display()
        ),
    }
}

fn timed_out(timeout: Duration) -> RunFailure {
    let value = friendly(timeout);
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

fn stalled(stall_timeout: Duration) -> RunFailure {
    let value = friendly(stall_timeout);
    RunFailure::at_limit(
        Step::Worker,
        format!(
            "the run went {value} without a row, a batch or work in DuckDB, its stall_timeout, \
             and was stopped as stuck; a source that waits on its engine longer before its \
             first row needs stall_timeout raised with its timeout"
        ),
        LimitHit {
            key: "stall_timeout".to_owned(),
            value,
            raise_in: "[report]".to_owned(),
            set_in: None,
        },
    )
}

/// `10m`, `1h 30m`, as a report file writes a duration.
fn friendly(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
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

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use dbrover_report::{LimitDefaults, RunRequest};

    use super::*;

    fn request(folder: &Path) -> RunRequest {
        RunRequest {
            report: folder.join("report.toml"),
            values: Default::default(),
            folder: folder.to_owned(),
            staging: folder.join(".run"),
            work: folder.join("work"),
            defaults: LimitDefaults::default(),
            memory_limit: None,
            explain: None,
            resume: false,
        }
    }

    /// A stand-in for the worker that reads no request and sends nothing, then exits after
    /// `seconds`: `sleep` itself, not a shell running it, which some shells (dash, Ubuntu's
    /// `sh`) run as a child that keeps the output open once the shell is killed.
    fn stand_in(seconds: u32) -> Command {
        let mut command = Command::new("sleep");
        command.arg(seconds.to_string());
        command
    }

    fn watch(stall_timeout: Duration) -> RunWatch {
        RunWatch {
            timeout: Duration::from_secs(60),
            stall_timeout,
            disk: None,
            keep_awake: None,
            resumable_above: None,
        }
    }

    fn last_event(run: &WorkerRun) -> Option<RunEvent> {
        let mut last = None;
        while let Some(timed) = run.wait_next() {
            last = Some(timed.event);
        }
        last
    }

    #[test]
    fn stops_a_worker_that_sends_nothing_for_its_stall_timeout() {
        let folder = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let run = WorkerRun::start(
            stand_in(30),
            &request(folder.path()),
            watch(Duration::from_secs(1)),
            || {},
        )
        .unwrap();
        let Some(RunEvent::Failed(failure)) = last_event(&run) else {
            panic!("the run should fail");
        };
        assert_eq!(
            failure.limit.as_ref().map(|limit| limit.key.as_str()),
            Some("stall_timeout"),
            "{failure:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn keeps_the_machine_awake_with_its_program_until_the_worker_ends() {
        let folder = tempfile::tempdir().unwrap();
        let pids = folder.path().join("pids");
        // Writes the worker's pid as it was given, and its own, then waits far longer than the
        // run: the watchdog must end it.
        let script = format!("echo {{pid}} $$ > '{}'; exec sleep 30", pids.display());
        let mut watched = watch(Duration::from_secs(60));
        watched.keep_awake = Some(KeepAwake::with_program(
            Duration::ZERO,
            "sh",
            ["-c", script.as_str()],
        ));
        let run = WorkerRun::start(stand_in(3), &request(folder.path()), watched, || {}).unwrap();
        let worker = run.child.lock().unwrap().id();
        assert!(matches!(last_event(&run), Some(RunEvent::Failed(_))));
        let written = std::fs::read_to_string(&pids).unwrap();
        let mut written = written.split_whitespace();
        assert_eq!(written.next(), Some(worker.to_string().as_str()));
        let program = written.next().unwrap().to_owned();
        let deadline = Instant::now() + Duration::from_secs(5);
        while is_running(&program) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!is_running(&program), "the program ends with the run");
    }

    fn is_running(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}
