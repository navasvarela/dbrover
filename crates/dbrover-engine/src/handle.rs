use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dbrover_store::{
    ConsoleFiles, CustomFiles, DataSourceFile, DiagramFiles, HistoryFile, KeymapFile, ProjectFile,
    SecretStore, SettingsFile, TrustedCommandsFile,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, error::TryRecvError};
use tokio::task::{JoinError, JoinHandle};

use crate::command_loop;
use crate::events::EventSink;
use crate::{Command, Event, ProgramRunner, ReleaseFeed, Repaint, UpdateInstaller};

/// How long [`EngineHandle::shutdown`] waits for in-flight work before abandoning it.
///
/// Chosen by feel, not measured: long enough for a cancel request to reach a database server
/// once queries exist (M3), short enough that quitting never feels stuck.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("could not start the engine runtime")]
    Runtime(#[source] std::io::Error),
    #[error("the engine is no longer running")]
    Stopped,
    #[error("the engine's command loop failed")]
    CommandLoopFailed(#[source] JoinError),
    #[error("the engine did not stop within {0:?}")]
    ShutdownTimedOut(Duration),
}

/// Where the engine keeps what it persists, and where it learns about new releases.
pub struct EngineConfig {
    pub data_sources: DataSourceFile,
    pub projects: ProjectFile,
    pub secrets: Arc<dyn SecretStore>,
    pub consoles: ConsoleFiles,
    pub diagrams: DiagramFiles,
    pub history: HistoryFile,
    pub keymap: KeymapFile,
    pub settings: SettingsFile,
    /// `actions.toml`, `snippets.toml` and `theme.toml`.
    pub custom_files: CustomFiles,
    /// The command actions the user trusted to run.
    pub trusted_commands: TrustedCommandsFile,
    /// What starts command actions' programs, normally [`crate::ProcessRunner`].
    pub programs: Arc<dyn ProgramRunner>,
    pub releases: Arc<dyn ReleaseFeed>,
    pub updater: Arc<dyn UpdateInstaller>,
    /// OpenSSH's `known_hosts`, which SSH tunnels check host keys against.
    pub known_hosts: PathBuf,
    /// How long a connection may sit unused before it is pinged ahead of its next request,
    /// normally [`IDLE_PING_AFTER`].
    pub idle_ping_after: Duration,
}

/// A connection unused for this long is pinged before its next request, so a statement isn't
/// sent down one the server has dropped and the session says it is gone instead. A minute:
/// long enough that working steadily never pays for a ping, short enough to catch a server
/// restarted during a coffee break. A ping is one round trip.
pub const IDLE_PING_AFTER: Duration = Duration::from_secs(60);

/// The UI's connection to a running engine.
///
/// Owns the engine's tokio runtime, so the engine lives exactly as long as this handle.
pub struct EngineHandle {
    runtime: Runtime,
    commands: UnboundedSender<Command>,
    events: UnboundedReceiver<Event>,
    command_loop: JoinHandle<()>,
}

impl EngineHandle {
    /// Starts the engine runtime and its command loop.
    ///
    /// The engine loads the data sources from `config` and reports them as its first events.
    /// `repaint` is called after every event so an idle UI wakes up to drain it.
    ///
    /// # Errors
    ///
    /// [`EngineError::Runtime`] if the OS refuses to create the runtime's worker threads.
    pub fn start(config: EngineConfig, repaint: impl Repaint) -> Result<Self, EngineError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("dbrover-engine")
            .enable_all()
            .build()
            .map_err(EngineError::Runtime)?;
        let (commands, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, events) = mpsc::unbounded_channel();
        let command_loop = runtime.spawn(command_loop::run(
            config,
            command_receiver,
            EventSink::new(event_sender, repaint),
        ));

        Ok(Self {
            runtime,
            commands,
            events,
            command_loop,
        })
    }

    /// Queues a command for the engine. Never blocks.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] if the command loop has exited.
    pub fn send(&self, command: Command) -> Result<(), EngineError> {
        self.commands
            .send(command)
            .map_err(|_| EngineError::Stopped)
    }

    /// Takes the next pending event, if any. Never blocks.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] if the command loop has exited and every event it sent has
    /// already been received.
    pub fn try_recv(&mut self) -> Result<Option<Event>, EngineError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(EngineError::Stopped),
        }
    }

    /// Stops the engine: closes the command channel so the command loop finishes, waits for
    /// it up to a short timeout, then shuts the runtime down.
    ///
    /// Must be called from outside the engine runtime (the UI thread), because it blocks.
    ///
    /// # Errors
    ///
    /// [`EngineError::CommandLoopFailed`] if the command loop panicked, or
    /// [`EngineError::ShutdownTimedOut`] if it did not finish in time.
    pub fn shutdown(self) -> Result<(), EngineError> {
        self.shutdown_within(SHUTDOWN_TIMEOUT)
    }

    /// [`EngineHandle::shutdown`] with a different wait. Tests use a longer one: on a busy CI
    /// runner (Windows especially), closing the history database took longer than the app's
    /// two seconds, and tests that restart the engine failed with `ShutdownTimedOut`.
    pub fn shutdown_within(self, timeout: Duration) -> Result<(), EngineError> {
        let Self {
            runtime,
            commands,
            events: _,
            command_loop,
        } = self;
        drop(commands);

        let finished =
            runtime.block_on(async { tokio::time::timeout(timeout, command_loop).await });
        runtime.shutdown_background();

        match finished {
            Ok(Ok(())) => Ok(()),
            Ok(Err(join_error)) => Err(EngineError::CommandLoopFailed(join_error)),
            Err(_elapsed) => Err(EngineError::ShutdownTimedOut(timeout)),
        }
    }
}
