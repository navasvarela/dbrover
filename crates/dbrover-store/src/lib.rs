//! Where DbRover keeps its files and secrets. See `docs/persistence.md` for the full layout.
//!
//! Everything here blocks (file system, OS keychain). The engine calls it from tokio's
//! blocking pool, never from an async worker or the UI thread.

mod atomic;
mod config_text;
mod consoles;
mod csv;
mod custom;
mod data_sources;
mod diagrams;
mod history;
mod keymap;
mod paths;
mod portable;
mod previous_install;
mod projects;
mod secrets;
mod settings;
mod sql_files;

pub use consoles::{
    ConsoleFiles, ConsoleKey, ConsoleName, ConsoleStoreError, InvalidConsoleName, SavedConsole,
};
pub use csv::{CsvError, CsvReader, CsvRecord, Separator};
pub use custom::{
    ActionBody, ActionId, ActionTarget, Checked, CommandAction, CommandOutput, CustomAction,
    CustomFile, CustomFileKind, CustomFiles, DEFAULT_COMMAND_TIMEOUT, Rgb, Snippet, SnippetBody,
    StdinFormat, ThemeOverrides, ThemeToken, TrustError, TrustedCommand, TrustedCommands,
    TrustedCommandsFile, driver_id, parse_actions, parse_snippets, parse_theme,
};
pub use data_sources::{DataSourceFile, StoreError};
pub use diagrams::{
    DiagramFile, DiagramFiles, DiagramFolder, DiagramKey, DiagramName, DiagramStoreError,
    InvalidDiagramName, PlacedTable, SavedDiagram,
};
pub use history::{
    History, HistoryEntry, HistoryError, HistoryExportError, HistoryFile, HistoryOutcome,
    HistoryQuery, HistorySource, HistorySourceKind, NamedCell, ReplacedCell, RowChange,
    RowChangeKind, redact_secrets,
};
pub use keymap::{KeymapBindings, KeymapError, KeymapFile};
pub use paths::{APP_NAME, AppPaths, PREVIOUS_APP_NAME};
pub use portable::{
    DataSourceImport, Difference, EntryStatus, ImportChoice, ImportChoices, ImportPlan,
    ImportPreview, Incoming, MissingPath, PortableError, ProjectImport, read_export, write_export,
};
pub use previous_install::{
    Adoption, AdoptionError, CopiedInstall, InstallFolders, adopt_previous_install,
};
pub use projects::{
    InvalidSchemaScope, InvalidSlug, InvalidTimeout, McpOverrides, McpPermissions, Project,
    ProjectFile, ProjectSlug, ProjectStoreError, Projects, SchemaScope, StatementTimeout,
};
pub use secrets::{Secret, SecretError, SecretStore, os_keychain};
pub use settings::{
    FormatSettings, HistorySettings, KeywordCase, LintSettings, Settings, SettingsError,
    SettingsFile, UpdateSettings,
};
pub use sql_files::{SqlFileContents, SqlFileError, modified, read_sql_file, write_sql_file};
