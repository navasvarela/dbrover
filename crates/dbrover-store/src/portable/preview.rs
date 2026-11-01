//! What importing a file would change, entry by entry, and the changes the user's choices
//! make of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbDatabase, ProjectId, ServerConfig, SshLogin,
    TlsMode, Variables, expand_path,
};

use super::Incoming;
use crate::projects::{McpOverrides, Project, Projects, SchemaScope};

/// How an entry in the file compares with what this machine has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    New,
    Unchanged,
    Differs(Vec<Difference>),
}

impl EntryStatus {
    /// Importing again changes nothing; anything new or changed is imported.
    pub fn default_choice(&self) -> ImportChoice {
        match self {
            Self::New | Self::Differs(_) => ImportChoice::Import,
            Self::Unchanged => ImportChoice::Skip,
        }
    }

    /// What the user may choose. A copy of a new entry would be the entry itself.
    pub fn choices(&self) -> &'static [ImportChoice] {
        match self {
            Self::New => &[ImportChoice::Import, ImportChoice::Skip],
            Self::Unchanged | Self::Differs(_) => {
                &[ImportChoice::Import, ImportChoice::Skip, ImportChoice::Copy]
            }
        }
    }
}

/// A setting whose value in the file isn't this machine's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub setting: &'static str,
    pub current: String,
    pub incoming: String,
}

/// What to do with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportChoice {
    /// Add it, or update this machine's entry with the same id to match the file.
    Import,
    Skip,
    /// Add it under a new id, leaving this machine's entry as it is.
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectImport {
    pub incoming: Project,
    /// This machine's project with the same id, or else the same slug.
    pub local: Option<Project>,
    pub status: EntryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSourceImport {
    /// As it would be imported: in this machine's project when the file's project matches
    /// one.
    pub incoming: DataSource,
    /// This machine's data source with the same id.
    pub local: Option<DataSource>,
    pub status: EntryStatus,
    /// Files it names that this machine doesn't have.
    pub missing: Vec<MissingPath>,
    /// The project id the file gives, to follow the user's choice for that project.
    project_in_file: Option<ProjectId>,
}

/// A file a data source names that can't be found here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingPath {
    pub setting: &'static str,
    pub path: PathBuf,
    pub problem: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPreview {
    pub file: PathBuf,
    pub projects: Vec<ProjectImport>,
    pub data_sources: Vec<DataSourceImport>,
    /// This machine's projects when the preview was made, so copies get unused slugs.
    local_projects: Projects,
}

/// One choice per entry, in the preview's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportChoices {
    pub projects: Vec<ImportChoice>,
    pub data_sources: Vec<ImportChoice>,
}

/// What to write: projects and data sources to add, or to replace those with the same id.
/// Projects go first, so every data source's project exists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub projects: Vec<Project>,
    pub data_sources: Vec<DataSource>,
}

impl ImportPlan {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.data_sources.is_empty()
    }
}

impl ImportPreview {
    /// Compares `incoming` with this machine's data sources and projects. Paths are checked
    /// on disk, after resolving their placeholders from `variables`.
    pub fn new(
        incoming: Incoming,
        local_data_sources: &[DataSource],
        local_projects: &Projects,
        variables: &dyn Variables,
    ) -> Self {
        let projects: Vec<ProjectImport> = incoming
            .projects
            .into_iter()
            .map(|project| ProjectImport::new(project, local_projects))
            .collect();
        let names = ProjectNames::new(local_projects, &projects);
        let data_sources = incoming
            .data_sources
            .into_iter()
            .map(|data_source| {
                let project = default_project(data_source.project, &projects, local_projects);
                DataSourceImport::new(data_source, project, local_data_sources, &names, variables)
            })
            .collect();
        Self {
            file: incoming.file,
            projects,
            data_sources,
            local_projects: local_projects.clone(),
        }
    }

    pub fn default_choices(&self) -> ImportChoices {
        ImportChoices {
            projects: self
                .projects
                .iter()
                .map(|entry| entry.status.default_choice())
                .collect(),
            data_sources: self
                .data_sources
                .iter()
                .map(|entry| entry.status.default_choice())
                .collect(),
        }
    }

