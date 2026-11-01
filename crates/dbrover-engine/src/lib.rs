//! The DbRover engine: owns every piece of I/O on its own tokio runtime and talks to the UI
//! only through [`Command`]s and [`Event`]s.
//!
//! The UI thread must never wait on a database, so it never calls into a driver directly.
//! It sends commands with [`EngineHandle::send`] and drains events with
//! [`EngineHandle::try_recv`] once per frame. See `docs/adr/0003-ui-engine-message-passing.md`.

mod command_loop;
mod config_files;
mod console_files;
mod diagram_files;
mod environment;
mod events;
mod export;
mod export_file;
mod handle;
mod history;
mod keymap;
mod open;
mod password_commands;
mod passwords;
mod programs;
mod projects;
mod protocol;
pub mod read_only;
pub mod read_write;
mod registry;
mod releases;
mod repaint;
#[cfg(test)]
mod repository_rename_tests;
mod session;
mod settings;
mod sql_files;
mod transfer;
mod tunnels;
mod updater;

pub use dbrover_ssh::HostKey;
pub use environment::ProcessEnvironment;
pub use export::{Export, ExportAll};
pub use handle::{EngineConfig, EngineError, EngineHandle, IDLE_PING_AFTER};
pub use open::{OpenError, open_data_source, open_data_source_for_mcp};
pub use password_commands::{PASSWORD_COMMAND_TIMEOUT, PasswordCommandError, PasswordCommands};
pub use passwords::PasswordNeeded;
pub use programs::{
    Invocation, ProcessRunner, ProgramFailure, ProgramId, ProgramOutput, ProgramRequest,
    ProgramRunner,
};
pub use protocol::{
    Command, ConsoleId, DatabaseFile, DdlFailure, DescribedTable, DiagramPicture, EditFailure,
    Event, Exporter, ImportFailure, PasswordKept, PasswordUpdate, RowEdit, RowsImport, RunId,
    RunOrigin, RunOutcome, RunPurpose, SessionState, StatementCheck, StatementFailure,
    StatementSummary, TableExport, TransactionEnd, TransactionMode,
};
pub use releases::{GithubReleases, Release, ReleaseError, ReleaseFeed};
pub use repaint::Repaint;
pub use sql_files::WatchedFile;
pub use tunnels::Tunnels;
pub use updater::{
    InstallError, InstallSupport, InstalledUpdate, PackagerInstaller, Restart, StagedInstaller,
    UpdateInstaller,
};
