use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dbrover_core::{
    CatalogPath, ChildrenPage, ChildrenQuery, ColumnInfo, DataSource, DataSourceId, DriverKind,
    Password, PasswordCommand, ProjectId, Rejection, RelationName, ResultShape, RoleDescription,
    Row, ServerInfo, TableDescription, TableName, TransactionStatus,
};
use dbrover_export::{ExportSpec, SqlTable};
use dbrover_sql::{ExistingTable, ParameterValues};
use dbrover_store::{
    ActionId, ConsoleKey, CustomFile, CustomFileKind, HistoryEntry, HistoryQuery, ImportPlan,
    ImportPreview, KeymapBindings, McpOverrides, McpPermissions, Project, Projects, RowChange,
    SavedConsole, Secret, Separator, Settings, SqlFileContents, TrustedCommand, TrustedCommands,
};

use crate::{
    Export, ExportAll, HostKey, InstalledUpdate, PasswordNeeded, ProgramFailure, ProgramId,
    ProgramOutput, ProgramRequest, Release, WatchedFile,
};

/// Identifies a query console. Chosen by the UI, unique for the app's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConsoleId(pub u64);

/// Identifies one execution in a console, so events from an older run can be told apart.
/// Chosen by the UI, unique for the app's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub u64);

/// A request from the UI to the engine.
///
/// Shutting the [`EngineHandle`](crate::EngineHandle) down closes the command channel, which is
/// how the UI asks the engine to stop.
#[derive(Debug)]
pub enum Command {
    /// Creates or updates (matched by id) a data source, storing its password and its SSH
    /// login's secret if they are set. Answered by [`Event::DataSourceSaved`] or
    /// [`Event::DataSourceSaveFailed`].
    SaveDataSource {
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
    },
    /// Disconnects, forgets the data source and removes its saved password.
    DeleteDataSource(DataSourceId),
    /// Adds a project, invisible to MCP, with a slug made from its name. The UI picks the id,
    /// so it can move a data source into the new project right after. Answered by
    /// [`Event::ProjectSaved`] or [`Event::ProjectSaveFailed`].
    CreateProject {
        id: ProjectId,
        name: String,
    },
    /// Changes a project's name, keeping its slug. Answered by [`Event::ProjectSaved`] or
    /// [`Event::ProjectSaveFailed`].
    RenameProject {
        id: ProjectId,
        name: String,
    },
    /// Sets whether MCP clients may use the project, and what it sets differently from the
    /// MCP defaults. Answered by [`Event::ProjectSaved`] or [`Event::ProjectSaveFailed`].
    SaveProjectMcp {
        id: ProjectId,
        mcp_enabled: bool,
        overrides: McpOverrides,
    },
    /// Replaces what projects allow MCP clients when they don't say otherwise. Answered by
    /// [`Event::McpDefaultsSaved`] or [`Event::McpDefaultsSaveFailed`].
    SaveMcpDefaults(McpPermissions),
    /// Saves the password of the data source's MCP login, or removes it with `None`. Only
    /// failures are answered, by [`Event::DataSourceSaveFailed`].
    SaveMcpPassword {
        id: DataSourceId,
        password: Option<Password>,
    },
    /// Moves the project's data sources to no project, then forgets the project. Never
    /// deletes a data source. Answered by an [`Event::DataSourceSaved`] per data source moved,
    /// then [`Event::ProjectDeleted`], or by [`Event::ProjectSaveFailed`].
    DeleteProject(ProjectId),
    /// Puts a data source in a project, or in no project. Answered by
    /// [`Event::DataSourceSaved`] or [`Event::DataSourceSaveFailed`].
    MoveToProject {
        data_source: DataSourceId,
        project: Option<ProjectId>,
    },
    /// Writes the data sources, the projects, and every project those data sources are in,
    /// to a file another machine can import. Never a password. Answered by
    /// [`Event::DataSourcesExported`].
    ExportDataSources {
        path: PathBuf,
        data_sources: Vec<DataSourceId>,
        projects: Vec<ProjectId>,
    },
    /// Reads an exported file and compares it with the data sources and projects here,
    /// changing nothing. Answered by [`Event::ImportPreviewed`].
    PreviewImport(PathBuf),
    /// Saves what the user chose to import: the projects first, then the data sources, each
    /// added or replacing the one with its id. Answered by an [`Event::ProjectSaved`] and
    /// [`Event::DataSourceSaved`] for each, then [`Event::ImportApplied`].
    ApplyImport(ImportPlan),
    /// Connects with these settings, and the secrets typed with them, without saving them,
    /// then disconnects. Answered by [`Event::ConnectionTested`].
    TestConnection {
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
    },
    Connect(DataSourceId),
    /// The password the user typed at the prompt that [`Event::PasswordNeeded`] opened, for
    /// the secret it asked for. Kept in memory until DbRover quits, and also saved to the
    /// keychain if `save` is set. The UI connects again afterwards. Answered by
    /// [`Event::PasswordProvided`].
    ProvidePassword {
        data_source: DataSourceId,
        secret: Secret,
        password: Password,
        save: bool,
    },
    /// Trusts the SSH server's host key that [`Event::HostKeyUnknown`] showed, by adding it
    /// to `known_hosts`. The UI connects again afterwards. Answered by
    /// [`Event::HostKeyTrusted`].
    TrustHostKey {
        data_source: DataSourceId,
        key: HostKey,
    },
    /// Trusts the data source's password command that [`Event::PasswordCommandUntrusted`]
    /// showed, as it was shown, by recording it in `trusted_commands.toml`. The UI connects
    /// again afterwards. Answered by [`Event::TrustedCommandsLoaded`].
    TrustPasswordCommand {
        data_source: DataSourceId,
        command: PasswordCommand,
    },
    /// Disconnects the explorer's connection and every console's connection to the data
    /// source, rolling back their open transactions.
    Disconnect(DataSourceId),
    /// Lists the children of a catalog node, or the part of them `query` asks for,
    /// connecting first if needed. Answered by [`Event::ChildrenLoaded`].
    LoadChildren {
        id: DataSourceId,
        path: CatalogPath,
        query: ChildrenQuery,
    },
    /// Answered by [`Event::DefinitionLoaded`].
    LoadDefinition {
        id: DataSourceId,
        path: CatalogPath,
    },
    /// Creates a new, empty database file, refusing to overwrite an existing one. Answered by
    /// [`Event::DatabaseFileCreated`].
    CreateDatabaseFile(DatabaseFile),
    /// Runs statements in order on the console's own connection, connecting it to
    /// `data_source` first if needed, and stops at the first failure. Each statement's first
    /// page of rows is reported; the last statement's remaining rows stay available through
    /// [`Command::FetchMore`] until the next run.
    ///
    /// With `parameters`, each statement's `:name` parameters take their values: bound where
    /// the driver binds them, written as literals where it doesn't. A statement whose
    /// parameter has no value fails without being sent. With none, statements go as typed.
    /// The history records each statement as typed, followed by a comment naming its values.
    Execute {
        console: ConsoleId,
        data_source: DataSourceId,
        run: RunId,
        statements: Vec<String>,
        parameters: ParameterValues,
        transactions: TransactionMode,
        purpose: RunPurpose,
        origin: RunOrigin,
    },
    /// Commits or rolls back the console's open transaction. Answered by
    /// [`Event::TransactionEnded`].
    EndTransaction {
        console: ConsoleId,
        end: TransactionEnd,
    },
    /// Reads the next page of the run's last statement.
    FetchMore {
        console: ConsoleId,
        run: RunId,
    },
    /// Stops the run's current statement, or discards its unread rows.
    Cancel {
        console: ConsoleId,
        run: RunId,
    },
    /// Connects a console whose connection was lost again, to the same data source, as a new
    /// session: whatever the old one had open is gone. A console that lost its connection
    /// refuses to run anything until then. Answered by [`Event::ConsoleSessionChanged`].
    ReconnectConsole(ConsoleId),
    /// Disconnects the console's connection, which rolls back its open transaction.
    CloseConsole(ConsoleId),
    /// Writes a console's text to its file. Saves are applied in the order sent. A failure is
    /// reported as [`Event::ConsoleSaveFailed`].
    SaveConsole {
        key: ConsoleKey,
        text: String,
    },
    /// Removes a console's file, if it exists.
    DeleteConsoleFile(ConsoleKey),
    /// Reads a `.sql` file from anywhere on disk, to show in a console. Answered by
    /// [`Event::SqlFileRead`]. Reads, saves and checks of `.sql` files happen, and are
    /// answered, in the order sent.
    OpenSqlFile(PathBuf),
    /// Writes a console's text to a `.sql` file, replacing it in one step. Saves are applied in
    /// the order sent. Answered by [`Event::SqlFileSaved`].
    SaveSqlFile {
        console: ConsoleKey,
        path: PathBuf,
        text: String,
    },
    /// Looks at whether `.sql` files changed on disk since the times given. Answered by an
    /// [`Event::SqlFileChanged`] for each one that did, before anything sent after it is.
    CheckSqlFiles(Vec<WatchedFile>),
    /// Resolves and describes a table on the explorer's connection, connecting first if
    /// needed, so a console's open result isn't disturbed. Answered by
    /// [`Event::TableDescribed`].
    DescribeTable {
        data_source: DataSourceId,
        table: RelationName,
    },
    /// Describes each table, in order, on the explorer's connection, connecting first if
    /// needed, for a diagram. Answered by one [`Event::TablesDescribed`] once all are. The
    /// explorer's connection serves its requests in order, so a caller with many tables sends
    /// a few at a time, letting the explorer's own requests through in between.
    DescribeTables {
        data_source: DataSourceId,
        tables: Vec<RelationName>,
    },
    /// Describes the role at `path` (a node of the roles folder) on the explorer's
    /// connection, for editing or dropping it. Answered by [`Event::RoleDescribed`].
    DescribeRole {
        data_source: DataSourceId,
        path: CatalogPath,
    },
    /// Runs statements that create or change database objects (the object helpers' DDL), in
    /// order, on the explorer's connection to the data source, connecting first if needed.
    /// Where the dialect's DDL is transactional they run in one transaction, so a failure
    /// undoes the ones before it; elsewhere each commits as it runs, and a failure stops at
    /// that statement. Refused on a read-only data source. Each statement sent is recorded in
    /// the history. Answered by [`Event::DdlApplied`].
    ApplyDdl {
        data_source: DataSourceId,
        statements: Vec<String>,
    },
    /// Loads the statement that creates the table, view or role at `path`, for Copy DDL, on
    /// the explorer's connection: the catalog's definition where the database keeps one, or
    /// for a table where it doesn't (PostgreSQL, SQL Server), one rebuilt from the table's
    /// description and its constraints. A role's is rebuilt from its description, without
    /// its password. Answered by [`Event::DdlLoaded`].
    LoadDdl {
        data_source: DataSourceId,
        path: CatalogPath,
    },
    /// Loads the table at `path` as Modify table starts from it, on the explorer's
    /// connection: its description and its constraints by name. Answered by
    /// [`Event::TableLoaded`].
    LoadTable {
        data_source: DataSourceId,
        path: CatalogPath,
    },
    /// Inserts the rows of a CSV file into a table, on the explorer's connection to the data
    /// source, all in one transaction: in batches of several rows per `INSERT`, and rolled back
    /// at the first failure, which names the line it was on. Refused on a read-only data
    /// source and where [`dbrover_sql::Dialect::import_refusal`] says. Reports
    /// [`Event::ImportProgress`] after each batch, then [`Event::RowsImported`].
    ImportRows {
        data_source: DataSourceId,
        import: RowsImport,
    },
    /// Runs the grid's generated `DELETE`s, `UPDATE`s and `INSERT`s on the console's
    /// connection, all or none: each must change exactly one row. Inside an open transaction
    /// they stay part of it (behind a savepoint); otherwise they commit together. Refused on a
    /// read-only data source. Each statement sent is recorded in the history, with its row's
    /// change. Answered by [`Event::EditsApplied`].
    ApplyEdits {
        console: ConsoleId,
        statements: Vec<RowEdit>,
    },
    /// Asks the console's own connection whether the database would accept each statement,
    /// without running any (see `Connection::validate`). Never connects: a console that
    /// isn't connected has every statement skipped. Answered by
    /// [`Event::StatementsValidated`].
    ValidateStatements {
        console: ConsoleId,
        statements: Vec<String>,
    },
    /// Reads `keymap.toml` again. Answered by [`Event::KeymapLoaded`].
    ReloadKeymap,
    /// Writes a new `keymap.toml` holding `text`, unless one exists, then reads it.
    /// Answered by [`Event::KeymapLoaded`].
    CreateKeymap {
        text: String,
    },
    /// Reads `settings.toml` again. Answered by [`Event::SettingsLoaded`].
    ReloadSettings,
    /// Replaces `settings.toml` with these settings, then reads it. Answered by
    /// [`Event::SettingsLoaded`].
    SaveSettings(Settings),
    /// Reads `actions.toml`, `snippets.toml` and `theme.toml` again. Answered by an
    /// [`Event::CustomFileLoaded`] for each.
    ReloadCustomFiles,
    /// Writes a new customisation file holding `text`, unless one exists, then reads it.
    /// Answered by [`Event::CustomFileLoaded`], with a problem if it couldn't be written.
    CreateCustomFile {
        kind: CustomFileKind,
        text: String,
    },
    /// Runs a command action's program on the node or cells it was chosen on, if the user has
    /// trusted it, with what the request names filled in. It runs until it ends, its timeout
    /// passes or [`Command::StopProgram`] stops it. Answered by [`Event::ProgramFinished`].
    RunProgram(Box<ProgramRequest>),
    /// Stops a running program, killing it. Answered by its [`Event::ProgramFinished`].
    StopProgram(ProgramId),
    /// Lets the command run without asking from now on, in place of whatever its action ran
    /// before, and saves that. Answered by [`Event::TrustedCommandsLoaded`].
    TrustCommand(TrustedCommand),
    /// Writes rows a console shows to a file. Answered by [`Event::ExportFinished`].
    ExportRows {
        console: ConsoleId,
        export: Export,
    },
    /// Runs a statement again on the console's connection and writes every row it returns to
    /// a file, a page at a time, so they are never all in memory. Unread rows of the console's
    /// last run are discarded first, as for any statement. On a read-only data source it runs
    /// as statements do there: checked, and in the database's read-only mode. Cancelled like a
    /// run, by [`Command::Cancel`] with the export's run, which removes the partial file.
    /// Recorded in the history. Reports [`Event::ExportProgress`] after each page, then
    /// [`Event::ExportFinished`].
    ExportAllRows {
        console: ConsoleId,
        export: ExportAll,
    },
    /// Writes every row of a table or view to a file, as `SELECT *` on the explorer's
    /// connection to the data source, a page at a time, as [`Command::ExportAllRows`] does.
    /// Cancelled by [`Command::CancelTableExport`]. Recorded in the history. Reports
    /// [`Event::ExportProgress`] after each page, then [`Event::ExportFinished`].
    ExportTable {
        data_source: DataSourceId,
        export: TableExport,
    },
    /// Stops a [`Command::ExportTable`] and removes its partial file.
    CancelTableExport {
        data_source: DataSourceId,
        run: RunId,
    },
    /// Runs a statement from a console straight into a file, as [`Command::ExportAllRows`]
    /// does, starting the console's connection as [`Command::Execute`] does if it has none:
    /// Run to file, which may be the console's first statement.
    RunToFile {
        console: ConsoleId,
        data_source: DataSourceId,
        export: ExportAll,
    },
    /// Looks up past statements, newest first. Answered by [`Event::HistoryFound`].
    SearchHistory(HistoryQuery),
    /// Writes every statement a search finds, without its usual limit, to a file, naming data
    /// sources by `names`. Answered by [`Event::HistoryExported`].
    ExportHistory {
        query: HistoryQuery,
        path: PathBuf,
        spec: ExportSpec,
        names: HashMap<DataSourceId, String>,
    },
    /// Asks the release feed for the newest published release. Answered by
    /// [`Event::LatestRelease`].
    CheckForUpdates,
    /// Downloads the newest release and installs it over this copy, to start at the next
    /// launch. Reports [`Event::UpdateProgress`] while downloading, then
    /// [`Event::UpdateInstalled`].
    InstallUpdate,
}