    /// The changes `choices` make. An entry without a choice is skipped.
    pub fn plan(&self, choices: &ImportChoices) -> ImportPlan {
        let mut plan = ImportPlan::default();
        let projects = self.plan_projects(&choices.projects, &mut plan);
        for (entry, choice) in self.data_sources.iter().zip(&choices.data_sources) {
            let project = entry.project_in_file.and_then(|id| {
                projects
                    .get(&id)
                    .copied()
                    .unwrap_or_else(|| self.local_projects.get(id).map(|local| local.id))
            });
            match choice {
                ImportChoice::Skip => {}
                ImportChoice::Import => {
                    let data_source = DataSource {
                        project,
                        ..entry.incoming.clone()
                    };
                    if entry.local.as_ref() != Some(&data_source) {
                        plan.data_sources.push(data_source);
                    }
                }
                ImportChoice::Copy => plan.data_sources.push(DataSource {
                    id: DataSourceId::generate(),
                    name: format!("{} (copy)", entry.incoming.name),
                    project,
                    ..entry.incoming.clone()
                }),
            }
        }
        plan
    }

    /// Adds the projects `choices` import to `plan`, and returns where each project of the
    /// file ends up: this machine's id for it, or `None` when it isn't imported at all.
    ///
    /// Copies are made last, so their slugs avoid the slugs of new projects imported as they
    /// are, wherever those come in the file.
    fn plan_projects(
        &self,
        choices: &[ImportChoice],
        plan: &mut ImportPlan,
    ) -> HashMap<ProjectId, Option<ProjectId>> {
        let mut taken = self.local_projects.clone();
        let mut targets = HashMap::new();
        let entries: Vec<_> = self.projects.iter().zip(choices.iter().copied()).collect();
        for (entry, choice) in &entries {
            let target = match (choice, &entry.local) {
                (ImportChoice::Copy, _) => continue,
                (ImportChoice::Skip, local) => local.as_ref().map(|local| local.id),
                (ImportChoice::Import, Some(local)) => {
                    let updated = Project {
                        id: local.id,
                        slug: local.slug.clone(),
                        ..entry.incoming.clone()
                    };
                    if &updated != local {
                        plan.projects.push(updated);
                    }
                    Some(local.id)
                }
                (ImportChoice::Import, None) => {
                    taken.projects.push(entry.incoming.clone());
                    plan.projects.push(entry.incoming.clone());
                    Some(entry.incoming.id)
                }
            };
            targets.insert(entry.incoming.id, target);
        }
        for (entry, _) in entries
            .iter()
            .filter(|(_, choice)| *choice == ImportChoice::Copy)
        {
            let copy = Project {
                id: ProjectId::generate(),
                slug: taken.unused_slug(&entry.incoming.name),
                ..entry.incoming.clone()
            };
            taken.projects.push(copy.clone());
            plan.projects.push(copy.clone());
            targets.insert(entry.incoming.id, Some(copy.id));
        }
        targets
    }
}

impl ProjectImport {
    fn new(incoming: Project, local_projects: &Projects) -> Self {
        let local = local_projects
            .get(incoming.id)
            .or_else(|| local_projects.by_slug(incoming.slug.as_str()))
            .cloned();
        let status = match &local {
            None => EntryStatus::New,
            Some(local) => compare(&project_settings(local), &project_settings(&incoming)),
        };
        Self {
            incoming,
            local,
            status,
        }
    }
}

impl DataSourceImport {
    fn new(
        data_source: DataSource,
        project: Option<ProjectId>,
        local_data_sources: &[DataSource],
        names: &ProjectNames,
        variables: &dyn Variables,
    ) -> Self {
        let project_in_file = data_source.project;
        let incoming = DataSource {
            project,
            ..data_source
        };
        let local = local_data_sources
            .iter()
            .find(|local| local.id == incoming.id)
            .cloned();
        let status = match &local {
            None => EntryStatus::New,
            Some(local) if *local == incoming => EntryStatus::Unchanged,
            Some(local) => compare(
                &data_source_settings(local, names),
                &data_source_settings(&incoming, names),
            ),
        };
        let missing = missing_paths(&incoming.config, variables);
        Self {
            incoming,
            local,
            status,
            missing,
            project_in_file,
        }
    }
}

/// Where a data source's project goes if every project gets its default choice: into this
/// machine's matching project, or in as it is.
fn default_project(
    project: Option<ProjectId>,
    projects: &[ProjectImport],
    local_projects: &Projects,
) -> Option<ProjectId> {
    let id = project?;
    match projects.iter().find(|entry| entry.incoming.id == id) {
        Some(entry) => Some(entry.local.as_ref().map_or(id, |local| local.id)),
        None => local_projects.get(id).map(|local| local.id),
    }
}

