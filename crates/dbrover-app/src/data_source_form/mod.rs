//! The editable fields behind the data source dialog, and their validation. No egui here, so
//! the rules are testable without a UI.

mod cassandra;
mod elasticsearch;
mod mongo;
mod server;
mod ssh;

use dbrover_core::{
    ClickHouseConfig, ConnectionConfig, DataSource, DataSourceId, DriverKind, DuckDbConfig,
    DuckDbDatabase, McpLogin, MysqlConfig, Password, Port, PostgresConfig, ProjectId, RedisConfig,
    SafetyLevel, SourceColor, SqlServerConfig, SqliteConfig, TemplateError, has_placeholder,
};
use dbrover_engine::PasswordUpdate;
use dbrover_engine::read_only::Storage;

pub use cassandra::CassandraFields;
pub use elasticsearch::ElasticsearchFields;
pub use mongo::{MongoFields, MongoTarget};
pub use server::{DatabaseField, ServerDefaults, ServerFields, TlsChoice, TlsOffer, UserField};
pub use ssh::{SshFields, SshLoginChoice};

const POSTGRES_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: PostgresConfig::DEFAULT_PORT,
    database: "postgres",
    database_field: DatabaseField::Name,
    user: "postgres",
    user_field: UserField::Required,
    tls: TlsOffer::Negotiated,
};

/// `mysql` exists on every server, as `postgres` does on PostgreSQL; `root` is the account
/// every installation starts with.
const MYSQL_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: MysqlConfig::DEFAULT_PORT,
    database: "mysql",
    database_field: DatabaseField::Name,
    user: "root",
    user_field: UserField::Required,
    tls: TlsOffer::Negotiated,
};

/// `master` exists on every server and every login may open it; `sa` is the administrator
/// every installation (and the official container) starts with.
const SQL_SERVER_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: SqlServerConfig::DEFAULT_PORT,
    database: "master",
    database_field: DatabaseField::Name,
    user: "sa",
    user_field: UserField::Required,
    tls: TlsOffer::Negotiated,
};

/// `default` exists on every server, and is the user every installation (and the official
/// container) starts with.
const CLICKHOUSE_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: ClickHouseConfig::DEFAULT_PORT,
    database: "default",
    database_field: DatabaseField::Name,
    user: "default",
    user_field: UserField::Required,
    tls: TlsOffer::OwnPort(ClickHouseConfig::DEFAULT_TLS_PORT),
};

/// Every server has database `0` and the `default` user, which signs in with no user name.
/// TLS is a port of its own, set in the server's configuration with no usual number, so the
/// port stays 6379 until changed.
const REDIS_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: RedisConfig::DEFAULT_PORT,
    database: RedisConfig::DEFAULT_DATABASE,
    database_field: DatabaseField::Number,
    user: "default",
    user_field: UserField::Optional,
    tls: TlsOffer::OwnPort(RedisConfig::DEFAULT_PORT),
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
    #[error("Host is required.")]
    HostMissing,
    #[error("Port must be a number from 1 to 65535, or a ${{VARIABLE}}.")]
    InvalidPort,
    #[error("Database is required.")]
    DatabaseMissing,
    #[error("Database number must be a number such as 0, or a ${{VARIABLE}}.")]
    InvalidDatabaseNumber,
    #[error("Connection string must start with mongodb:// or mongodb+srv://.")]
    InvalidConnectionString,
    #[error("User is required.")]
    UserMissing,
    #[error("File is required.")]
    PathMissing,
    #[error("SSH host is required.")]
    SshHostMissing,
    #[error("SSH port must be a number from 1 to 65535, or a ${{VARIABLE}}.")]
    InvalidSshPort,
    #[error("SSH user is required.")]
    SshUserMissing,
    #[error("Key file is required to sign in with a key file.")]
    SshKeyFileMissing,
    #[error("Password command: {0}.")]
    PasswordCommand(TemplateError),
    #[error("Statement timeout must be a whole number of seconds, or empty.")]
    InvalidStatementTimeout,
}

/// Text as the user typed it. Converted to a [`DataSource`] only on Test or Save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSourceForm {
    /// Optional: an empty name becomes [`ConnectionConfig::default_name`].
    pub name: String,
    pub color: Option<SourceColor>,
    pub safety: SafetyLevel,
    /// Not edited in the dialog, only carried through it, so saving an edit keeps the data
    /// source in its project.
    pub project: Option<ProjectId>,
    pub mcp_login: McpLoginFields,
    pub fields: DriverFields,
}