/// One table [`Command::DescribeTables`] asked for, as it names it, and its description:
/// `None` if there is no such plain table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribedTable {
    pub table: RelationName,
    pub result: Result<Option<TableDescription>, String>,
}

/// How a console's statements are committed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransactionMode {
    /// Each statement commits on its own, unless the user opens a transaction with `BEGIN`.
    #[default]
    Auto,
    /// A transaction is started before the first statement that changes anything, and stays
    /// open until committed or rolled back.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionEnd {
    Commit,
    Rollback,
}

/// Where a password given at the prompt was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordKept {
    /// In the keychain, for this and later sessions.
    Saved,
    /// In memory only, as asked.
    ForThisSession,
    /// In memory only, because the keychain refused it.
    SaveFailed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PasswordUpdate {
    /// Use whatever password is saved, if any.
    #[default]
    Keep,
    Set(Password),
}

/// Something the engine reports to the UI.
///
/// Errors arrive as display strings: the UI only shows them, and the full error has already
/// been logged by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The engine's command loop is running and accepting commands.
    Started,
    DataSourcesLoaded(Vec<DataSource>),
    /// `data_sources.toml` exists but can't be read. Saving is refused until it is fixed, so
    /// a broken file is never overwritten with an empty list.
    DataSourcesUnavailable {
        error: String,
    },
    DataSourceSaved(DataSource),
    DataSourceSaveFailed {
        id: DataSourceId,
        error: String,
    },
    DataSourceDeleted(DataSourceId),
    /// What `projects.toml` holds, read at start-up, right before the data sources. No
    /// projects if there is no file.
    ProjectsLoaded(Projects),
    /// `projects.toml` exists but can't be read. Changing projects is refused until it is
    /// fixed, so a broken file is never overwritten.
    ProjectsUnavailable {
        error: String,
    },
    /// A project was created or renamed.
    ProjectSaved(Project),
    ProjectSaveFailed {
        id: ProjectId,
        error: String,
    },
    /// The project is gone; its data sources were moved to no project first.
    ProjectDeleted(ProjectId),
    McpDefaultsSaved(McpPermissions),
    McpDefaultsSaveFailed {
        error: String,
    },
    /// Every saved console file, read once at start-up, right after the data sources.
    ConsolesLoaded(Vec<SavedConsole>),
    /// The bindings `keymap.toml` sets, read at start-up (before the data sources) and on
    /// request. Empty if there is no file.
    KeymapLoaded {
        path: PathBuf,
        result: Result<KeymapBindings, String>,
    },
    /// What `settings.toml` holds, read at start-up (after the keymap, before the data
    /// sources), after a save and on request. The defaults if there is no file.
    SettingsLoaded {
        path: PathBuf,
        result: Result<Settings, String>,
    },
    /// One of `actions.toml`, `snippets.toml` and `theme.toml`, read at start-up (after the
    /// settings, before the data sources) and on request: what it holds that can be used, and
    /// its problems. Empty if there is no file.
    CustomFileLoaded {
        path: PathBuf,
        file: CustomFile,
    },
    /// The command actions the user trusted to run, read at start-up right after the
    /// customisation files, and again after each [`Command::TrustCommand`].
    TrustedCommandsLoaded(TrustedCommands),
    /// A program from [`Command::RunProgram`] ended: what it wrote, or why there is nothing to
    /// use.
    ProgramFinished {
        program: ProgramId,
        result: Result<ProgramOutput, ProgramFailure>,
    },
    /// The answer to [`Command::CheckForUpdates`]: the newest published release, or why it
    /// couldn't be found out.
    LatestRelease(Result<Release, String>),
    /// How much of an update has downloaded: bytes so far, and the total when known.
    UpdateProgress {
        downloaded: u64,
        total: Option<u64>,
    },
    /// The answer to [`Command::InstallUpdate`]: the update now in place, and what restarting
    /// DbRover does to finish it, or why it couldn't be installed.
    UpdateInstalled(Result<InstalledUpdate, String>),
    ConsoleSaveFailed {
        key: ConsoleKey,
        error: String,
    },
    /// The answer to [`Command::OpenSqlFile`].
    SqlFileRead {
        path: PathBuf,
        result: Result<SqlFileContents, String>,
    },
    /// The answer to [`Command::SaveSqlFile`]: the file's new modification time.
    SqlFileSaved {
        console: ConsoleKey,
        path: PathBuf,
        result: Result<Option<SystemTime>, String>,
    },
    /// A file [`Command::CheckSqlFiles`] looked at changed on disk: what it holds now, or
    /// `None` if it is gone.
    SqlFileChanged {
        path: PathBuf,
        contents: Option<SqlFileContents>,
    },
    /// The answer to [`Command::DescribeTable`]: `None` if there is no such plain table.
    TableDescribed {
        data_source: DataSourceId,
        table: RelationName,
        result: Result<Option<TableDescription>, String>,
    },
    /// The answer to [`Command::DescribeTables`]: each table asked for, in order.
    TablesDescribed {
        data_source: DataSourceId,
        tables: Vec<DescribedTable>,
    },
    /// The answer to [`Command::DescribeRole`].
    RoleDescribed {
        data_source: DataSourceId,
        path: CatalogPath,
        result: Result<RoleDescription, String>,
    },
    /// The answer to [`Command::ApplyDdl`].
    DdlApplied {
        data_source: DataSourceId,
        result: Result<(), DdlFailure>,
    },
    /// The answer to [`Command::LoadDdl`].
    DdlLoaded {
        data_source: DataSourceId,
        path: CatalogPath,
        result: Result<String, String>,
    },
    /// The answer to [`Command::LoadTable`].
    TableLoaded {
        data_source: DataSourceId,
        path: CatalogPath,
        result: Result<ExistingTable, String>,
    },
    /// The answer to [`Command::ApplyEdits`].
    EditsApplied {
        console: ConsoleId,
        result: Result<(), EditFailure>,
    },
    /// The answer to [`Command::ValidateStatements`]: each statement, as sent, and what the
    /// database said.
    StatementsValidated {
        console: ConsoleId,
        results: Vec<(String, StatementCheck)>,
    },
    /// How many rows [`Command::ImportRows`] has inserted so far, uncommitted until it ends.
    ImportProgress {
        data_source: DataSourceId,
        rows: u64,
    },
    /// The answer to [`Command::ImportRows`]: how many rows were imported, or why none were.
    RowsImported {
        data_source: DataSourceId,
        result: Result<u64, ImportFailure>,
    },
    /// How many rows [`Command::ExportAllRows`] or [`Command::ExportTable`] has written so far.
    ExportProgress {
        exporter: Exporter,
        rows: usize,
    },
    /// The answer to [`Command::ExportRows`], [`Command::ExportAllRows`] and
    /// [`Command::ExportTable`]: how many rows were written, or why the file wasn't.
    ExportFinished {
        exporter: Exporter,
        path: PathBuf,
        result: Result<usize, String>,
    },
    /// The answer to [`Command::SearchHistory`], echoing what was searched for so the UI can
    /// drop answers to searches it has moved on from.
    HistoryFound {
        query: HistoryQuery,
        result: Result<Vec<HistoryEntry>, String>,
    },
    /// The answer to [`Command::ExportHistory`]: how many statements were written.
    HistoryExported {
        path: PathBuf,
        result: Result<usize, String>,
    },
    SessionChanged {
        id: DataSourceId,
        state: SessionState,
    },
    ConnectionTested {
        id: DataSourceId,
        result: Result<ServerInfo, String>,
    },
    /// A connection to the data source was not attempted for want of a secret, its database
    /// password or what its SSH login needs: none is saved, or the keychain couldn't be read.
    /// The session that wanted it reports [`SessionState::Failed`] too; the UI asks for it
    /// and answers with [`Command::ProvidePassword`].
    PasswordNeeded {
        data_source: DataSourceId,
        secret: Secret,
        reason: PasswordNeeded,
    },
    /// A connection to the data source stopped at its SSH server, whose host key isn't in
    /// `known_hosts` yet. The session reports [`SessionState::Failed`] too; the UI shows the
    /// key's fingerprint and, if the user trusts it, answers with [`Command::TrustHostKey`].
    HostKeyUnknown {
        data_source: DataSourceId,
        key: HostKey,
    },
    /// A connection to the data source was not attempted because its password command hasn't
    /// been trusted to run. The session reports [`SessionState::Failed`] too; the UI shows the
    /// command in full and, if the user trusts it, answers with
    /// [`Command::TrustPasswordCommand`].
    PasswordCommandUntrusted {
        data_source: DataSourceId,
        command: PasswordCommand,
    },
    /// The answer to [`Command::TrustHostKey`]: whether `known_hosts` now holds the key.
    HostKeyTrusted {
        data_source: DataSourceId,
        result: Result<(), String>,
    },
    /// The answer to [`Command::ExportDataSources`]: how many data sources were written.
    DataSourcesExported {
        path: PathBuf,
        result: Result<usize, String>,
    },
    /// The answer to [`Command::PreviewImport`]. Boxed, since a preview is large next to
    /// every other event.
    ImportPreviewed {
        path: PathBuf,
        result: Result<Box<ImportPreview>, String>,
    },
    /// The answer to [`Command::ApplyImport`], once everything chosen is saved, or once
    /// saving failed. Projects saved before a data source failed stay saved.
    ImportApplied(Result<(), String>),
    /// The answer to [`Command::ProvidePassword`].
    PasswordProvided {
        data_source: DataSourceId,
        secret: Secret,
        kept: PasswordKept,
    },
    ChildrenLoaded {
        id: DataSourceId,
        path: CatalogPath,
        /// What was asked for, so the answer to a query since replaced can be told apart.
        query: ChildrenQuery,
        result: Result<ChildrenPage, String>,
    },
    DefinitionLoaded {
        id: DataSourceId,
        path: CatalogPath,
        result: Result<String, String>,
    },
    DatabaseFileCreated {
        file: DatabaseFile,
        result: Result<(), String>,
    },
    ConsoleSessionChanged {
        console: ConsoleId,
        state: SessionState,
    },
    /// The console's connection entered or left a transaction, by any statement or by
    /// [`Command::EndTransaction`]. A new connection starts outside one, and closing one ends
    /// its transaction, so neither is reported.
    TransactionChanged {
        console: ConsoleId,
        status: TransactionStatus,
    },
    /// The answer to [`Command::EndTransaction`]: what actually happened, which is a rollback
    /// when committing a transaction that had failed.
    TransactionEnded {
        console: ConsoleId,
        result: Result<TransactionEnd, String>,
    },
    /// A statement that returns rows has started; its rows follow as [`Event::RowsFetched`].
    StatementStarted {
        run: RunId,
        statement: usize,
        columns: Vec<ColumnInfo>,
        /// Whether its rows are rows or documents, one a row (ADR 0006).
        shape: ResultShape,
    },
    RowsFetched {
        run: RunId,
        statement: usize,
        rows: Vec<Row>,
        /// Whether [`Command::FetchMore`] can read more.
        more: bool,
    },
    StatementFinished {
        run: RunId,
        statement: usize,
        result: Result<StatementSummary, StatementFailure>,
    },
    /// Every statement has run, or one failed. Rows of the last statement may still be
    /// fetched afterwards.
    RunFinished {
        run: RunId,
        outcome: RunOutcome,
        elapsed: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementSummary {
    /// Rows changed (or returned), as the database reports it.
    pub rows_changed: Option<u64>,
    /// Time until the statement completed, or until its first page for row results.
    pub elapsed: Duration,
    /// Whether rows were left unread.
    pub truncated: bool,
}

/// What the database said about a statement it was asked to prepare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementCheck {
    Accepted,
    Rejected(Rejection),
    /// Not asked: the console isn't connected, holds unread rows or an open transaction, the
    /// statement isn't a data query or change, or the database couldn't be asked.
    Skipped,
}

