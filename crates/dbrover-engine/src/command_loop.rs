use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dbrover_core::{DataSource, DataSourceId, Password, ProjectId, describe_error};
use dbrover_store::{
    ConsoleFiles, CustomFiles, DiagramFiles, ImportPlan, KeymapFile, Project, Secret, SecretError,
    SecretStore, SettingsFile, TrustedCommand, TrustedCommands, TrustedCommandsFile,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinError, JoinSet};

use crate::config_files;
use crate::console_files;
use crate::diagram_files;
use crate::events::EventSink;
use crate::export;
use crate::history::{HistoryExport, HistoryLog};
use crate::keymap;
use crate::open::{TestServices, TypedSecrets, create_database_file, test_connection};
use crate::password_commands::PasswordCommands;
use crate::passwords::SessionPasswords;
use crate::programs::{self, ProgramFailure, ProgramId, ProgramRequest, ProgramRunner};
use crate::projects::{ProjectError, ProjectRegistry};
use crate::registry::{Registry, RegistryError};
use crate::releases::{self, ReleaseFeed};
use crate::session::{Script, Services, Session, SessionOwner, SessionRequest};
use crate::settings;
use crate::sql_files;
use crate::transfer;
use crate::tunnels::Tunnels;
use crate::updater::{self, UpdateInstaller};
use crate::{
    Command, ConsoleId, EditFailure, EngineConfig, Event, ExportAll, Exporter, HostKey,
    PasswordKept, PasswordUpdate, ProcessEnvironment, RunOutcome, StatementCheck,
};

