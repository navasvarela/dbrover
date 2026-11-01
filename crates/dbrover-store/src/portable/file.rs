//! The exported file's format: `data_sources.toml`'s `[[data_source]]` tables, plus a format
//! version and the projects they are in.
//!
//! Every type here names, field by field, what an export may carry. Conversions destructure
//! DbRover's own types without `..`, so a field added to a data source or project doesn't
//! compile until someone decides here whether it travels. That is the allow-list the plan
//! asks for (`docs/well-rounded.md`, Risks): copying the whole data source and removing
//! secrets would let a new secret through silently.

use std::path::{Path, PathBuf};

use dbrover_core::{
    ClickHouseConfig, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    ElasticsearchAuth, ElasticsearchConfig, McpLogin, MongoConfig, MysqlConfig, PasswordCommand,
    Port, PostgresConfig, ProjectId, RedisConfig, SafetyLevel, ServerConfig, SourceColor,
    SqlServerConfig, SqliteConfig, SshConfig, SshLogin, TlsMode,
};
use serde::{Deserialize, Serialize};

use crate::projects::{McpOverrides, Project, ProjectSlug};

/// The version this DbRover writes. A file with a newer one is refused rather than read in
/// part, since what it adds may matter (a restriction, say).
pub(crate) const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PortableFile {
    pub(crate) format_version: u32,
    #[serde(default, rename = "project", skip_serializing_if = "Vec::is_empty")]
    pub(crate) projects: Vec<PortableProject>,
    #[serde(default, rename = "data_source")]
    pub(crate) data_sources: Vec<PortableDataSource>,
}

/// A project and what MCP clients may do in it. Its overrides travel as they are, so on a
/// machine whose defaults differ the project allows what that machine's defaults say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableProject {
    id: ProjectId,
    name: String,
    slug: ProjectSlug,
    #[serde(default)]
    mcp_enabled: bool,
    #[serde(default, skip_serializing_if = "McpOverrides::is_empty")]
    mcp: McpOverrides,
}

impl From<&Project> for PortableProject {
    fn from(project: &Project) -> Self {
        let Project {
            id,
            name,
            slug,
            mcp_enabled,
            mcp,
        } = project;
        Self {
            id: *id,
            name: name.clone(),
            slug: slug.clone(),
            mcp_enabled: *mcp_enabled,
            mcp: mcp.clone(),
        }
    }
}