/// One statement of the grid's Submit, and what it does to its row, for the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEdit {
    pub sql: String,
    /// `None` only where nobody knows the row as it was, as in tests of the SQL alone.
    pub change: Option<RowChange>,
}

/// Why the grid's changes weren't applied. In a transaction, none were: they were rolled back.
/// One command at a time (Redis), those before `statement` were applied, and stay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFailure {
    /// The index of the statement that failed, if one did (rather than starting or ending
    /// the transaction around them).
    pub statement: Option<usize>,
    pub message: String,
}

/// Who an export reports to: a console, for its rows, or a data source, for a table's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exporter {
    Console(ConsoleId),
    DataSource(DataSourceId),
}

/// Every row of a table or view to write to a file.
#[derive(Debug, Clone)]
pub struct TableExport {
    /// Identifies the export for [`Command::CancelTableExport`]; chosen by the UI.
    pub run: RunId,
    pub table: SqlTable,
    pub path: PathBuf,
    pub spec: ExportSpec,
}

/// A CSV file to insert into a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowsImport {
    pub table: TableName,
    pub file: PathBuf,
    pub separator: Separator,
    /// Whether the first record names the columns rather than holding a row.
    pub header: bool,
    /// Each table column given a value, and the field of the file that holds it, counting from
    /// 0. An empty field is `NULL`. The columns not listed get their defaults.
    pub mapping: Vec<(String, usize)>,
}

