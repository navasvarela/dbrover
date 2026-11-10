//! The DbRover engine: owns every piece of I/O on its own tokio runtime and talks to the UI
//! only through [`Command`]s and [`Event`]s.
//!
//! The UI thread must never wait on a database, so it never calls into a driver directly.
//! It sends commands with [`EngineHandle::send`] and drains events with
//! [`EngineHandle::try_recv`] once per frame. See `docs/adr/0003-ui-engine-message-passing.md`.

mod cloud_sign_in;
mod command_loop;
mod config_files;
mod console_files;
mod diagram_files;
mod environment;
mod events;
mod export;
mod export_file;
mod export_programs;
mod handle;
mod history;
mod keymap;
mod open;
mod outside_changes;
mod password_commands;
mod passwords;
mod pictures;
mod programs;
mod projects;
mod protocol;
mod query_folders;
pub mod read_only;
pub mod read_write;
mod registry;
mod releases;
mod repaint;
mod report_explain;
mod report_files;
mod report_source;
mod report_worker;
#[cfg(test)]
mod repository_rename_tests;
mod schema_snapshot;
mod session;
mod settings;
mod sql_files;
mod stop_signals;
mod transfer;
mod tunnels;
mod updater;
mod whole_file;

pub use dbrover_ssh::HostKey;
pub use environment::ProcessEnvironment;
pub use export::{Export, ExportAll};
pub use export_programs::ExportProgramRequest;
pub use handle::{EngineConfig, EngineError, EngineHandle, IDLE_PING_AFTER};
pub use open::{OpenError, open_data_source, open_data_source_for_mcp};
pub use outside_changes::Noticed;
pub use password_commands::{PASSWORD_COMMAND_TIMEOUT, PasswordCommandError, PasswordCommands};
pub use passwords::PasswordNeeded;
pub use programs::{
    Invocation, ProcessRunner, ProgramFailure, ProgramId, ProgramOutput, ProgramRequest,
    ProgramRunner,
};
pub use protocol::{
    Command, ConsoleId, CostApproval, DatabaseFile, DdlFailure, DescribedTable, Document,
    EditFailure, Event, Exporter, ImportFailure, PasswordKept, PasswordUpdate, Picture,
    QueryFolderProblem, ReportSaveFailure, ReportText, RowEdit, RowsImport, RunId, RunOrigin,
    RunOutcome, RunPurpose, SessionState, StatementCheck, StatementFailure, StatementSummary,
    TableExport, TransactionEnd, TransactionMode,
};
pub use releases::{GithubReleases, Release, ReleaseError, ReleaseFeed};
pub use repaint::Repaint;
pub use report_source::{ReportSource, SourceAccess};
pub use report_worker::{RunStopper, WorkerRun};
pub use schema_snapshot::{read_schema, read_snapshot_file};
pub use sql_files::WatchedFile;
pub use stop_signals::StopSignals;
pub use tunnels::Tunnels;
pub use updater::{
    InstallError, InstallSupport, InstalledUpdate, PackagerInstaller, Restart, StagedInstaller,
    UpdateInstaller,
};
pub use whole_file::write_whole;