/// Project names by id, from this machine and the file, to show which project a data source
/// is in.
struct ProjectNames(HashMap<ProjectId, String>);

impl ProjectNames {
    fn new(local: &Projects, incoming: &[ProjectImport]) -> Self {
        let incoming = incoming
            .iter()
            .map(|entry| (entry.incoming.id, entry.incoming.name.clone()));
        let local = local
            .projects
            .iter()
            .map(|project| (project.id, project.name.clone()));
        // Local names last, so they win for a project both have.
        Self(incoming.chain(local).collect())
    }

    fn describe(&self, project: Option<ProjectId>) -> String {
        match project {
            None => "No project".to_owned(),
            Some(id) => self.0.get(&id).cloned().unwrap_or_else(|| id.to_string()),
        }
    }
}

/// Settings as `(name, value)` pairs, compared by name. An entry is unchanged when every
/// value is.
type Settings = Vec<(&'static str, String)>;

fn compare(current: &Settings, incoming: &Settings) -> EntryStatus {
    let value = |settings: &Settings, name: &str| {
        settings
            .iter()
            .find(|(setting, _)| *setting == name)
            .map(|(_, value)| value.clone())
    };
    let names = incoming.iter().chain(current).map(|(name, _)| *name);
    let mut differences: Vec<Difference> = Vec::new();
    for name in names {
        if differences
            .iter()
            .any(|difference| difference.setting == name)
        {
            continue;
        }
        let (now, then) = (value(current, name), value(incoming, name));
        if now != then {
            differences.push(Difference {
                setting: name,
                current: now.unwrap_or_else(|| "—".to_owned()),
                incoming: then.unwrap_or_else(|| "—".to_owned()),
            });
        }
    }
    if differences.is_empty() {
        EntryStatus::Unchanged
    } else {
        EntryStatus::Differs(differences)
    }
}

fn project_settings(project: &Project) -> Settings {
    let McpOverrides {
        read,
        write,
        schema,
        admin,
        max_rows,
        statement_timeout,
        schemas,
    } = &project.mcp;
    let or_default = |value: Option<String>| value.unwrap_or_else(|| "default".to_owned());
    vec![
        ("Name", project.name.clone()),
        ("Served to MCP clients", yes_no(project.mcp_enabled)),
        ("MCP reading", or_default(read.map(yes_no))),
        ("MCP writing", or_default(write.map(yes_no))),
        ("MCP schema changes", or_default(schema.map(yes_no))),
        ("MCP administration", or_default(admin.map(yes_no))),
        (
            "MCP schemas",
            or_default(schemas.as_ref().map(|scope| match scope {
                SchemaScope::All => "all".to_owned(),
                SchemaScope::Only(schemas) => schemas.join(", "),
            })),
        ),
        (
            "MCP row limit",
            or_default(max_rows.map(|rows| rows.to_string())),
        ),
        (
            "MCP statement timeout",
            or_default(statement_timeout.map(|timeout| timeout.to_string())),
        ),
    ]
}

fn data_source_settings(data_source: &DataSource, names: &ProjectNames) -> Settings {
    let mut settings = vec![
        ("Name", data_source.name.clone()),
        (
            "Colour",
            data_source
                .color
                .map_or("none", |color| color.display_name())
                .to_owned(),
        ),
        ("Safety", data_source.safety.display_name().to_owned()),
        ("Project", names.describe(data_source.project)),
        (
            "MCP login",
            data_source
                .mcp_login
                .as_ref()
                .map_or_else(|| "none".to_owned(), |login| login.user.clone()),
        ),
        (
            "Driver",
            data_source.config.driver().display_name().to_owned(),
        ),
    ];
    match &data_source.config {
        ConnectionConfig::Postgres(config) => server_settings(&config.server, &mut settings),
        ConnectionConfig::Mysql(config) => server_settings(&config.server, &mut settings),
        ConnectionConfig::SqlServer(config) => server_settings(&config.server, &mut settings),
        ConnectionConfig::ClickHouse(config) => server_settings(&config.server, &mut settings),
        ConnectionConfig::Redis(config) => server_settings(&config.server, &mut settings),
        ConnectionConfig::Elasticsearch(config) => {
            server_settings(&config.server, &mut settings);
            if config.auth == dbrover_core::ElasticsearchAuth::ApiKey {
                settings.push(("Signs in with", "an API key".to_owned()));
            }
        }
        ConnectionConfig::Cassandra(config) => {
            server_settings(&config.server, &mut settings);
            if !config.local_datacenter.is_empty() {
                settings.push(("Local datacenter", config.local_datacenter.clone()));
            }
            if let Some(seconds) = config.statement_timeout {
                settings.push(("Statement timeout", format!("{seconds} s")));
            }
        }
        ConnectionConfig::Mongo(config) => {
            server_settings(&config.server, &mut settings);
            if !config.auth_database.is_empty() {
                settings.push(("Authentication database", config.auth_database.clone()));
            }
            if let Some(string) = &config.connection_string {
                settings.push(("Connection string", string.clone()));
            }
        }
        ConnectionConfig::Sqlite(config) => {
            settings.push(("File", config.path.display().to_string()));
            settings.push(("Read-only", yes_no(config.read_only)));
        }
        ConnectionConfig::DuckDb(config) => match &config.database {
            DuckDbDatabase::File { path, read_only } => {
                settings.push(("File", path.display().to_string()));
                settings.push(("Read-only", yes_no(*read_only)));
            }
            DuckDbDatabase::InMemory => settings.push(("File", "in memory".to_owned())),
        },
    }
    settings
}

fn server_settings(server: &ServerConfig, settings: &mut Settings) {
    settings.extend([
        ("Host", server.host.clone()),
        ("Port", server.port.to_string()),
        ("Database", server.database.clone()),
        ("User", server.user.clone()),
    ]);
    let mode = match &server.tls {
        TlsMode::Disable => "disable",
        TlsMode::Prefer => "prefer",
        TlsMode::Require => "require",
        TlsMode::VerifyFull { ca_file } => {
            if let Some(ca_file) = ca_file {
                settings.push(("CA file", ca_file.display().to_string()));
            }
            "verify-full"
        }
    };
    settings.push(("TLS", mode.to_owned()));
    let password = match &server.password_command {
        // As a list, so an argument with a space in it reads as one.
        Some(command) => format!("from {:?}", command.source()),
        None => "from the keychain".to_owned(),
    };
    settings.push(("Password", password));
    let Some(ssh) = &server.ssh else {
        settings.push(("SSH", "none".to_owned()));
        return;
    };
    settings.push(("SSH", ssh.to_string()));
    let login = match &ssh.login {
        SshLogin::Agent => "agent".to_owned(),
        SshLogin::KeyFile { key_file } => format!("key file {}", key_file.display()),
        SshLogin::Password => "password".to_owned(),
    };
    settings.push(("SSH login", login));
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

/// The files a data source names, by setting.
fn paths(config: &ConnectionConfig) -> Vec<(&'static str, &Path)> {
    let server = match config {
        ConnectionConfig::Sqlite(config) => return vec![("File", &config.path)],
        ConnectionConfig::DuckDb(config) => {
            return match &config.database {
                DuckDbDatabase::File { path, .. } => vec![("File", path)],
                DuckDbDatabase::InMemory => Vec::new(),
            };
        }
        ConnectionConfig::Postgres(config) => &config.server,
        ConnectionConfig::Mysql(config) => &config.server,
        ConnectionConfig::SqlServer(config) => &config.server,
        ConnectionConfig::ClickHouse(config) => &config.server,
        ConnectionConfig::Redis(config) => &config.server,
        ConnectionConfig::Mongo(config) => &config.server,
        ConnectionConfig::Elasticsearch(config) => &config.server,
        ConnectionConfig::Cassandra(config) => &config.server,
    };
    let ca_file = match &server.tls {
        TlsMode::VerifyFull {
            ca_file: Some(ca_file),
        } => Some(("CA file", ca_file.as_path())),
        _ => None,
    };
    let key_file = match server.ssh.as_ref().map(|ssh| &ssh.login) {
        Some(SshLogin::KeyFile { key_file }) => Some(("SSH key file", key_file.as_path())),
        _ => None,
    };
    ca_file.into_iter().chain(key_file).collect()
}

fn missing_paths(config: &ConnectionConfig, variables: &dyn Variables) -> Vec<MissingPath> {
    paths(config)
        .into_iter()
        .filter_map(|(setting, path)| {
            let problem = match expand_path(path, variables) {
                Err(error) => error.to_string(),
                Ok(resolved) if !resolved.exists() => "not found on this machine".to_owned(),
                Ok(_) => return None,
            };
            Some(MissingPath {
                setting,
                path: path.to_owned(),
                problem,
            })
        })
        .collect()
}