/// Why an import stopped. None of it stays: the rows were rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    /// The lines of the file the failure is on: one line, or the lines of a batch where the
    /// database couldn't say which row it was. `None` when no row was to blame (the file or the
    /// transaction).
    pub lines: Option<(usize, usize)>,
    pub message: String,
}

/// Why DDL stopped, and what of it stays applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlFailure {
    /// The index of the statement that failed, or `None` if none was sent: the data source is
    /// read-only, or the transaction around them couldn't be started or ended.
    pub statement: Option<usize>,
    pub message: String,
    /// How many statements stay applied: none when they ran in one transaction, otherwise the
    /// ones before the failure, which committed as they ran.
    pub applied: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementFailure {
    pub message: String,
    /// Byte offset into the statement's text, when the database reports one.
    pub position: Option<usize>,
}

/// A database file to create, for the engines whose databases are files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseFile {
    Sqlite(PathBuf),
    DuckDb(PathBuf),
}

impl DatabaseFile {
    pub fn path(&self) -> &Path {
        match self {
            Self::Sqlite(path) | Self::DuckDb(path) => path,
        }
    }

    pub fn driver(&self) -> DriverKind {
        match self {
            Self::Sqlite(_) => DriverKind::Sqlite,
            Self::DuckDb(_) => DriverKind::DuckDb,
        }
    }
}