/// The login `dbrover mcp` signs in as instead of the data source's own. An empty user means
/// none, and MCP clients sign in as the data source does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpLoginFields {
    pub user: String,
    /// Empty keeps the saved one.
    pub password: String,
    /// Whether the data source had an MCP login when the dialog opened, so removing it also
    /// removes its saved password.
    had_login: bool,
}

/// What saving the dialog does to the MCP login's saved password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpPasswordChange {
    Keep,
    Set(Password),
    Remove,
}

/// Each engine's fields: the shared [`ServerFields`] for a network engine, plus whatever is
/// particular to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverFields {
    Postgres(PostgresFields),
    Sqlite(SqliteFields),
    Mysql(MysqlFields),
    SqlServer(SqlServerFields),
    DuckDb(DuckDbFields),
    ClickHouse(ClickHouseFields),
    Redis(RedisFields),
    Mongo(MongoFields),
    Elasticsearch(ElasticsearchFields),
    Cassandra(CassandraFields),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresFields {
    pub server: ServerFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MysqlFields {
    pub server: ServerFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlServerFields {
    pub server: ServerFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickHouseFields {
    pub server: ServerFields,
}

/// Redis, Valkey and Dragonfly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisFields {
    pub server: ServerFields,
    /// What the explorer groups keys by, as typed: spaces count, since a separator may be one.
    pub separator: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuckDbFields {
    pub location: DuckDbLocation,
    /// Kept while In memory is chosen, so switching back to File doesn't lose it.
    pub path: String,
    pub read_only: bool,
}

/// Where a DuckDB data source's database is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuckDbLocation {
    File,
    InMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteFields {
    pub path: String,
    pub read_only: bool,
}

impl DriverFields {
    fn new(driver: DriverKind) -> Self {
        match driver {
            DriverKind::Postgres => Self::Postgres(PostgresFields {
                server: ServerFields::new(POSTGRES_DEFAULTS),
            }),
            DriverKind::Sqlite => Self::Sqlite(SqliteFields {
                path: String::new(),
                read_only: false,
            }),
            DriverKind::Mysql => Self::Mysql(MysqlFields {
                server: ServerFields::new(MYSQL_DEFAULTS),
            }),
            DriverKind::SqlServer => Self::SqlServer(SqlServerFields {
                server: ServerFields::new(SQL_SERVER_DEFAULTS),
            }),
            DriverKind::DuckDb => Self::DuckDb(DuckDbFields {
                location: DuckDbLocation::File,
                path: String::new(),
                read_only: false,
            }),
            DriverKind::ClickHouse => Self::ClickHouse(ClickHouseFields {
                server: ServerFields::new(CLICKHOUSE_DEFAULTS),
            }),
            DriverKind::Redis => Self::Redis(RedisFields {
                server: ServerFields::new(REDIS_DEFAULTS),
                separator: RedisConfig::DEFAULT_SEPARATOR.to_owned(),
            }),
            DriverKind::Mongo => Self::Mongo(MongoFields::new()),
            DriverKind::Elasticsearch => Self::Elasticsearch(ElasticsearchFields::new()),
            DriverKind::Cassandra => Self::Cassandra(CassandraFields::new()),
        }
    }

    fn from_config(config: &ConnectionConfig) -> Self {
        match config {
            ConnectionConfig::Postgres(config) => Self::Postgres(PostgresFields {
                server: ServerFields::from_config(&config.server, POSTGRES_DEFAULTS),
            }),
            ConnectionConfig::Sqlite(config) => Self::Sqlite(SqliteFields {
                path: config.path.display().to_string(),
                read_only: config.read_only,
            }),
            ConnectionConfig::Mysql(config) => Self::Mysql(MysqlFields {
                server: ServerFields::from_config(&config.server, MYSQL_DEFAULTS),
            }),
            ConnectionConfig::SqlServer(config) => Self::SqlServer(SqlServerFields {
                server: ServerFields::from_config(&config.server, SQL_SERVER_DEFAULTS),
            }),
            ConnectionConfig::DuckDb(config) => Self::DuckDb(match &config.database {
                DuckDbDatabase::File { path, read_only } => DuckDbFields {
                    location: DuckDbLocation::File,
                    path: path.display().to_string(),
                    read_only: *read_only,
                },
                DuckDbDatabase::InMemory => DuckDbFields {
                    location: DuckDbLocation::InMemory,
                    path: String::new(),
                    read_only: false,
                },
            }),
            ConnectionConfig::ClickHouse(config) => Self::ClickHouse(ClickHouseFields {
                server: ServerFields::from_config(&config.server, CLICKHOUSE_DEFAULTS),
            }),
            ConnectionConfig::Redis(config) => Self::Redis(RedisFields {
                server: ServerFields::from_config(&config.server, REDIS_DEFAULTS),
                separator: config.separator.clone(),
            }),
            ConnectionConfig::Mongo(config) => Self::Mongo(MongoFields::from_config(config)),
            ConnectionConfig::Elasticsearch(config) => {
                Self::Elasticsearch(ElasticsearchFields::from_config(config))
            }
            ConnectionConfig::Cassandra(config) => {
                Self::Cassandra(CassandraFields::from_config(config))
            }
        }
    }

    fn driver(&self) -> DriverKind {
        match self {
            Self::Postgres(_) => DriverKind::Postgres,
            Self::Sqlite(_) => DriverKind::Sqlite,
            Self::Mysql(_) => DriverKind::Mysql,
            Self::SqlServer(_) => DriverKind::SqlServer,
            Self::DuckDb(_) => DriverKind::DuckDb,
            Self::ClickHouse(_) => DriverKind::ClickHouse,
            Self::Redis(_) => DriverKind::Redis,
            Self::Mongo(_) => DriverKind::Mongo,
            Self::Elasticsearch(_) => DriverKind::Elasticsearch,
            Self::Cassandra(_) => DriverKind::Cassandra,
        }
    }

    /// The server section, for engines that connect over the network.
    fn server(&self) -> Option<&ServerFields> {
        match self {
            Self::Postgres(fields) => Some(&fields.server),
            Self::Mysql(fields) => Some(&fields.server),
            Self::SqlServer(fields) => Some(&fields.server),
            Self::ClickHouse(fields) => Some(&fields.server),
            Self::Redis(fields) => Some(&fields.server),
            Self::Mongo(fields) => Some(&fields.server),
            Self::Elasticsearch(fields) => Some(&fields.server),
            Self::Cassandra(fields) => Some(&fields.server),
            Self::Sqlite(_) | Self::DuckDb(_) => None,
        }
    }

    fn to_config(&self) -> Result<ConnectionConfig, FormError> {
        Ok(match self {
            Self::Postgres(fields) => ConnectionConfig::Postgres(PostgresConfig {
                server: fields.server.to_config()?,
            }),
            Self::Sqlite(fields) => ConnectionConfig::Sqlite(fields.to_config()?),
            Self::Mysql(fields) => ConnectionConfig::Mysql(MysqlConfig {
                server: fields.server.to_config()?,
            }),
            Self::SqlServer(fields) => ConnectionConfig::SqlServer(SqlServerConfig {
                server: fields.server.to_config()?,
            }),
            Self::DuckDb(fields) => ConnectionConfig::DuckDb(fields.to_config()?),
            Self::ClickHouse(fields) => ConnectionConfig::ClickHouse(ClickHouseConfig {
                server: fields.server.to_config()?,
                read_only: false,
            }),
            Self::Redis(fields) => ConnectionConfig::Redis(RedisConfig {
                server: fields.server.to_config()?,
                separator: fields.separator.clone(),
            }),
            Self::Mongo(fields) => ConnectionConfig::Mongo(fields.to_config()?),
            Self::Elasticsearch(fields) => ConnectionConfig::Elasticsearch(fields.to_config()?),
            Self::Cassandra(fields) => ConnectionConfig::Cassandra(fields.to_config()?),
        })
    }
}

impl DataSourceForm {
    pub fn new(driver: DriverKind) -> Self {
        Self {
            name: String::new(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: McpLoginFields::default(),
            fields: DriverFields::new(driver),
        }
    }

    pub fn from_data_source(data_source: &DataSource) -> Self {
        Self {
            name: data_source.name.clone(),
            color: data_source.color,
            safety: data_source.safety,
            project: data_source.project,
            mcp_login: McpLoginFields {
                user: data_source
                    .mcp_login
                    .as_ref()
                    .map(|login| login.user.clone())
                    .unwrap_or_default(),
                password: String::new(),
                had_login: data_source.mcp_login.is_some(),
            },
            fields: DriverFields::from_config(&data_source.config),
        }
    }

    pub fn driver(&self) -> DriverKind {
        self.fields.driver()
    }

    /// Where the database is kept, which with the engine decides its read-only guard.
    pub fn storage(&self) -> Storage {
        match &self.fields {
            DriverFields::DuckDb(DuckDbFields {
                location: DuckDbLocation::InMemory,
                ..
            }) => Storage::InMemory,
            _ => Storage::Kept,
        }
    }

    pub fn to_data_source(&self, id: DataSourceId) -> Result<DataSource, FormError> {
        let config = self.fields.to_config()?;
        let name = match self.name.trim() {
            "" => config.default_name(),
            name => name.to_owned(),
        };
        Ok(DataSource {
            id,
            name,
            color: self.color,
            safety: self.safety,
            project: self.project,
            mcp_login: self.mcp_login(),
            config,
        })
    }

    /// Only engines that sign in have a login to swap.
    fn mcp_login(&self) -> Option<McpLogin> {
        let user = self.mcp_login.user.trim();
        (self.driver().uses_password() && !user.is_empty()).then(|| McpLogin {
            user: user.to_owned(),
        })
    }

    pub fn mcp_password_change(&self) -> McpPasswordChange {
        if self.mcp_login().is_none() {
            return if self.mcp_login.had_login {
                McpPasswordChange::Remove
            } else {
                McpPasswordChange::Keep
            };
        }
        match self.mcp_login.password.as_str() {
            "" => McpPasswordChange::Keep,
            password => McpPasswordChange::Set(Password::new(password.to_owned())),
        }
    }

    pub fn password_update(&self) -> PasswordUpdate {
        match self.fields.server().and_then(ServerFields::typed_password) {
            Some(password) => PasswordUpdate::Set(Password::new(password)),
            None => PasswordUpdate::Keep,
        }
    }

    /// The SSH password or key passphrase typed in the form, which replaces the saved one.
    pub fn ssh_secret_update(&self) -> PasswordUpdate {
        match self
            .fields
            .server()
            .and_then(|server| server.ssh.typed_secret())
        {
            Some(secret) => PasswordUpdate::Set(Password::new(secret)),
            None => PasswordUpdate::Keep,
        }
    }
}

impl SqliteFields {
    fn to_config(&self) -> Result<SqliteConfig, FormError> {
        Ok(SqliteConfig {
            path: required(&self.path, FormError::PathMissing)?.into(),
            read_only: self.read_only,
        })
    }
}

impl DuckDbFields {
    fn to_config(&self) -> Result<DuckDbConfig, FormError> {
        let database = match self.location {
            DuckDbLocation::File => DuckDbDatabase::File {
                path: required(&self.path, FormError::PathMissing)?.into(),
                read_only: self.read_only,
            },
            DuckDbLocation::InMemory => DuckDbDatabase::InMemory,
        };
        Ok(DuckDbConfig { database })
    }
}

/// A port number, or text with a `${NAME}` placeholder that must resolve to one when
/// connecting.
fn parse_port(text: &str, invalid: FormError) -> Result<Port, FormError> {
    let port = text.trim();
    if has_placeholder(port) {
        return Ok(Port::Placeholder(port.to_owned()));
    }
    port.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .map(Port::Number)
        .ok_or(invalid)
}

fn required(value: &str, missing: FormError) -> Result<String, FormError> {
    match value.trim() {
        "" => Err(missing),
        value => Ok(value.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{ServerConfig, SshConfig, SshLogin, TlsMode};

    use super::*;

    fn postgres_form(edit: impl FnOnce(&mut ServerFields)) -> DataSourceForm {
        let mut form = DataSourceForm::new(DriverKind::Postgres);
        if let DriverFields::Postgres(fields) = &mut form.fields {
            edit(&mut fields.server);
        }
        form
    }

    #[test]
    fn names_unnamed_data_sources_and_reports_invalid_server_fields() {
        let named = postgres_form(|f| f.host = " db.internal ".to_owned())
            .to_data_source(DataSourceId::generate())
            .unwrap();
        let invalid = postgres_form(|f| f.port = "abc".to_owned());

        assert_eq!(named.name, "postgres@db.internal");
        assert_eq!(
            invalid.to_data_source(DataSourceId::generate()),
            Err(FormError::InvalidPort)
        );
    }

    #[test]
    fn round_trips_existing_data_sources() {
        let configs = [
            ConnectionConfig::Sqlite(SqliteConfig {
                path: "/data/chinook.db".into(),
                read_only: true,
            }),
            ConnectionConfig::Postgres(PostgresConfig {
                server: ServerConfig {
                    host: "db.internal".to_owned(),
                    port: 6432.into(),
                    database: "app".to_owned(),
                    user: "juan".to_owned(),
                    tls: TlsMode::VerifyFull {
                        ca_file: Some("/etc/ca.pem".into()),
                    },
                    ssh: None,
                    password_command: None,
                },
            }),
            ConnectionConfig::Mysql(MysqlConfig {
                server: ServerConfig {
                    host: "10.0.0.5".to_owned(),
                    port: 3306.into(),
                    database: "shop".to_owned(),
                    user: "root".to_owned(),
                    tls: TlsMode::Prefer,
                    ssh: Some(SshConfig {
                        host: "bastion".to_owned(),
                        port: 2222.into(),
                        user: "deploy".to_owned(),
                        login: SshLogin::KeyFile {
                            key_file: "/home/juan/.ssh/id_ed25519".into(),
                        },
                    }),
                    password_command: None,
                },
            }),
        ];
        for config in configs {
            let data_source = DataSource {
                id: DataSourceId::generate(),
                name: "saved".to_owned(),
                color: Some(SourceColor::Teal),
                safety: SafetyLevel::ReadOnly,
                project: Some(ProjectId::generate()),
                mcp_login: config.driver().uses_password().then(|| McpLogin {
                    user: "assistant".to_owned(),
                }),
                config,
            };

            let form = DataSourceForm::from_data_source(&data_source);

            assert_eq!(form.to_data_source(data_source.id), Ok(data_source));
        }
    }

    #[test]
    fn only_a_typed_password_replaces_the_saved_one() {
        let mut form = DataSourceForm::new(DriverKind::Postgres);
        assert_eq!(form.password_update(), PasswordUpdate::Keep);

        form = postgres_form(|f| f.password = "s3cret".to_owned());

        assert_eq!(
            form.password_update(),
            PasswordUpdate::Set(Password::new("s3cret"))
        );
        assert_eq!(
            DataSourceForm::new(DriverKind::Sqlite).password_update(),
            PasswordUpdate::Keep
        );
    }

    #[test]
    fn only_a_typed_ssh_secret_replaces_the_saved_one() {
        let mut form = postgres_form(|f| {
            f.ssh.enabled = true;
            f.ssh.login = SshLoginChoice::Password;
        });
        assert_eq!(form.ssh_secret_update(), PasswordUpdate::Keep);

        if let DriverFields::Postgres(fields) = &mut form.fields {
            fields.server.ssh.secret = "over ssh".to_owned();
        }

        assert_eq!(
            form.ssh_secret_update(),
            PasswordUpdate::Set(Password::new("over ssh"))
        );
    }

    #[test]
    fn keeps_sets_or_removes_the_mcp_logins_password() {
        let mut form = postgres_form(|_| {});
        assert_eq!(form.mcp_password_change(), McpPasswordChange::Keep);

        form.mcp_login.user = " assistant ".to_owned();
        form.mcp_login.password = "for the assistant".to_owned();
        let saved = form.to_data_source(DataSourceId::generate()).unwrap();
        assert_eq!(
            saved.mcp_login,
            Some(McpLogin {
                user: "assistant".to_owned()
            })
        );
        assert_eq!(
            form.mcp_password_change(),
            McpPasswordChange::Set(Password::new("for the assistant"))
        );

        let mut edited = DataSourceForm::from_data_source(&saved);
        assert_eq!(edited.mcp_login.user, "assistant");
        assert_eq!(edited.mcp_password_change(), McpPasswordChange::Keep);
        edited.mcp_login.user.clear();
        assert_eq!(edited.mcp_password_change(), McpPasswordChange::Remove);
        assert_eq!(edited.to_data_source(saved.id).unwrap().mcp_login, None);

        let mut sqlite = DataSourceForm::new(DriverKind::Sqlite);
        sqlite.mcp_login.user = "assistant".to_owned();
        assert_eq!(sqlite.mcp_password_change(), McpPasswordChange::Keep);
    }
}
