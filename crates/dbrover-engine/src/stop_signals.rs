//! The signals that ask a command-line process to stop, for `dbrover report run`
//! (`docs/reporting.md`, "Running it"): Ctrl-C everywhere, Ctrl-Break on Windows, and on Unix
//! the SIGTERM that cron, systemd, `timeout` and CI runners send to stop a job and the SIGHUP
//! of a terminal that closes.
//!
//! Once listened for, these signals no longer end the process; whoever listens decides what a
//! stop does, so a run can be cancelled and its staging folder removed before the process
//! exits.

use std::io;
use std::thread;

use tokio::runtime::{Builder, Runtime};

/// The stop signals, listened for.
pub struct StopSignals {
    /// A small runtime of its own: tokio's signal handling needs a reactor to wake on.
    runtime: Runtime,
    listeners: Listeners,
}

impl StopSignals {
    /// Starts listening. From now on a stop signal no longer ends the process.
    ///
    /// # Errors
    ///
    /// If the runtime or a signal's handler can't be set up; the signals then end the process
    /// as before.
    pub fn listen() -> io::Result<Self> {
        let runtime = Builder::new_current_thread().enable_io().build()?;
        let listeners = {
            let _context = runtime.enter();
            Listeners::new()?
        };
        Ok(Self { runtime, listeners })
    }

    /// Calls `stop` on a thread of its own when the first stop signal comes. The thread waits
    /// as long as the process lives.
    ///
    /// # Errors
    ///
    /// If the thread can't be started.
    pub fn on_signal(self, stop: impl FnOnce() + Send + 'static) -> io::Result<()> {
        let Self {
            runtime,
            mut listeners,
        } = self;
        thread::Builder::new()
            .name("stop-signals".to_owned())
            .spawn(move || {
                let signal = runtime.block_on(listeners.first());
                tracing::info!("asked to stop by {signal}");
                stop();
            })
            .map(drop)
    }
}

#[cfg(unix)]
struct Listeners {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hang_up: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Listeners {
    fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hang_up: signal(SignalKind::hangup())?,
        })
    }

    /// The name of the first signal that comes.
    async fn first(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.hang_up.recv() => "SIGHUP",
        }
    }
}

#[cfg(windows)]
struct Listeners {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

#[cfg(windows)]
impl Listeners {
    fn new() -> io::Result<Self> {
        Ok(Self {
            ctrl_c: tokio::signal::windows::ctrl_c()?,
            ctrl_break: tokio::signal::windows::ctrl_break()?,
        })
    }

    /// The name of the first signal that comes.
    async fn first(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "Ctrl-C",
            _ = self.ctrl_break.recv() => "Ctrl-Break",
        }
    }
}