/// What a run is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunPurpose {
    /// Running the statements.
    #[default]
    Run,
    /// Having the database describe how it would run them. Where the dialect has an `EXPLAIN`
    /// prefix, the console has put it in front already; where it explains through a session
    /// setting instead (SQL Server's `SHOWPLAN_XML`), the engine switches that on before the
    /// statements and off after them, whatever happens.
    Explain,
    /// Having the database run them and describe how it did (`EXPLAIN ANALYZE`), the console
    /// having put the prefix in front. They run as [`RunPurpose::Run`]'s do: in the console's
    /// transaction mode and, on a read-only data source, checked and inside the read-only
    /// transaction. Like [`RunPurpose::Explain`], their parameters are written in as literals.
    ExplainAnalyse,
}

/// What started a run, which the history records with its statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RunOrigin {
    /// Typed in the console, or run again from it.
    #[default]
    Console,
    /// A custom action from `actions.toml`, filled in and run at once.
    Action(ActionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
}

/// The state of a data source's connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SessionState {
    #[default]
    Disconnected,
    Connecting,
    Connected(ServerInfo),
    Failed(String),
    /// The connection was lost after it was made, for this reason: the server closed or ended
    /// the session, restarted, or stopped answering. The explorer reconnects on its next
    /// request; a console only when asked, since its transaction, temporary tables and
    /// session settings went with the connection.
    Lost(String),
}
