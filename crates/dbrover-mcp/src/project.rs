//! Which project a server serves, and what it may do there.

use std::time::Duration;

use dbrover_core::{DataSource, DriverKind};
use dbrover_engine::read_only::McpServing;
use dbrover_engine::read_write;
use dbrover_store::{
    DataSourceFile, McpPermissions, Project, ProjectFile, ProjectStoreError, SchemaScope,
    StoreError,
};

/// Why the server can't start for the project asked for. Each message says what to do.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("could not read the projects")]
    Projects(#[source] ProjectStoreError),
    #[error("could not read the data sources")]
    DataSources(#[source] StoreError),
    #[error("there is no project with the slug {slug:?}; {}", known_slugs(known))]
    UnknownProject { slug: String, known: Vec<String> },
    #[error(
        "the project {slug:?} is not allowed for MCP clients; set mcp_enabled = true on it in \
         projects.toml"
    )]
    NotEnabled { slug: String },
    #[error("the project {slug:?} doesn't allow reading, so an MCP client could do nothing there")]
    ReadNotAllowed { slug: String },
}

fn known_slugs(known: &[String]) -> String {
    if known.is_empty() {
        "there are no projects yet: create one in DbRover's explorer".to_owned()
    } else {
        format!("the projects are: {}", known.join(", "))
    }
}

/// How much one call may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_rows: usize,
    pub statement_timeout: Duration,
}

/// What a project allows beyond reading, each one for the `execute` tool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub write: bool,
    pub schema: bool,
    pub admin: bool,
}

/// A permission beyond reading, as `projects.toml` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Write,
    Schema,
    Admin,
}

impl Permission {
    pub fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Schema => "schema",
            Self::Admin => "admin",
        }
    }
}

impl Changes {
    fn from_permissions(permissions: &McpPermissions) -> Self {
        Self {
            write: permissions.write,
            schema: permissions.schema,
            admin: permissions.admin,
        }
    }

    pub fn any(self) -> bool {
        self.write || self.schema || self.admin
    }

    /// Whether a connection to `driver` is opened to run changes too: only if this allows
    /// some and the engine has a transaction to run them in. Otherwise it is opened read-only,
    /// which on SQLite and ClickHouse is the engine's own read-only mode.
    pub fn open_writable(self, driver: DriverKind) -> bool {
        self.any() && read_write::mode(driver).is_some()
    }

    pub fn allows(self, permission: Permission) -> bool {
        match permission {
            Permission::Write => self.write,
            Permission::Schema => self.schema,
            Permission::Admin => self.admin,
        }
    }

    /// The names of the permissions this allows, for the log and the client.
    pub fn names(self) -> Vec<&'static str> {
        [Permission::Write, Permission::Schema, Permission::Admin]
            .into_iter()
            .filter(|permission| self.allows(*permission))
            .map(Permission::name)
            .collect()
    }
}

/// A project resolved for serving: its data sources, as MCP may use them, and its limits.
#[derive(Debug, Clone)]
pub struct ServedProject {
    pub project: Project,
    pub limits: Limits,
    /// What `execute` may run; nothing, when the project only reads.
    pub changes: Changes,
    /// The schemas the catalog tools show and statements may name.
    pub schemas: SchemaScope,
    pub data_sources: Vec<DataSource>,
}

impl ServedProject {
    /// Reads both files and picks the project with `slug`, refusing one MCP may not use.
    ///
    /// Read once, at start-up: a client restarts the server to see changes, which is what
    /// clients do when their configuration changes anyway.
    ///
    /// # Errors
    ///
    /// [`StartError`] if a file can't be read, or the project doesn't exist or isn't allowed.
    pub fn load(
        projects: &ProjectFile,
        data_sources: &DataSourceFile,
        slug: &str,
    ) -> Result<Self, StartError> {
        let projects = projects.load().map_err(StartError::Projects)?;
        let Some(project) = projects.by_slug(slug) else {
            return Err(StartError::UnknownProject {
                slug: slug.to_owned(),
                known: projects
                    .projects
                    .iter()
                    .map(|project| project.slug.to_string())
                    .collect(),
            });
        };
        if !project.mcp_enabled {
            return Err(StartError::NotEnabled {
                slug: slug.to_owned(),
            });
        }
        let permissions = projects.mcp_permissions(project);
        if !permissions.read {
            return Err(StartError::ReadNotAllowed {
                slug: slug.to_owned(),
            });
        }
        let changes = Changes::from_permissions(&permissions);
        let data_sources = data_sources
            .load()
            .map_err(StartError::DataSources)?
            .into_iter()
            .filter(|data_source| data_source.project == Some(project.id))
            .filter(|data_source| is_served(slug, data_source))
            .map(|data_source| {
                // Opening SQLite files read-only is stronger than the `query_only` each read
                // sets on a file opened to run changes too.
                if changes.open_writable(data_source.config.driver()) {
                    data_source
                } else {
                    dbrover_engine::read_only::connection_config(data_source)
                }
            })
            .collect();
        Ok(Self {
            project: project.clone(),
            limits: Limits {
                max_rows: usize::try_from(permissions.max_rows.get()).unwrap_or(usize::MAX),
                statement_timeout: permissions.statement_timeout.duration(),
            },
            changes,
            schemas: permissions.schemas.clone(),
            data_sources,
        })
    }

    pub fn slug(&self) -> &str {
        self.project.slug.as_str()
    }