impl From<PortableProject> for Project {
    fn from(project: PortableProject) -> Self {
        let PortableProject {
            id,
            name,
            slug,
            mcp_enabled,
            mcp,
        } = project;
        Self {
            id,
            name,
            slug,
            mcp_enabled,
            mcp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PortableDataSource {
    pub(crate) id: DataSourceId,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<SourceColor>,
    /// A team's production data source stays careful or read-only on every machine.
    #[serde(default, skip_serializing_if = "SafetyLevel::is_normal")]
    safety: SafetyLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<ProjectId>,
    /// The user MCP clients sign in as travels, so a team shares the dedicated login; its
    /// password stays in each machine's keychain, like the data source's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp_login: Option<McpLogin>,
    #[serde(flatten)]
    config: PortableConfig,
}

impl PortableDataSource {
    /// `data_source` as the file writes it, with each path rewritten by `path`.
    pub(crate) fn export(data_source: &DataSource, path: impl Fn(&Path) -> PathBuf) -> Self {
        let DataSource {
            id,
            name,
            color,
            safety,
            project,
            mcp_login,
            config,
        } = data_source;
        Self {
            id: *id,
            name: name.clone(),
            color: *color,
            safety: *safety,
            project: *project,
            mcp_login: mcp_login.clone(),
            config: PortableConfig::export(config, &path),
        }
    }

    /// The data source this entry describes, with each path rewritten by `path`.
    pub(crate) fn import(self, path: impl Fn(&Path) -> PathBuf) -> DataSource {
        let Self {
            id,
            name,
            color,
            safety,
            project,
            mcp_login,
            config,
        } = self;
        DataSource {
            id,
            name,
            color,
            safety,
            project,
            mcp_login,
            config: config.import(&path),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "driver", rename_all = "lowercase")]
enum PortableConfig {
    Postgres(PortableServer),
    Sqlite(PortableSqlite),
    Mysql(PortableServer),
    #[serde(rename = "sqlserver")]
    SqlServer(PortableServer),
    #[serde(rename = "duckdb")]
    DuckDb(PortableDuckDb),
    #[serde(rename = "clickhouse")]
    ClickHouse(PortableServer),
    Redis(PortableRedis),
    #[serde(rename = "mongodb")]
    Mongo(PortableMongo),
    Elasticsearch(PortableElasticsearch),
}

impl PortableConfig {
    fn export(config: &ConnectionConfig, path: &impl Fn(&Path) -> PathBuf) -> Self {
        match config {
            ConnectionConfig::Postgres(PostgresConfig { server }) => {
                Self::Postgres(PortableServer::export(server, path))
            }
            ConnectionConfig::Mysql(MysqlConfig { server }) => {
                Self::Mysql(PortableServer::export(server, path))
            }
            ConnectionConfig::SqlServer(SqlServerConfig { server }) => {
                Self::SqlServer(PortableServer::export(server, path))
            }
            // `read_only` is never saved, so it never travels either.
            ConnectionConfig::ClickHouse(ClickHouseConfig {
                server,
                read_only: _,
            }) => Self::ClickHouse(PortableServer::export(server, path)),
            ConnectionConfig::Redis(RedisConfig { server, separator }) => {
                Self::Redis(PortableRedis {
                    server: PortableServer::export(server, path),
                    separator: separator.clone(),
                })
            }
            ConnectionConfig::Mongo(MongoConfig {
                server,
                auth_database,
                connection_string,
            }) => Self::Mongo(PortableMongo {
                server: PortableServer::export(server, path),
                auth_database: auth_database.clone(),
                connection_string: connection_string.clone(),
            }),
            ConnectionConfig::Elasticsearch(ElasticsearchConfig { server, auth }) => {
                Self::Elasticsearch(PortableElasticsearch {
                    server: PortableServer::export(server, path),
                    auth: *auth,
                })
            }
            ConnectionConfig::Sqlite(SqliteConfig {
                path: file,
                read_only,
            }) => Self::Sqlite(PortableSqlite {
                path: path(file),
                read_only: *read_only,
            }),
            ConnectionConfig::DuckDb(DuckDbConfig { database }) => Self::DuckDb(PortableDuckDb {
                database: match database {
                    DuckDbDatabase::File {
                        path: file,
                        read_only,
                    } => PortableDuckDbDatabase::File {
                        path: path(file),
                        read_only: *read_only,
                    },
                    DuckDbDatabase::InMemory => PortableDuckDbDatabase::InMemory,
                },
            }),
        }
    }

    fn import(self, path: &impl Fn(&Path) -> PathBuf) -> ConnectionConfig {
        match self {
            Self::Postgres(server) => ConnectionConfig::Postgres(PostgresConfig {
                server: server.import(path),
            }),
            Self::Mysql(server) => ConnectionConfig::Mysql(MysqlConfig {
                server: server.import(path),
            }),
            Self::SqlServer(server) => ConnectionConfig::SqlServer(SqlServerConfig {
                server: server.import(path),
            }),
            Self::ClickHouse(server) => ConnectionConfig::ClickHouse(ClickHouseConfig {
                server: server.import(path),
                read_only: false,
            }),
            Self::Redis(PortableRedis { server, separator }) => {
                ConnectionConfig::Redis(RedisConfig {
                    server: server.import(path),
                    separator,
                })
            }
            Self::Mongo(PortableMongo {
                server,
                auth_database,
                connection_string,
            }) => ConnectionConfig::Mongo(MongoConfig {
                server: server.import(path),
                auth_database,
                connection_string,
            }),
            Self::Elasticsearch(PortableElasticsearch { server, auth }) => {
                ConnectionConfig::Elasticsearch(ElasticsearchConfig {
                    server: server.import(path),
                    auth,
                })
            }
            Self::Sqlite(PortableSqlite {
                path: file,
                read_only,
            }) => ConnectionConfig::Sqlite(SqliteConfig {
                path: path(&file),
                read_only,
            }),
            Self::DuckDb(PortableDuckDb { database }) => ConnectionConfig::DuckDb(DuckDbConfig {
                database: match database {
                    PortableDuckDbDatabase::File {
                        path: file,
                        read_only,
                    } => DuckDbDatabase::File {
                        path: path(&file),
                        read_only,
                    },
                    PortableDuckDbDatabase::InMemory => DuckDbDatabase::InMemory,
                },
            }),
        }
    }
}

/// A Redis server, and the separator its keys are grouped by, written only when it isn't `:`
/// as in `data_sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableRedis {
    #[serde(flatten)]
    server: PortableServer,
    #[serde(
        default = "default_separator",
        skip_serializing_if = "is_default_separator"
    )]
    separator: String,
}

/// A MongoDB server, with its authentication database and connection string written only when
/// set, as in `data_sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableMongo {
    #[serde(flatten)]
    server: PortableServer,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    auth_database: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection_string: Option<String>,
}