#[derive(Debug, thiserror::Error)]
enum SaveError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("could not store the password")]
    Password(#[source] SecretError),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

/// Handles commands until the UI closes the command channel, then disconnects everything.
pub(crate) async fn run(
    config: EngineConfig,
    mut commands: UnboundedReceiver<Command>,
    events: EventSink,
) {
    tracing::info!("engine started");
    events.send(Event::Started);
    events.send(keymap::load(&config.keymap).await);
    let settings = settings::load(&config.settings).await;
    let retention = settings::history_retention(&settings);
    events.send(settings);
    for event in config_files::load_custom(&config.custom_files).await {
        events.send(event);
    }
    let trusted = programs::load_trusted(&config.trusted_commands).await;
    events.send(Event::TrustedCommandsLoaded(trusted.clone()));
    let (trusted, _) = watch::channel(trusted);
    let projects = ProjectRegistry::load(config.projects).await;
    events.send(projects.loaded_event());
    let registry = Registry::load(config.data_sources).await;
    events.send(registry.loaded_event());
    events.send(console_files::load(&config.consoles).await);
    events.send(diagram_files::load(&config.diagrams).await);
    let (history, history_task) = HistoryLog::start(config.history, events.clone());
    if let Some(retention) = retention {
        history.prune(retention);
    }

    let mut engine = Engine {
        history,
        registry,
        projects,
        secrets: config.secrets,
        passwords: SessionPasswords::default(),
        tunnels: Tunnels::new(config.known_hosts),
        idle_ping_after: config.idle_ping_after,
        console_files: config.consoles,
        diagram_files: config.diagrams,
        keymap_file: config.keymap,
        settings_file: config.settings,
        custom_files: config.custom_files,
        trusted_file: config.trusted_commands,
        trusted,
        program_runner: config.programs,
        programs: HashMap::new(),
        releases: config.releases,
        updater: config.updater,
        events,
        sessions: HashMap::new(),
        consoles: HashMap::new(),
        closing: JoinSet::new(),
    };
    while let Some(command) = commands.recv().await {
        engine.handle(command).await;
    }
    // Sessions hold history handles too, so the history thread ends only once they have
    // stopped and their last statements are recorded.
    engine.shut_down().await;
    if let Err(error) = history_task.await {
        tracing::error!("the history thread failed: {error}");
    }
    tracing::info!("engine stopped");
}

struct ConsoleSession {
    data_source: DataSourceId,
    session: Session,
}

struct Engine {
    registry: Registry,
    projects: ProjectRegistry,
    history: HistoryLog,
    secrets: Arc<dyn SecretStore>,
    /// Passwords given at the prompt, shared with every session.
    passwords: SessionPasswords,
    /// SSH tunnels, shared with every session so a data source's sessions share one.
    tunnels: Tunnels,
    idle_ping_after: Duration,
    console_files: ConsoleFiles,
    diagram_files: DiagramFiles,
    keymap_file: KeymapFile,
    settings_file: SettingsFile,
    custom_files: CustomFiles,
    trusted_file: TrustedCommandsFile,
    /// The commands the user trusted, actions' and password commands'; the engine runs no
    /// others. Sessions watch it, since they run password commands from their own tasks.
    trusted: watch::Sender<TrustedCommands>,
    program_runner: Arc<dyn ProgramRunner>,
    /// How to stop each program that may still be running. Dropping one stops its program,
    /// which is how shutting down stops them all.
    programs: HashMap<ProgramId, oneshot::Sender<()>>,
    releases: Arc<dyn ReleaseFeed>,
    updater: Arc<dyn UpdateInstaller>,
    events: EventSink,
    /// The explorer's sessions, one per data source.
    sessions: HashMap<DataSourceId, Session>,
    /// Each query console's own session.
    consoles: HashMap<ConsoleId, ConsoleSession>,
    /// Sessions that were asked to disconnect and are still closing their connection.
    closing: JoinSet<()>,
}

impl Engine {
    /// Saves and deletes (of data sources and console files) are awaited here, so they apply
    /// in the order the UI sent them.
    /// Anything that waits on a network (connecting, testing) runs in its own task.
    async fn handle(&mut self, command: Command) {
        match command {
            Command::SaveDataSource {
                data_source,
                password,
                ssh_secret,
            } => self.save(data_source, password, ssh_secret).await,
            Command::DeleteDataSource(id) => self.delete(id).await,
            Command::CreateProject { id, name } => {
                let result = self.projects.create(id, &name).await;
                self.report_project(id, result);
            }
            Command::RenameProject { id, name } => {
                let result = self.projects.rename(id, &name).await;
                self.report_project(id, result);
            }
            Command::SaveProjectMcp {
                id,
                mcp_enabled,
                overrides,
            } => {
                let result = self.projects.save_mcp(id, mcp_enabled, overrides).await;
                self.report_project(id, result);
            }
            Command::SaveMcpDefaults(defaults) => {
                let event = match self.projects.save_mcp_defaults(defaults).await {
                    Ok(defaults) => {
                        tracing::info!("saved the MCP defaults");
                        Event::McpDefaultsSaved(defaults)
                    }
                    Err(error) => {
                        let error = describe_error(&error);
                        tracing::error!("could not save the MCP defaults: {error}");
                        Event::McpDefaultsSaveFailed { error }
                    }
                };
                self.events.send(event);
            }
            Command::SaveMcpPassword { id, password } => self.save_mcp_password(id, password).await,
            Command::DeleteProject(id) => self.delete_project(id).await,
            Command::MoveToProject {
                data_source,
                project,
            } => self.move_to_project(data_source, project).await,
            Command::ExportDataSources {
                path,
                data_sources,
                projects,
            } => self.export(path, &data_sources, &projects),
            Command::PreviewImport(path) => self.preview_import(path),
            Command::ApplyImport(plan) => self.apply_import(plan).await,
            Command::TestConnection {
                data_source,
                password,
                ssh_secret,
            } => {
                tokio::spawn(test_connection(
                    data_source,
                    TypedSecrets {
                        password,
                        ssh: ssh_secret,
                    },
                    TestServices {
                        secrets: Arc::clone(&self.secrets),
                        tunnels: self.tunnels.clone(),
                        commands: self.password_commands().shown(),
                        events: self.events.clone(),
                    },
                ));
            }
            Command::Connect(id) => self.connect(id),
            Command::ProvidePassword {
                data_source,
                secret,
                password,
                save,
            } => {
                self.provide_password(data_source, secret, password, save)
                    .await
            }
            Command::TrustHostKey { data_source, key } => {
                self.trust_host_key(data_source, key).await;
            }
            Command::TrustPasswordCommand {
                data_source,
                command,
            } => {
                self.let_go_of_failed_session(data_source);
                self.trust_command(TrustedCommand::password(data_source, &command))
                    .await;
            }
            Command::Disconnect(id) => self.disconnect(id),
            Command::LoadChildren { id, path, query } => {
                self.request(id, SessionRequest::Children(path, query));
            }
            Command::LoadDefinition { id, path } => {
                self.request(id, SessionRequest::Definition(path));
            }
            Command::CreateDatabaseFile(file) => {
                tokio::spawn(create_database_file(file, self.events.clone()));
            }
            Command::Execute {
                console,
                data_source,
                run,
                statements,
                parameters,
                transactions,
                purpose,
                origin,
            } => self.execute(
                console,
                data_source,
                Script {
                    run,
                    statements,
                    parameters,
                    transactions,
                    purpose,
                    origin,
                },
            ),
            Command::EndTransaction { console, end } => {
                if let Some(console) = self.consoles.get(&console) {
                    console.session.request(SessionRequest::EndTransaction(end));
                }
            }
            Command::FetchMore { console, run } => {
                if let Some(console) = self.consoles.get(&console) {
                    console.session.request(SessionRequest::FetchMore(run));
                }
            }
            Command::Cancel { console, run } => {
                if let Some(console) = self.consoles.get(&console) {
                    console.session.cancel(run);
                }
            }
            Command::ReconnectConsole(console) => self.reconnect_console(console),
            Command::CloseConsole(console) => self.close_console(console),
            Command::SaveConsole { key, text } => {
                if let Some(failed) = console_files::save(&self.console_files, key, text).await {
                    self.events.send(failed);
                }
            }
            Command::DeleteConsoleFile(key) => {
                console_files::delete(&self.console_files, key).await;
            }
            Command::SaveDiagram { key, file } => {
                if let Some(failed) = diagram_files::save(&self.diagram_files, key, file).await {
                    self.events.send(failed);
                }
            }
            Command::DeleteDiagram(key) => diagram_files::delete(&self.diagram_files, key).await,
            // Reads, saves and checks of `.sql` files are awaited rather than spawned, so they
            // happen, and are answered, in the order sent. A check spawned before a save could
            // otherwise read the file's old text and be answered after the save: the console,
            // seeing nothing unsaved, would then go back to the old text. Each is a read, a
            // write or a `stat` per file, which keeps the other commands waiting no longer
            // than a console save does.
            Command::OpenSqlFile(path) => sql_files::read(path, &self.events).await,
            Command::SaveSqlFile {
                console,
                path,
                text,
            } => self.events.send(sql_files::save(console, path, text).await),
            Command::CheckSqlFiles(files) => sql_files::check(files, &self.events).await,
            Command::DescribeTable { data_source, table } => {
                self.request(data_source, SessionRequest::DescribeTable(table));
            }
            Command::DescribeTables {
                data_source,
                tables,
            } => self.request(data_source, SessionRequest::DescribeTables(tables)),
            Command::DescribeRole { data_source, path } => {
                self.request(data_source, SessionRequest::DescribeRole(path));
            }
            Command::ApplyDdl {
                data_source,
                statements,
            } => self.request(data_source, SessionRequest::ApplyDdl(statements)),
            Command::LoadDdl { data_source, path } => {
                self.request(data_source, SessionRequest::Ddl(path));
            }
            Command::LoadTable { data_source, path } => {
                self.request(data_source, SessionRequest::LoadTable(path));
            }
            Command::RunProgram(request) => self.run_program(*request),
            Command::StopProgram(program) => {
                if let Some(stop) = self.programs.remove(&program) {
                    // The program may have ended already, taking its receiver with it.
                    let _ = stop.send(());
                }
            }
            Command::TrustCommand(command) => self.trust_command(command).await,
            Command::ImportRows {
                data_source,
                import,
            } => self.request(data_source, SessionRequest::ImportRows(import)),
            Command::ApplyEdits {
                console,
                statements,
            } => match self.consoles.get(&console) {
                Some(session) => session
                    .session
                    .request(SessionRequest::ApplyEdits(statements)),
                None => self.events.send(Event::EditsApplied {
                    console,
                    result: Err(EditFailure {
                        statement: None,
                        message: "the console isn't connected; run its query again".to_owned(),
                    }),
                }),
            },
            Command::ValidateStatements {
                console,
                statements,
            } => match self.consoles.get(&console) {
                Some(session) if !session.session.is_finished() => session
                    .session
                    .request(SessionRequest::Validate(statements)),
                _ => self.events.send(Event::StatementsValidated {
                    console,
                    results: statements
                        .into_iter()
                        .map(|sql| (sql, StatementCheck::Skipped))
                        .collect(),
                }),
            },
            Command::ReloadKeymap => self.events.send(keymap::load(&self.keymap_file).await),
            Command::CreateKeymap { text } => {
                self.events
                    .send(keymap::create(&self.keymap_file, text).await);
            }
            Command::CreateCustomFile { kind, text } => {
                self.events
                    .send(config_files::create_custom(&self.custom_files, kind, text).await);
            }
            Command::ReloadCustomFiles => {
                for event in config_files::load_custom(&self.custom_files).await {
                    self.events.send(event);
                }
            }
            Command::ReloadSettings => {
                self.events.send(settings::load(&self.settings_file).await);
            }
            Command::SaveSettings(new) => {
                let saved = settings::save(&self.settings_file, new).await;
                if let Some(retention) = settings::history_retention(&saved) {
                    self.history.prune(retention);
                }
                self.events.send(saved);
            }
            Command::ExportRows { console, export } => {
                tokio::spawn(export::run(console, export, self.events.clone()));
            }
            Command::ExportTable {
                data_source,
                export,
            } => self.request(data_source, SessionRequest::ExportTable(export)),
            Command::RunToFile {
                console,
                data_source,
                export,
            } => self.run_to_file(console, data_source, export),
            Command::CancelTableExport { data_source, run } => {
                if let Some(session) = self.sessions.get(&data_source) {
                    session.cancel(run);
                }
            }
            Command::ExportAllRows { console, export } => match self.consoles.get(&console) {
                Some(session) => session.session.request(SessionRequest::ExportAll(export)),
                None => self.events.send(Event::ExportFinished {
                    exporter: Exporter::Console(console),
                    path: export.path,
                    result: Err("the console isn't connected; run its query again".to_owned()),
                }),
            },
            Command::SearchHistory(query) => self.history.search(query),
            Command::ExportHistory {
                query,
                path,
                spec,
                names,
            } => self.history.export(HistoryExport {
                query,
                path,
                spec,
                names,
            }),
            Command::CheckForUpdates => {
                tokio::spawn(releases::check(
                    Arc::clone(&self.releases),
                    self.events.clone(),
                ));
            }
            Command::InstallUpdate => {
                tokio::spawn(updater::install(
                    Arc::clone(&self.updater),
                    self.events.clone(),
                ));
            }
        }
    }

    async fn save(
        &mut self,
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
    ) {
        let id = data_source.id;
        match self
            .try_save(data_source.clone(), password, ssh_secret)
            .await
        {
            Ok(()) => {
                tracing::info!(data_source = %data_source.name, "saved data source");
                self.events.send(Event::DataSourceSaved(data_source));
            }
            Err(error) => {
                let error = describe_error(&error);
                tracing::error!(data_source = %data_source.name, "could not save data source: {error}");
                self.events.send(Event::DataSourceSaveFailed { id, error });
            }
        }
    }

    async fn try_save(
        &mut self,
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
    ) -> Result<(), SaveError> {
        self.registry.ensure_available()?;
        let updates = [(Secret::Database, password), (Secret::Ssh, ssh_secret)];
        for (secret, update) in updates {
            if let PasswordUpdate::Set(value) = update {
                self.save_secret(data_source.id, secret, value).await?;
            }
        }
        self.registry.upsert(data_source).await?;
        Ok(())
    }

    /// Saves a secret typed in the dialog, replacing any given at the prompt for this session.
    async fn save_secret(
        &mut self,
        id: DataSourceId,
        secret: Secret,
        value: Password,
    ) -> Result<(), SaveError> {
        self.passwords.remove(id, secret);
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || secrets.set(id, secret, &value))
            .await
            .map_err(SaveError::Task)?
            .map_err(SaveError::Password)
    }

    async fn delete(&mut self, id: DataSourceId) {
        self.disconnect(id);
        match self.registry.remove(id).await {
            Ok(true) => {
                self.passwords.remove_all(id);
                self.delete_secrets(id).await;
                tracing::info!(%id, "deleted data source");
                self.events.send(Event::DataSourceDeleted(id));
            }
            Ok(false) => tracing::warn!(%id, "delete requested for an unknown data source"),
            Err(error) => {
                tracing::error!(%id, "could not delete data source: {}", describe_error(&error));
            }
        }
    }

    fn report_project(&self, id: ProjectId, result: Result<Project, ProjectError>) {
        match result {
            Ok(project) => {
                tracing::info!(project = %project.name, slug = %project.slug, "saved project");
                self.events.send(Event::ProjectSaved(project));
            }
            Err(error) => {
                let error = describe_error(&error);
                tracing::error!(%id, "could not save project: {error}");
                self.events.send(Event::ProjectSaveFailed { id, error });
            }
        }
    }

    /// Moves the data sources out first: if that fails, the project stays and nothing has
    /// changed; if forgetting the project then fails, it is only left empty.
    async fn delete_project(&mut self, id: ProjectId) {
        if !self.projects.contains(id) {
            self.report_project_failure(id, &ProjectError::Unknown);
            return;
        }
        let moved = match self.registry.leave_project(id).await {
            Ok(moved) => moved,
            Err(error) => {
                self.report_project_failure(id, &error);
                return;
            }
        };
        for data_source in moved {
            self.events.send(Event::DataSourceSaved(data_source));
        }
        match self.projects.remove(id).await {
            Ok(()) => {
                tracing::info!(%id, "deleted project");
                self.events.send(Event::ProjectDeleted(id));
            }
            Err(error) => self.report_project_failure(id, &error),
        }
    }

    fn report_project_failure(&self, id: ProjectId, error: &(dyn std::error::Error + 'static)) {
        let error = describe_error(error);
        tracing::error!(%id, "could not delete project: {error}");
        self.events.send(Event::ProjectSaveFailed { id, error });
    }

    async fn move_to_project(&mut self, id: DataSourceId, project: Option<ProjectId>) {
        let result = match project {
            Some(project) if !self.projects.contains(project) => {
                Err(describe_error(&ProjectError::Unknown))
            }
            _ => self
                .registry
                .move_to_project(id, project)
                .await
                .map_err(|error| describe_error(&error)),
        };
        match result {
            Ok(data_source) => {
                let project = project.map_or_else(|| "none".to_owned(), |id| id.to_string());
                tracing::info!(data_source = %data_source.name, %project, "moved data source");
                self.events.send(Event::DataSourceSaved(data_source));
            }
            Err(error) => {
                tracing::error!(%id, "could not move data source: {error}");
                self.events.send(Event::DataSourceSaveFailed { id, error });
            }
        }
    }

    /// Remembers the password for the session first, so the next connection works even if
    /// the keychain refuses it.
    async fn provide_password(
        &mut self,
        data_source: DataSourceId,
        secret: Secret,
        password: Password,
        save: bool,
    ) {
        self.passwords.insert(data_source, secret, password.clone());
        self.let_go_of_failed_session(data_source);
        let kept = if save {
            let secrets = Arc::clone(&self.secrets);
            let result =
                tokio::task::spawn_blocking(move || secrets.set(data_source, secret, &password))
                    .await;
            match result {
                Ok(Ok(())) => PasswordKept::Saved,
                Ok(Err(error)) => PasswordKept::SaveFailed(describe_error(&error)),
                Err(error) => PasswordKept::SaveFailed(describe_error(&error)),
            }
        } else {
            PasswordKept::ForThisSession
        };
        match &kept {
            PasswordKept::SaveFailed(error) => {
                tracing::warn!(%data_source, "could not save the password, keeping it for this session: {error}");
            }
            PasswordKept::Saved | PasswordKept::ForThisSession => {
                tracing::info!(%data_source, "password given: {kept:?}");
            }
        }
        self.events.send(Event::PasswordProvided {
            data_source,
            secret,
            kept,
        });
    }

    /// The explorer's session that asked the UI something has reported its failure, but its
    /// task may not have ended yet; `connect` would then take it for one still connecting and
    /// ignore the Connect the UI sends once answered. A question only follows a failed
    /// attempt, so the session is let go here.
    fn let_go_of_failed_session(&mut self, id: DataSourceId) {
        if let Some(session) = self.sessions.remove(&id) {
            self.closing.spawn(session.stop());
        }
    }

    /// Adds the key to `known_hosts`, so the next tunnel to its server trusts it.
    async fn trust_host_key(&mut self, data_source: DataSourceId, key: HostKey) {
        self.let_go_of_failed_session(data_source);
        let known_hosts = self.tunnels.known_hosts().to_owned();
        let server = key.server();
        let fingerprint = key.fingerprint();
        let result = tokio::task::spawn_blocking(move || dbrover_ssh::trust(&known_hosts, &key))
            .await
            .map_err(|error| describe_error(&error))
            .and_then(|written| written.map_err(|error| describe_error(&error)));
        match &result {
            Ok(()) => {
                tracing::info!(%data_source, "trusted the host key of {server}: {fingerprint}")
            }
            Err(error) => {
                tracing::error!(%data_source, "could not trust the host key of {server}: {error}");
            }
        }
        self.events.send(Event::HostKeyTrusted {
            data_source,
            result,
        });
    }

    fn export(&self, path: PathBuf, data_sources: &[DataSourceId], projects: &[ProjectId]) {
        let gathered = self
            .registry
            .data_sources()
            .map_err(|e| describe_error(&e))
            .and_then(|all| {
                let projects_here = self.projects.projects().map_err(|e| describe_error(&e))?;
                Ok(transfer::DataSourceExport::gather(
                    path.clone(),
                    all,
                    projects_here,
                    data_sources,
                    projects,
                ))
            });
        match gathered {
            Ok(export) => {
                tokio::spawn(transfer::export(export, self.events.clone()));
            }
            Err(error) => self.events.send(Event::DataSourcesExported {
                path,
                result: Err(error),
            }),
        }
    }

    fn preview_import(&self, path: PathBuf) {
        let snapshot = self
            .registry
            .data_sources()
            .map_err(|e| describe_error(&e))
            .and_then(|data_sources| {
                let projects = self.projects.projects().map_err(|e| describe_error(&e))?;
                Ok((data_sources.to_vec(), projects.clone()))
            });
        match snapshot {
            Ok((data_sources, projects)) => {
                tokio::spawn(transfer::preview(
                    path,
                    data_sources,
                    projects,
                    self.events.clone(),
                ));
            }
            Err(error) => self.events.send(Event::ImportPreviewed {
                path,
                result: Err(error),
            }),
        }
    }

    /// Saves the projects, then the data sources, each file once.
    async fn apply_import(&mut self, plan: ImportPlan) {
        let ImportPlan {
            projects,
            data_sources,
        } = plan;
        let result = self.import_projects(projects).await;
        let result = match result {
            Ok(()) => self.import_data_sources(data_sources).await,
            Err(error) => Err(error),
        };
        match &result {
            Ok(()) => tracing::info!("imported data sources"),
            Err(error) => tracing::error!("could not import data sources: {error}"),
        }
        self.events.send(Event::ImportApplied(result));
    }

    async fn import_projects(&mut self, projects: Vec<Project>) -> Result<(), String> {
        if projects.is_empty() {
            return Ok(());
        }
        self.projects
            .import(projects.clone())
            .await
            .map_err(|error| describe_error(&error))?;
        for project in projects {
            self.events.send(Event::ProjectSaved(project));
        }
        Ok(())
    }

    async fn import_data_sources(&mut self, data_sources: Vec<DataSource>) -> Result<(), String> {
        if data_sources.is_empty() {
            return Ok(());
        }
        self.registry
            .upsert_all(data_sources.clone())
            .await
            .map_err(|error| describe_error(&error))?;
        for data_source in data_sources {
            self.events.send(Event::DataSourceSaved(data_source));
        }
        Ok(())
    }

    async fn save_mcp_password(&self, id: DataSourceId, password: Option<Password>) {
        let secrets = Arc::clone(&self.secrets);
        let result = tokio::task::spawn_blocking(move || match password {
            Some(password) => secrets.set(id, Secret::McpLogin, &password),
            None => secrets.delete(id, Secret::McpLogin),
        })
        .await;
        let error = match result {
            Ok(Ok(())) => {
                tracing::info!(%id, "saved the MCP login's password");
                return;
            }
            Ok(Err(error)) => describe_error(&error),
            Err(error) => describe_error(&error),
        };
        tracing::error!(%id, "could not save the MCP login's password: {error}");
        self.events.send(Event::DataSourceSaveFailed { id, error });
    }

    /// Removes every keychain entry the data source has. A leftover entry is harmless, so
    /// failing to remove one is only logged.
    async fn delete_secrets(&self, id: DataSourceId) {
        let secrets = Arc::clone(&self.secrets);
        let result = tokio::task::spawn_blocking(move || {
            Secret::ALL
                .into_iter()
                .map(|secret| secrets.delete(id, secret))
                .fold(Ok(()), Result::and)
        })
        .await;
        let error = match result {
            Ok(Ok(())) => return,
            Ok(Err(error)) => describe_error(&error),
            Err(error) => describe_error(&error),
        };
        tracing::warn!(%id, "could not remove the saved secrets: {error}");
    }

    fn connect(&mut self, id: DataSourceId) {
        if self
            .sessions
            .get(&id)
            .is_some_and(|session| !session.is_finished())
        {
            tracing::debug!(%id, "already connected or connecting");
            return;
        }
        let Some(data_source) = self.registry.get(id) else {
            tracing::warn!(%id, "connect requested for an unknown data source");
            return;
        };
        let session = Session::start(
            data_source.clone(),
            SessionOwner::Explorer(id),
            self.services(),
        );
        self.sessions.insert(id, session);
    }

    /// Queues a run on the console's session, (re)connecting it if it isn't connected to
    /// `data_source`.
    fn execute(&mut self, console: ConsoleId, data_source: DataSourceId, script: Script) {
        match self.console_session(console, data_source) {
            Some(session) => session.request(SessionRequest::Execute(script)),
            None => self.events.send(Event::RunFinished {
                run: script.run,
                outcome: RunOutcome::Failed,
                elapsed: std::time::Duration::ZERO,
            }),
        }
    }

    fn run_to_file(&mut self, console: ConsoleId, data_source: DataSourceId, export: ExportAll) {
        match self.console_session(console, data_source) {
            Some(session) => session.request(SessionRequest::ExportAll(export)),
            None => self.events.send(Event::ExportFinished {
                exporter: Exporter::Console(console),
                path: export.path,
                result: Err("the data source is gone".to_owned()),
            }),
        }
    }

    /// The console's session on `data_source`, started if it has none or has one on another
    /// data source. `None` for a data source that isn't saved.
    fn console_session(
        &mut self,
        console: ConsoleId,
        data_source: DataSourceId,
    ) -> Option<&Session> {
        let reusable = self.consoles.get(&console).is_some_and(|existing| {
            existing.data_source == data_source && !existing.session.is_finished()
        });
        if !reusable {
            self.close_console(console);
            let Some(config) = self.registry.get(data_source) else {
                tracing::warn!(%data_source, "run requested for an unknown data source");
                return None;
            };
            let session = Session::start(
                config.clone(),
                SessionOwner::Console(console),
                self.services(),
            );
            self.consoles.insert(
                console,
                ConsoleSession {
                    data_source,
                    session,
                },
            );
        }
        self.consoles.get(&console).map(|console| &console.session)
    }

    fn run_program(&mut self, request: ProgramRequest) {
        let program = request.program;
        let invocation = self.prepare_program(&request);
        let invocation = match invocation {
            Ok(invocation) => invocation,
            Err(failure) => {
                tracing::warn!("{}: {failure}", request.action);
                self.events.send(Event::ProgramFinished {
                    program,
                    result: Err(failure),
                });
                return;
            }
        };
        let (stop, stopped) = oneshot::channel();
        self.programs.retain(|_, stop| !stop.is_closed());
        self.programs.insert(program, stop);
        tokio::spawn(programs::run(
            program,
            invocation,
            request.command.timeout,
            stopped,
            Arc::clone(&self.program_runner),
            self.events.clone(),
        ));
    }

    fn prepare_program(
        &self,
        request: &ProgramRequest,
    ) -> Result<programs::Invocation, ProgramFailure> {
        if !programs::is_trusted(&self.trusted.borrow(), request) {
            return Err(ProgramFailure::NotTrusted);
        }
        let data_source = self
            .registry
            .get(request.data_source)
            .ok_or(ProgramFailure::NoDataSource)?;
        programs::invocation(request, data_source, &ProcessEnvironment)
    }

    async fn trust_command(&mut self, command: TrustedCommand) {
        self.trusted.send_modify(|trusted| trusted.trust(command));
        let trusted = self.trusted.borrow().clone();
        programs::save_trusted(&self.trusted_file, &trusted).await;
        self.events.send(Event::TrustedCommandsLoaded(trusted));
    }

    fn password_commands(&self) -> PasswordCommands {
        PasswordCommands::recorded(self.trusted.subscribe(), Arc::clone(&self.program_runner))
    }

    fn services(&self) -> Services {
        Services {
            idle_ping_after: self.idle_ping_after,
            secrets: Arc::clone(&self.secrets),
            passwords: self.passwords.clone(),
            tunnels: self.tunnels.clone(),
            commands: self.password_commands(),
            events: self.events.clone(),
            history: self.history.clone(),
        }
    }

    /// Replaces the console's session with a new one to the same data source.
    fn reconnect_console(&mut self, console: ConsoleId) {
        let Some(data_source) = self
            .consoles
            .get(&console)
            .map(|existing| existing.data_source)
        else {
            return;
        };
        self.close_console(console);
        let Some(config) = self.registry.get(data_source) else {
            tracing::warn!(%data_source, "reconnect requested for an unknown data source");
            return;
        };
        tracing::info!(data_source = %config.name, "reconnecting a console");
        let session = Session::start(
            config.clone(),
            SessionOwner::Console(console),
            self.services(),
        );
        self.consoles.insert(
            console,
            ConsoleSession {
                data_source,
                session,
            },
        );
    }

    fn close_console(&mut self, console: ConsoleId) {
        if let Some(console) = self.consoles.remove(&console) {
            self.closing.spawn(console.session.stop());
        }
    }

    /// Queues work on the data source's session, starting one if it isn't connected, so
    /// expanding a disconnected data source in the explorer connects it.
    fn request(&mut self, id: DataSourceId, request: SessionRequest) {
        self.connect(id);
        if let Some(session) = self.sessions.get(&id) {
            session.request(request);
        }
    }

    fn disconnect(&mut self, id: DataSourceId) {
        if let Some(session) = self.sessions.remove(&id) {
            self.closing.spawn(session.stop());
        }
        let consoles: Vec<ConsoleId> = self
            .consoles
            .iter()
            .filter(|(_, console)| console.data_source == id)
            .map(|(console, _)| *console)
            .collect();
        for console in consoles {
            self.close_console(console);
        }
    }

    async fn shut_down(mut self) {
        drop(self.history);
        for (_, session) in self.sessions.drain() {
            self.closing.spawn(session.stop());
        }
        for (_, console) in self.consoles.drain() {
            self.closing.spawn(console.session.stop());
        }
        while let Some(result) = self.closing.join_next().await {
            if let Err(error) = result {
                tracing::error!("session shutdown failed: {error}");
            }
        }
    }
}