    /// The data source a tool call names: by name, or by id when two share a name.
    pub fn data_source(&self, name_or_id: &str) -> Option<&DataSource> {
        self.data_sources
            .iter()
            .find(|data_source| data_source.name == name_or_id)
            .or_else(|| {
                self.data_sources
                    .iter()
                    .find(|data_source| data_source.id.to_string() == name_or_id)
            })
    }
}

/// A data source is served only where the database refuses what the check can't see, such as
/// a function that writes: in the engine's read-only mode, or through an MCP login.
fn is_served(slug: &str, data_source: &DataSource) -> bool {
    let serving = McpServing::of(data_source);
    if let Some(reason) = serving.reason(data_source.config.driver()) {
        tracing::warn!(project = slug, data_source = %data_source.name, "not served: {reason}");
    }
    serving == McpServing::Served
}

#[cfg(test)]
mod tests {
    use dbrover_core::{
        ConnectionConfig, DataSourceId, McpLogin, ProjectId, ServerConfig, SqlServerConfig,
        SqliteConfig, TlsMode,
    };
    use dbrover_store::{ProjectSlug, Projects};

    use super::*;

    struct Files {
        _dir: tempfile::TempDir,
        projects: ProjectFile,
        data_sources: DataSourceFile,
    }

    fn files(projects: Vec<Project>, data_sources: &[DataSource]) -> Files {
        let dir = tempfile::tempdir().unwrap();
        let project_file = ProjectFile::new(dir.path().join("projects.toml"));
        project_file
            .save(&Projects {
                projects,
                ..Projects::default()
            })
            .unwrap();
        let data_source_file = DataSourceFile::new(dir.path().join("data_sources.toml"));
        data_source_file.save(data_sources).unwrap();
        Files {
            _dir: dir,
            projects: project_file,
            data_sources: data_source_file,
        }
    }

    fn project(name: &str, mcp_enabled: bool) -> Project {
        let mut project = Project::new(
            ProjectId::generate(),
            name.to_owned(),
            ProjectSlug::from_name(name),
        );
        project.mcp_enabled = mcp_enabled;
        project
    }

    fn sqlite(name: &str, project: Option<ProjectId>) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: format!("/data/{name}.db").into(),
                read_only: false,
            }),
        }
    }

    fn sql_server(name: &str, project: Option<ProjectId>) -> DataSource {
        DataSource {
            name: name.to_owned(),
            config: ConnectionConfig::SqlServer(SqlServerConfig {
                server: ServerConfig {
                    host: "erp.internal".to_owned(),
                    port: SqlServerConfig::DEFAULT_PORT.into(),
                    database: "erp".to_owned(),
                    user: "reader".to_owned(),
                    tls: TlsMode::default(),
                    ssh: None,
                    password_command: None,
                },
            }),
            ..sqlite(name, project)
        }
    }

    #[test]
    fn serves_only_the_projects_own_data_sources_with_a_read_only_mode() {
        let shop = project("Shop", true);
        let other = project("Other", true);
        let files = files(
            vec![shop.clone(), other.clone()],
            &[
                sqlite("orders", Some(shop.id)),
                sqlite("elsewhere", Some(other.id)),
                sqlite("loose", None),
                sql_server("erp", Some(shop.id)),
            ],
        );

        let served = ServedProject::load(&files.projects, &files.data_sources, "shop").unwrap();

        assert_eq!(served.data_sources.len(), 1);
        let orders = served.data_source("orders").unwrap();
        assert!(matches!(
            &orders.config,
            ConnectionConfig::Sqlite(SqliteConfig {
                read_only: true,
                ..
            })
        ));
        assert_eq!(served.data_source(&orders.id.to_string()), Some(orders));
        assert!(served.data_source("elsewhere").is_none());
        assert!(
            served.data_source("erp").is_none(),
            "SQL Server has no read-only mode, and erp has no MCP login"
        );
        assert_eq!(served.limits.max_rows, 500);
        assert_eq!(served.limits.statement_timeout, Duration::from_secs(30));
    }

    #[test]
    fn serves_an_engine_without_a_read_only_mode_only_through_an_mcp_login() {
        let shop = project("Shop", true);
        let guarded = DataSource {
            mcp_login: Some(McpLogin {
                user: "assistant".to_owned(),
            }),
            ..sql_server("guarded", Some(shop.id))
        };
        let files = files(
            vec![shop],
            &[guarded.clone(), sql_server("unguarded", guarded.project)],
        );

        let served = ServedProject::load(&files.projects, &files.data_sources, "shop").unwrap();

        assert_eq!(served.data_source("guarded"), Some(&guarded));
        assert!(served.data_source("unguarded").is_none());
    }

    #[test]
    fn refuses_unknown_disabled_and_unreadable_projects() {
        let mut closed = project("Closed", true);
        closed.mcp.read = Some(false);
        let files = files(vec![project("Hidden", false), closed], &[]);

        let start = |slug| ServedProject::load(&files.projects, &files.data_sources, slug);

        assert!(matches!(
            start("nope"),
            Err(StartError::UnknownProject { known, .. }) if known == ["hidden", "closed"]
        ));
        assert!(matches!(
            start("hidden"),
            Err(StartError::NotEnabled { .. })
        ));
        assert!(matches!(
            start("closed"),
            Err(StartError::ReadNotAllowed { .. })
        ));
    }
}