/// An Elasticsearch or OpenSearch node, with what it signs in with written only for an API
/// key, as in `data_sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableElasticsearch {
    #[serde(flatten)]
    server: PortableServer,
    #[serde(default, skip_serializing_if = "ElasticsearchAuth::is_password")]
    auth: ElasticsearchAuth,
}

fn default_separator() -> String {
    RedisConfig::DEFAULT_SEPARATOR.to_owned()
}

fn is_default_separator(separator: &str) -> bool {
    separator == RedisConfig::DEFAULT_SEPARATOR
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableServer {
    host: String,
    port: Port,
    database: String,
    user: String,
    #[serde(default)]
    tls: PortableTls,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh: Option<PortableSsh>,
    /// Travels as written: it names a program and the server's settings, never a secret. The
    /// trust to run it stays on each machine, in `trusted_commands.toml`, so an imported
    /// command asks before it first runs there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password_command: Option<PasswordCommand>,
}

impl PortableServer {
    fn export(server: &ServerConfig, path: &impl Fn(&Path) -> PathBuf) -> Self {
        let ServerConfig {
            host,
            port,
            database,
            user,
            tls,
            ssh,
            password_command,
        } = server;
        Self {
            host: host.clone(),
            port: port.clone(),
            database: database.clone(),
            user: user.clone(),
            tls: PortableTls::export(tls, path),
            ssh: ssh.as_ref().map(|ssh| PortableSsh::export(ssh, path)),
            password_command: password_command.clone(),
        }
    }

    fn import(self, path: &impl Fn(&Path) -> PathBuf) -> ServerConfig {
        let Self {
            host,
            port,
            database,
            user,
            tls,
            ssh,
            password_command,
        } = self;
        ServerConfig {
            host,
            port,
            database,
            user,
            tls: tls.import(path),
            ssh: ssh.map(|ssh| ssh.import(path)),
            password_command,
        }
    }
}

/// [`SshConfig`], written the same way. Where the SSH server is and whom to sign in as
/// travel; the password or key passphrase stays in each machine's keychain, and the key file
/// is a path like any other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableSsh {
    host: String,
    port: Port,
    user: String,
    #[serde(flatten)]
    login: PortableSshLogin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "login", rename_all = "kebab-case")]
enum PortableSshLogin {
    Agent,
    KeyFile { key_file: PathBuf },
    Password,
}

impl PortableSsh {
    fn export(ssh: &SshConfig, path: &impl Fn(&Path) -> PathBuf) -> Self {
        let SshConfig {
            host,
            port,
            user,
            login,
        } = ssh;
        Self {
            host: host.clone(),
            port: port.clone(),
            user: user.clone(),
            login: match login {
                SshLogin::Agent => PortableSshLogin::Agent,
                SshLogin::KeyFile { key_file } => PortableSshLogin::KeyFile {
                    key_file: path(key_file),
                },
                SshLogin::Password => PortableSshLogin::Password,
            },
        }
    }

    fn import(self, path: &impl Fn(&Path) -> PathBuf) -> SshConfig {
        let Self {
            host,
            port,
            user,
            login,
        } = self;
        SshConfig {
            host,
            port,
            user,
            login: match login {
                PortableSshLogin::Agent => SshLogin::Agent,
                PortableSshLogin::KeyFile { key_file } => SshLogin::KeyFile {
                    key_file: path(&key_file),
                },
                PortableSshLogin::Password => SshLogin::Password,
            },
        }
    }
}

/// [`TlsMode`], written the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
enum PortableTls {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyFull {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ca_file: Option<PathBuf>,
    },
}

impl PortableTls {
    fn export(tls: &TlsMode, path: &impl Fn(&Path) -> PathBuf) -> Self {
        match tls {
            TlsMode::Disable => Self::Disable,
            TlsMode::Prefer => Self::Prefer,
            TlsMode::Require => Self::Require,
            TlsMode::VerifyFull { ca_file } => Self::VerifyFull {
                ca_file: ca_file.as_deref().map(path),
            },
        }
    }

    fn import(self, path: &impl Fn(&Path) -> PathBuf) -> TlsMode {
        match self {
            Self::Disable => TlsMode::Disable,
            Self::Prefer => TlsMode::Prefer,
            Self::Require => TlsMode::Require,
            Self::VerifyFull { ca_file } => TlsMode::VerifyFull {
                ca_file: ca_file.as_deref().map(path),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableSqlite {
    path: PathBuf,
    #[serde(default)]
    read_only: bool,
}

/// [`DuckDbConfig`], written the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PortableDuckDb {
    #[serde(flatten)]
    database: PortableDuckDbDatabase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "database", rename_all = "kebab-case")]
enum PortableDuckDbDatabase {
    File {
        path: PathBuf,
        #[serde(default)]
        read_only: bool,
    },
    InMemory,
}
