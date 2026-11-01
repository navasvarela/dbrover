use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::placeholder::{PlaceholderError, Setting, Variables, expand, expand_path};
use crate::{DriverError, PasswordCommand};

/// Stable identity of a data source. Written once when the data source is created, so renaming
/// it never breaks the keychain entry, consoles or history that refer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DataSourceId(Uuid);

impl DataSourceId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for DataSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Parses the form [`Display`](fmt::Display) writes, as found in folder names under `consoles/`.
impl FromStr for DataSourceId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

/// Stable identity of a project, a named group of data sources. Like [`DataSourceId`], written
/// once, so renaming a project never breaks the data sources (or MCP clients) that refer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(Uuid);

impl ProjectId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A saved connection configuration. Never holds a password; those live in the OS keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSource {
    pub id: DataSourceId,
    pub name: String,
    /// Tints the data source's explorer row and its consoles, so production stands out from
    /// a local copy at a glance. Optional, and absent from files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<SourceColor>,
    /// What consoles may change: everything, everything after asking, or nothing. Written
    /// only when it isn't [`SafetyLevel::Normal`], so older files load unchanged.
    #[serde(default, skip_serializing_if = "SafetyLevel::is_normal")]
    pub safety: SafetyLevel,
    /// The project the data source is grouped under; `None` is "No project". Absent from
    /// files written before projects existed. An id `projects.toml` doesn't know also means
    /// no project, rather than an error, so deleting that file never loses a data source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// Another login for `dbrover mcp` to connect as instead of the data source's own, so
    /// the database's grants limit what an assistant can do. Its password has a keychain
    /// entry of its own. Only engines that sign in with a password use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_login: Option<McpLogin>,
    #[serde(flatten)]
    pub config: ConnectionConfig,
}

/// The database user `dbrover mcp` signs in as, written in `data_sources.toml` as
/// `mcp_login = { user = "assistant" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLogin {
    pub user: String,
}

/// The colours a data source can be marked with.
///
/// A fixed set of names rather than free-form RGB: the app picks each one's shade per theme,
/// so a colour chosen in the dark theme still reads in the light one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Violet,
    Pink,
}

impl SourceColor {
    pub const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Blue,
        Self::Violet,
        Self::Pink,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Teal => "Teal",
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Pink => "Pink",
        }
    }
}

/// How careful DbRover is with a data source's data. Its own setting, apart from the colour:
/// a colour is only a look, though red suggests [`Careful`](Self::Careful).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafetyLevel {
    /// Changes run when asked; only an `UPDATE` or `DELETE` without `WHERE` asks first.
    #[default]
    Normal,
    /// Every change asks first, naming the data source: statements that change data or the
    /// schema, and grid edits. New consoles start in manual commit mode.
    Careful,
    /// Changes are refused: statements are classified before they are sent, and run in the
    /// database's read-only mode, as the MCP server's read-only projects are.
    ReadOnly,
}

impl SafetyLevel {
    pub const ALL: [Self; 3] = [Self::Normal, Self::Careful, Self::ReadOnly];

    pub fn is_normal(&self) -> bool {
        *self == Self::Normal
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Careful => "Careful",
            Self::ReadOnly => "Read-only",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Normal => "Changes run when asked. UPDATE or DELETE without WHERE asks first.",
            Self::Careful => {
                "Every change asks first: statements and grid edits. Consoles start in manual \
                 commit mode."
            }
            Self::ReadOnly => "Changes are refused, by DbRover and by the database.",
        }
    }

    /// The level a colour suggests: red usually marks production.
    pub fn suggested_by(color: Option<SourceColor>) -> Option<Self> {
        (color == Some(SourceColor::Red)).then_some(Self::Careful)
    }
}

/// Engine-specific connection settings.
///
/// A closed enum rather than an open driver registry: each variant needs its own form in the
/// UI and its own driver crate anyway, and the compiler then points at every place a new
/// engine must be handled. See `docs/adr/0002-native-drivers-behind-a-trait.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "driver", rename_all = "lowercase")]
pub enum ConnectionConfig {
    Postgres(PostgresConfig),
    Sqlite(SqliteConfig),
    /// MySQL and MariaDB: one driver serves both.
    Mysql(MysqlConfig),
    /// Microsoft SQL Server and Azure SQL.
    #[serde(rename = "sqlserver")]
    SqlServer(SqlServerConfig),
    /// DuckDB, in a file or in memory.
    #[serde(rename = "duckdb")]
    DuckDb(DuckDbConfig),
    #[serde(rename = "clickhouse")]
    ClickHouse(ClickHouseConfig),
    /// Redis, Valkey and Dragonfly: one driver serves all three.
    Redis(RedisConfig),
    #[serde(rename = "mongodb")]
    Mongo(MongoConfig),
    /// Elasticsearch and OpenSearch: one driver serves both.
    Elasticsearch(ElasticsearchConfig),
    /// Apache Cassandra and ScyllaDB: one driver serves both.
    Cassandra(CassandraConfig),
}

impl ConnectionConfig {
    /// The same configuration signing in as `user`; `None` for engines without a login.
    pub fn with_user(&self, user: &str) -> Option<Self> {
        let mut config = self.clone();
        let server = config.server_mut()?;
        user.clone_into(&mut server.user);
        Some(config)
    }

    /// Where the server is, for engines that connect over the network.
    pub fn server(&self) -> Option<&ServerConfig> {
        match self {
            Self::Postgres(config) => Some(&config.server),
            Self::Mysql(config) => Some(&config.server),
            Self::SqlServer(config) => Some(&config.server),
            Self::ClickHouse(config) => Some(&config.server),
            Self::Redis(config) => Some(&config.server),
            Self::Mongo(config) => Some(&config.server),
            Self::Elasticsearch(config) => Some(&config.server),
            Self::Cassandra(config) => Some(&config.server),
            Self::Sqlite(_) | Self::DuckDb(_) => None,
        }
    }

    fn server_mut(&mut self) -> Option<&mut ServerConfig> {
        match self {
            Self::Postgres(config) => Some(&mut config.server),
            Self::Mysql(config) => Some(&mut config.server),
            Self::SqlServer(config) => Some(&mut config.server),
            Self::ClickHouse(config) => Some(&mut config.server),
            Self::Redis(config) => Some(&mut config.server),
            Self::Mongo(config) => Some(&mut config.server),
            Self::Elasticsearch(config) => Some(&mut config.server),
            Self::Cassandra(config) => Some(&mut config.server),
            Self::Sqlite(_) | Self::DuckDb(_) => None,
        }
    }

    pub fn driver(&self) -> DriverKind {
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

    /// The database a connection opens, on engines whose catalog has databases above its
    /// schemas; the explorer lists that database's schemas. MySQL's and ClickHouse's databases
    /// are their schemas, so they have no such level, Redis's numbered databases hold keys,
    /// MongoDB's hold collections, and Elasticsearch has none. Cassandra's keyspaces are its
    /// schemas.
    pub fn database(&self) -> Option<&str> {
        match self {
            Self::Postgres(config) => Some(&config.server.database),
            Self::SqlServer(config) => Some(&config.server.database),
            Self::DuckDb(config) => config.database_name(),
            Self::Sqlite(_)
            | Self::Mysql(_)
            | Self::ClickHouse(_)
            | Self::Redis(_)
            | Self::Mongo(_)
            | Self::Elasticsearch(_)
            | Self::Cassandra(_) => None,
        }
    }

    /// The schema unqualified names resolve to, as a new connection sees it. A fact of the
    /// configuration rather than of the dialect: on MySQL and ClickHouse it is the database
    /// connected to.
    /// On SQL Server it is each user's own setting, `dbo` unless an administrator chose
    /// another.
    pub fn default_schema(&self) -> &str {
        match self {
            Self::Postgres(_) => "public",
            Self::SqlServer(_) => "dbo",
            Self::DuckDb(_) => "main",
            Self::ClickHouse(config) => &config.server.database,
            Self::Sqlite(_) => "main",
            Self::Mysql(config) => &config.server.database,
            // The number of the database a connection selects, which keys belong to.
            Self::Redis(config) => &config.server.database,
            // The database a console's commands run in until `use` changes it.
            Self::Mongo(config) => &config.server.database,
            // Empty: indices belong to no database.
            Self::Elasticsearch(config) => &config.server.database,
            // The keyspace a console starts in, which may be empty: then only qualified names
            // resolve until `USE` picks one.
            Self::Cassandra(config) => &config.server.database,
        }
    }

    /// These settings with every `${NAME}` placeholder replaced, and a leading `~` in paths
    /// replaced by the home folder: what a driver connects with.
    ///
    /// # Errors
    ///
    /// [`PlaceholderError`] naming the setting that uses an unset variable, or whose port
    /// doesn't resolve to a number.
    pub fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        Ok(match self {
            Self::Postgres(config) => Self::Postgres(PostgresConfig {
                server: config.server.resolve(variables)?,
            }),
            Self::Mysql(config) => Self::Mysql(MysqlConfig {
                server: config.server.resolve(variables)?,
            }),
            Self::SqlServer(config) => Self::SqlServer(SqlServerConfig {
                server: config.server.resolve(variables)?,
            }),
            Self::ClickHouse(config) => Self::ClickHouse(ClickHouseConfig {
                server: config.server.resolve(variables)?,
                read_only: config.read_only,
            }),
            Self::Redis(config) => Self::Redis(RedisConfig {
                server: config.server.resolve(variables)?,
                separator: config.separator.clone(),
            }),
            Self::Mongo(config) => Self::Mongo(MongoConfig {
                server: config.server.resolve(variables)?,
                auth_database: expand(&config.auth_database, variables)
                    .map_err(PlaceholderError::in_setting(Setting::AuthDatabase))?,
                connection_string: config
                    .connection_string
                    .as_deref()
                    .map(|text| {
                        expand(text, variables)
                            .map_err(PlaceholderError::in_setting(Setting::ConnectionString))
                    })
                    .transpose()?,
            }),
            Self::Elasticsearch(config) => Self::Elasticsearch(ElasticsearchConfig {
                server: config.server.resolve(variables)?,
                auth: config.auth,
            }),
            Self::Cassandra(config) => Self::Cassandra(CassandraConfig {
                server: config.server.resolve(variables)?,
                local_datacenter: expand(&config.local_datacenter, variables)
                    .map_err(PlaceholderError::in_setting(Setting::LocalDatacenter))?,
                statement_timeout: config.statement_timeout,
            }),
            Self::Sqlite(config) => Self::Sqlite(SqliteConfig {
                path: expand_path(&config.path, variables)
                    .map_err(PlaceholderError::in_setting(Setting::File))?,
                read_only: config.read_only,
            }),
            Self::DuckDb(config) => Self::DuckDb(DuckDbConfig {
                database: match &config.database {
                    DuckDbDatabase::File { path, read_only } => DuckDbDatabase::File {
                        path: expand_path(path, variables)
                            .map_err(PlaceholderError::in_setting(Setting::File))?,
                        read_only: *read_only,
                    },
                    DuckDbDatabase::InMemory => DuckDbDatabase::InMemory,
                },
            }),
        })
    }

    /// A name for a data source the user didn't name: `database@host` (`db0@host` on Redis), or
    /// the file name.
    pub fn default_name(&self) -> String {
        match self {
            Self::Postgres(config) => config.server.default_name(),
            Self::Mysql(config) => config.server.default_name(),
            Self::SqlServer(config) => config.server.default_name(),
            Self::ClickHouse(config) => config.server.default_name(),
            // `db0@localhost`, as the explorer labels a numbered database.
            Self::Redis(config) => format!("db{}", config.server.default_name()),
            Self::Mongo(config) => config.server.default_name(),
            // The host alone: a cluster has no database to name.
            Self::Elasticsearch(config) => config.server.host.clone(),
            // `keyspace@host`, or the host alone when no keyspace is chosen.
            Self::Cassandra(config) if config.server.database.is_empty() => {
                config.server.host.clone()
            }
            Self::Cassandra(config) => config.server.default_name(),
            Self::Sqlite(config) => file_name(&config.path),
            Self::DuckDb(config) => match &config.database {
                DuckDbDatabase::File { path, .. } => file_name(path),
                DuckDbDatabase::InMemory => DuckDbConfig::IN_MEMORY_NAME.to_owned(),
            },
        }
    }
}

impl DriverKind {
    /// Whether connections authenticate with a password, so the keychain is worth asking.
    /// Skipping the lookup for other drivers avoids pointless keychain prompts.
    pub fn uses_password(self) -> bool {
        match self {
            Self::Postgres
            | Self::Mysql
            | Self::SqlServer
            | Self::ClickHouse
            | Self::Redis
            | Self::Mongo
            | Self::Elasticsearch
            | Self::Cassandra => true,
            Self::Sqlite | Self::DuckDb => false,
        }
    }
}

/// Describes where the connection goes, for logs and tooltips. Never includes secrets.
impl fmt::Display for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres(config) => config.server.fmt(f),
            Self::Mysql(config) => config.server.fmt(f),
            Self::SqlServer(config) => config.server.fmt(f),
            Self::ClickHouse(config) => config.server.fmt(f),
            Self::Redis(config) => config.server.fmt(f),
            Self::Mongo(config) => match &config.connection_string {
                Some(_) => write!(f, "{} (connection string)", config.server.host),
                None => config.server.fmt(f),
            },
            Self::Elasticsearch(config) => config.server.fmt(f),
            Self::Cassandra(config) => config.server.fmt(f),
            Self::Sqlite(config) if config.read_only => {
                write!(f, "{} (read-only)", config.path.display())
            }
            Self::Sqlite(config) => write!(f, "{}", config.path.display()),
            Self::DuckDb(config) => match &config.database {
                DuckDbDatabase::File {
                    path,
                    read_only: true,
                } => write!(f, "{} (read-only)", path.display()),
                DuckDbDatabase::File { path, .. } => write!(f, "{}", path.display()),
                DuckDbDatabase::InMemory => f.write_str("in memory"),
            },
        }
    }
}

/// The file name of `path`, or the whole path if it has none.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Where a database server is and whom to connect as: what every network engine's settings
/// start with, and what the data source dialog's server section edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: Port,
    pub database: String,
    pub user: String,
    /// Absent from files written before TLS existed, which then get the default.
    #[serde(default)]
    pub tls: TlsMode,
    /// The SSH server to reach the database through, for databases only a jump host can
    /// reach. Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfig>,
    /// A program to run for the password on every connection, in place of the keychain's,
    /// for tokens that expire. Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_command: Option<PasswordCommand>,
}

impl ServerConfig {
    fn default_name(&self) -> String {
        format!("{}@{}", self.database, self.host)
    }

    /// The port to connect to, for drivers.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connect`] if the port is still a placeholder: the engine resolves
    /// every setting before it connects, so this points at a caller that skipped that.
    pub fn port_number(&self) -> Result<u16, DriverError> {
        self.port.number().ok_or_else(|| {
            DriverError::Connect(
                format!("the port {} has not been resolved to a number", self.port).into(),
            )
        })
    }

    fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        let text = |text: &str, setting| {
            expand(text, variables).map_err(PlaceholderError::in_setting(setting))
        };
        Ok(Self {
            host: text(&self.host, Setting::Host)?,
            port: self.port.resolve(Setting::Port, variables)?,
            database: text(&self.database, Setting::Database)?,
            user: text(&self.user, Setting::User)?,
            tls: match &self.tls {
                TlsMode::VerifyFull {
                    ca_file: Some(path),
                } => TlsMode::VerifyFull {
                    ca_file: Some(
                        expand_path(path, variables)
                            .map_err(PlaceholderError::in_setting(Setting::CaFile))?,
                    ),
                },
                tls => tls.clone(),
            },
            ssh: self
                .ssh
                .as_ref()
                .map(|ssh| ssh.resolve(variables))
                .transpose()?,
            // Its arguments are filled in from the settings resolved here, as it runs. `${NAME}`
            // in an argument stays as written: the program gets DbRover's environment and can
            // read the variable itself.
            password_command: self.password_command.clone(),
        })
    }
}

/// An SSH server that forwards connections to the database server: a jump host, or the
/// database's own machine when its port is only open to itself. DbRover opens a tunnel to it
/// before the driver connects, and the driver connects through the tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    pub port: Port,
    pub user: String,
    #[serde(flatten)]
    pub login: SshLogin,
}

impl SshConfig {
    pub const DEFAULT_PORT: u16 = 22;

    /// The port to connect to, once resolved; see [`ServerConfig::port_number`].
    ///
    /// # Errors
    ///
    /// [`DriverError::Connect`] if the port is still a placeholder.
    pub fn port_number(&self) -> Result<u16, DriverError> {
        self.port.number().ok_or_else(|| {
            DriverError::Connect(
                format!(
                    "the SSH port {} has not been resolved to a number",
                    self.port
                )
                .into(),
            )
        })
    }

    fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        let text = |text: &str, setting| {
            expand(text, variables).map_err(PlaceholderError::in_setting(setting))
        };
        Ok(Self {
            host: text(&self.host, Setting::SshHost)?,
            port: self.port.resolve(Setting::SshPort, variables)?,
            user: text(&self.user, Setting::SshUser)?,
            login: match &self.login {
                SshLogin::KeyFile { key_file } => SshLogin::KeyFile {
                    key_file: expand_path(key_file, variables)
                        .map_err(PlaceholderError::in_setting(Setting::SshKeyFile))?,
                },
                login @ (SshLogin::Agent | SshLogin::Password) => login.clone(),
            },
        })
    }
}

/// `user@host:port`.
impl fmt::Display for SshConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}:{}", self.user, self.host, self.port)
    }
}

/// How DbRover signs in to the SSH server. The secret a key file or password needs is kept in
/// the keychain, never in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "login", rename_all = "kebab-case")]
pub enum SshLogin {
    /// With the keys the running SSH agent holds (`SSH_AUTH_SOCK`, or Pageant on Windows).
    Agent,
    /// With a private key file, in OpenSSH's format. An encrypted key's passphrase is the
    /// secret.
    KeyFile { key_file: PathBuf },
    /// With the SSH user's password, which is the secret.
    Password,
}

impl SshLogin {
    /// Whether signing in may need a secret from the keychain: a password always does, a key
    /// file only when it is encrypted.
    pub fn may_need_secret(&self) -> bool {
        !matches!(self, Self::Agent)
    }
}

/// Where a network driver opens its TCP connections: to the server in its settings, or to
/// the local end of an SSH tunnel that forwards to it. Either way, TLS checks the server's
/// certificate against [`ServerConfig::host`], the name the user gave it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Direct,
    Tunnel(SocketAddr),
}

/// A server's port: a number, or text with a `${NAME}` placeholder that resolves to one when
/// connecting. Written to the file as a number, or as a string when it has a placeholder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Port {
    Number(u16),
    Placeholder(String),
}

impl Port {
    /// The port number, once resolved. `None` for a placeholder: drivers only see ports that
    /// [`ConnectionConfig::resolve`] has turned into numbers.
    pub fn number(&self) -> Option<u16> {
        match self {
            Self::Number(port) => Some(*port),
            Self::Placeholder(_) => None,
        }
    }

    fn resolve(
        &self,
        setting: Setting,
        variables: &dyn Variables,
    ) -> Result<Self, PlaceholderError> {
        let Self::Placeholder(text) = self else {
            return Ok(self.clone());
        };
        let value = expand(text, variables).map_err(PlaceholderError::in_setting(setting))?;
        value
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .map(Self::Number)
            .ok_or(PlaceholderError::InvalidPort { setting, value })
    }
}

impl From<u16> for Port {
    fn from(port: u16) -> Self {
        Self::Number(port)
    }
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(port) => port.fmt(f),
            Self::Placeholder(text) => f.write_str(text),
        }
    }
}

/// `user@host:port/database`, then `via` and the SSH server when there is one.
impl fmt::Display for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )?;
        match &self.ssh {
            Some(ssh) => write!(f, " via {ssh}"),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresConfig {
    /// Flattened, so the file keeps `host`, `port` and the rest next to `driver`, as it did
    /// before the server settings were shared between engines.
    #[serde(flatten)]
    pub server: ServerConfig,
}

impl PostgresConfig {
    pub const DEFAULT_PORT: u16 = 5432;
}

/// A MySQL or MariaDB server. Its `database` is the one connected to, which unqualified
/// names resolve in, and the explorer lists every database the user can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MysqlConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
}

impl MysqlConfig {
    pub const DEFAULT_PORT: u16 = 3306;
}

/// A SQL Server or Azure SQL database, signed in to with a SQL Server login. Its `database` is
/// the one connected to; the explorer lists every database the login may open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlServerConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
}

impl SqlServerConfig {
    pub const DEFAULT_PORT: u16 = 1433;
}

/// Whether and how a network connection is encrypted. The modes are libpq's `sslmode` values
/// of the same names, so they mean what PostgreSQL users already expect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum TlsMode {
    /// Never encrypt.
    Disable,
    /// Encrypt if the server offers it, without checking its certificate; otherwise connect
    /// unencrypted. The default, as in libpq: it changes nothing for servers without TLS.
    #[default]
    Prefer,
    /// Always encrypt, without checking the server's certificate. Protects against someone
    /// reading the traffic, not against someone impersonating the server.
    Require,
    /// Always encrypt, and check that the certificate is signed by a trusted authority and
    /// names the host connected to.
    VerifyFull {
        /// A PEM file with the authorities to trust instead of the public web ones, for
        /// servers with a private or self-signed certificate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ca_file: Option<PathBuf>,
    },
}

/// A ClickHouse server, reached through its HTTP interface. Its `database` is the one
/// unqualified names resolve in, and the explorer lists every database the user can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
    /// Whether every request carries `readonly = 2`, so the server refuses any change. Never
    /// saved: a read-only data source's connections are opened with it, and its safety level
    /// is the setting users see.
    #[serde(skip)]
    pub read_only: bool,
}

impl ClickHouseConfig {
    /// The HTTP interface's port.
    pub const DEFAULT_PORT: u16 = 8123;
    /// The HTTPS interface's port: TLS is a port of its own, not an upgrade of the HTTP one.
    pub const DEFAULT_TLS_PORT: u16 = 8443;
}

/// A Redis, Valkey or Dragonfly server. Its `database` is the number of the database a
/// connection selects, `0` unless told otherwise; its user is optional, and an empty one signs in
/// as the server's `default` user. The explorer lists every database that holds keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedisConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
    /// What the explorer groups keys by: `user:42:cart` sits under `user` › `42` with `:`.
    /// Empty lists keys ungrouped. Written only when it isn't `:`.
    #[serde(
        default = "RedisConfig::default_separator",
        skip_serializing_if = "RedisConfig::is_default_separator"
    )]
    pub separator: String,
}

impl RedisConfig {
    pub const DEFAULT_PORT: u16 = 6379;
    /// The database a new data source selects.
    pub const DEFAULT_DATABASE: &str = "0";
    /// The separator most applications' keys use.
    pub const DEFAULT_SEPARATOR: &str = ":";

    fn default_separator() -> String {
        Self::DEFAULT_SEPARATOR.to_owned()
    }

    fn is_default_separator(separator: &str) -> bool {
        separator == Self::DEFAULT_SEPARATOR
    }
}

/// A MongoDB server: the shared server form, or a connection string pasted whole.
///
/// The server's `database` is the one a console's commands run in until `use` changes it; the
/// explorer lists every database the user may see. Its user is optional, for a server without
/// access control, and signs in against `auth_database` (`admin` when empty).
///
/// With a connection string, the host, port, TLS and SSH tunnel are the string's, and `host` only
/// names the data source: the string may name several hosts, or a DNS record (`mongodb+srv://`).
/// The user and password are kept out of the string, the password in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
    /// Written only when set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_database: String,
    /// `mongodb://…` or `mongodb+srv://…`, without its user and password. Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_string: Option<String>,
}

impl MongoConfig {
    pub const DEFAULT_PORT: u16 = 27017;
    /// The database a new data source's consoles start in, as the shell's do.
    pub const DEFAULT_DATABASE: &str = "test";
    /// Where a user's credentials are kept unless the data source says otherwise.
    pub const DEFAULT_AUTH_DATABASE: &str = "admin";
}

/// An Elasticsearch or OpenSearch cluster, reached through one of its nodes. Its `database` is
/// empty, since indices belong to no database, and its user is used only when it signs in with
/// a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElasticsearchConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
    /// Written only for an API key.
    #[serde(default, skip_serializing_if = "ElasticsearchAuth::is_password")]
    pub auth: ElasticsearchAuth,
}

impl ElasticsearchConfig {
    /// The REST API's port, plain or over TLS, as the servers configure it.
    pub const DEFAULT_PORT: u16 = 9200;
}

/// What an Elasticsearch data source signs in with. Either way the secret is kept where a
/// password is (the keychain, or a password command), so there is one per data source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticsearchAuth {
    /// The server's user and its password, over HTTP basic authentication.
    #[default]
    Password,
    /// An API key, in Elasticsearch's encoded form (base64 of `id:key`), in the password's place.
    ApiKey,
}

impl ElasticsearchAuth {
    pub fn is_password(&self) -> bool {
        *self == Self::Password
    }
}

/// An Apache Cassandra or ScyllaDB cluster, reached through one of its nodes. Its `database` is
/// the keyspace a console starts in, which may be empty; its user may be empty too, for a
/// cluster that doesn't ask for a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassandraConfig {
    #[serde(flatten)]
    pub server: ServerConfig,
    /// The datacenter requests go to first, as the cluster names it (`datacenter1`). Empty
    /// prefers none, which a cluster in one datacenter doesn't need. Written only when set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local_datacenter: String,
    /// How long a statement may run, in seconds, before the client stops waiting and, on
    /// ScyllaDB, the server stops it too (`USING TIMEOUT`). `None` leaves the server's own
    /// timeouts. Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_timeout: Option<NonZeroU32>,
}

impl CassandraConfig {
    /// CQL's native protocol port.
    pub const DEFAULT_PORT: u16 = 9042;
    /// The port most servers take CQL over TLS on, beside the plain one.
    pub const DEFAULT_TLS_PORT: u16 = 9142;
}

/// A DuckDB database: a file, or one in memory that starts empty each time, for querying CSV,
/// Parquet and JSON files with DuckDB's table functions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuckDbConfig {
    #[serde(flatten)]
    pub database: DuckDbDatabase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "database", rename_all = "kebab-case")]
pub enum DuckDbDatabase {
    File {
        path: PathBuf,
        #[serde(default)]
        read_only: bool,
    },
    InMemory,
}

impl DuckDbConfig {
    /// What DuckDB calls an in-memory database in its catalog.
    pub const IN_MEMORY_NAME: &str = "memory";

    /// The name DuckDB gives the database it opens: the file's name without its extension, or
    /// `memory`. `None` for a file name that isn't valid UTF-8.
    pub fn database_name(&self) -> Option<&str> {
        match &self.database {
            DuckDbDatabase::File { path, .. } => path.file_stem()?.to_str(),
            DuckDbDatabase::InMemory => Some(Self::IN_MEMORY_NAME),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteConfig {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverKind {
    Postgres,
    Sqlite,
    Mysql,
    SqlServer,
    DuckDb,
    ClickHouse,
    Redis,
    Mongo,
    Elasticsearch,
    Cassandra,
}

impl DriverKind {
    pub const ALL: [Self; 10] = [
        Self::Postgres,
        Self::Sqlite,
        Self::Mysql,
        Self::SqlServer,
        Self::DuckDb,
        Self::ClickHouse,
        Self::Redis,
        Self::Mongo,
        Self::Elasticsearch,
        Self::Cassandra,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL / MariaDB",
            Self::SqlServer => "SQL Server",
            Self::DuckDb => "DuckDB",
            Self::ClickHouse => "ClickHouse",
            Self::Redis => "Redis / Valkey",
            Self::Mongo => "MongoDB",
            Self::Elasticsearch => "Elasticsearch / OpenSearch",
            Self::Cassandra => "Cassandra / ScyllaDB",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placeholder::tests::Fixed;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct File {
        data_source: Vec<DataSource>,
    }

    #[test]
    fn round_trips_through_toml_with_a_driver_tag() {
        let file = File {
            data_source: vec![
                DataSource {
                    id: DataSourceId::generate(),
                    name: "local".to_owned(),
                    color: None,
                    safety: SafetyLevel::Normal,
                    project: None,
                    mcp_login: None,
                    config: ConnectionConfig::Postgres(PostgresConfig {
                        server: ServerConfig {
                            host: "localhost".to_owned(),
                            port: 5432.into(),
                            database: "app".to_owned(),
                            user: "juan".to_owned(),
                            tls: TlsMode::VerifyFull {
                                ca_file: Some("/etc/ssl/internal-ca.pem".into()),
                            },
                            ssh: None,
                            password_command: None,
                        },
                    }),
                },
                DataSource {
                    id: DataSourceId::generate(),
                    name: "chinook".to_owned(),
                    color: None,
                    safety: SafetyLevel::Normal,
                    project: None,
                    mcp_login: None,
                    config: ConnectionConfig::Sqlite(SqliteConfig {
                        path: "/tmp/chinook.db".into(),
                        read_only: true,
                    }),
                },
            ],
        };

        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("driver = \"postgres\""), "{text}");
        assert!(text.contains("driver = \"sqlite\""), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    #[test]
    fn a_redis_data_source_round_trips_with_its_database_number() {
        let file = File {
            data_source: vec![DataSource {
                id: DataSourceId::generate(),
                name: "cache".to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project: None,
                mcp_login: Some(McpLogin {
                    user: "mcp".to_owned(),
                }),
                config: ConnectionConfig::Redis(RedisConfig {
                    server: ServerConfig {
                        host: "localhost".to_owned(),
                        port: RedisConfig::DEFAULT_PORT.into(),
                        database: "2".to_owned(),
                        user: String::new(),
                        tls: TlsMode::Require,
                        ssh: None,
                        password_command: None,
                    },
                    separator: RedisConfig::DEFAULT_SEPARATOR.to_owned(),
                }),
            }],
        };
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("driver = \"redis\""), "{text}");
        assert!(!text.contains("separator"), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
        assert_eq!(file.data_source[0].config.default_name(), "db2@localhost");
        assert_eq!(file.data_source[0].config.driver(), DriverKind::Redis);
        assert!(DriverKind::Redis.uses_password());

        let grouped_by_dots = text.replace(
            "driver = \"redis\"",
            "driver = \"redis\"\nseparator = \".\"",
        );
        let read = toml::from_str::<File>(&grouped_by_dots).unwrap();
        let ConnectionConfig::Redis(config) = &read.data_source[0].config else {
            panic!("expected a Redis data source");
        };
        assert_eq!(config.separator, ".");
    }

    #[test]
    fn a_mongodb_data_source_round_trips_with_its_connection_string() {
        let file = File {
            data_source: vec![DataSource {
                id: DataSourceId::generate(),
                name: "atlas".to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config: ConnectionConfig::Mongo(MongoConfig {
                    server: ServerConfig {
                        host: "cluster0.example.mongodb.net".to_owned(),
                        port: MongoConfig::DEFAULT_PORT.into(),
                        database: "shop".to_owned(),
                        user: "app".to_owned(),
                        tls: TlsMode::Disable,
                        ssh: None,
                        password_command: None,
                    },
                    auth_database: String::new(),
                    connection_string: Some(
                        "mongodb+srv://cluster0.example.mongodb.net/shop".to_owned(),
                    ),
                }),
            }],
        };
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("driver = \"mongodb\""), "{text}");
        assert!(!text.contains("auth_database"), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
        assert_eq!(
            file.data_source[0].config.default_name(),
            "shop@cluster0.example.mongodb.net"
        );
        assert_eq!(
            file.data_source[0].config.to_string(),
            "cluster0.example.mongodb.net (connection string)"
        );
    }

    #[test]
    fn an_elasticsearch_data_source_writes_its_api_key_choice_only_when_made() {
        let data_source = |auth| DataSource {
            id: DataSourceId::generate(),
            name: "logs".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Elasticsearch(ElasticsearchConfig {
                server: ServerConfig {
                    host: "search.example.com".to_owned(),
                    port: ElasticsearchConfig::DEFAULT_PORT.into(),
                    database: String::new(),
                    user: String::new(),
                    tls: TlsMode::VerifyFull { ca_file: None },
                    ssh: None,
                    password_command: None,
                },
                auth,
            }),
        };
        for (auth, written) in [
            (ElasticsearchAuth::Password, None),
            (ElasticsearchAuth::ApiKey, Some("auth = \"api_key\"")),
        ] {
            let file = File {
                data_source: vec![data_source(auth)],
            };
            let text = toml::to_string(&file).unwrap();
            assert!(text.contains("driver = \"elasticsearch\""), "{text}");
            assert_eq!(text.contains("auth ="), written.is_some(), "{text}");
            if let Some(written) = written {
                assert!(text.contains(written), "{text}");
            }
            assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
            assert_eq!(
                file.data_source[0].config.default_name(),
                "search.example.com"
            );
        }
    }

    #[test]
    fn writes_an_mcp_login_only_when_there_is_one() {
        let server = ServerConfig {
            host: "localhost".to_owned(),
            port: 5432.into(),
            database: "app".to_owned(),
            user: "juan".to_owned(),
            tls: TlsMode::Prefer,
            ssh: None,
            password_command: None,
        };
        let mut data_source = DataSource {
            id: DataSourceId::generate(),
            name: "local".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Postgres(PostgresConfig { server }),
        };
        let without = toml::to_string(&File {
            data_source: vec![data_source.clone()],
        })
        .unwrap();
        assert!(!without.contains("mcp_login"), "{without}");

        data_source.mcp_login = Some(McpLogin {
            user: "assistant".to_owned(),
        });
        let file = File {
            data_source: vec![data_source],
        };
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("user = \"assistant\""), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    #[test]
    fn swaps_the_user_of_engines_that_sign_in() {
        let server = ServerConfig {
            host: "localhost".to_owned(),
            port: 3306.into(),
            database: "shop".to_owned(),
            user: "owner".to_owned(),
            tls: TlsMode::Prefer,
            ssh: None,
            password_command: None,
        };
        let mysql = ConnectionConfig::Mysql(MysqlConfig { server });

        let Some(ConnectionConfig::Mysql(as_assistant)) = mysql.with_user("assistant") else {
            panic!("expected a MySQL configuration");
        };
        assert_eq!(as_assistant.server.user, "assistant");
        assert_eq!(as_assistant.server.database, "shop");
        let sqlite = ConnectionConfig::Sqlite(SqliteConfig {
            path: "/tmp/a.db".into(),
            read_only: false,
        });
        assert_eq!(sqlite.with_user("assistant"), None);
    }

    #[test]
    fn tls_defaults_to_prefer_in_files_written_before_it_existed() {
        let without = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "local"
            driver = "postgres"
            host = "localhost"
            port = 5432
            database = "app"
            user = "juan"
        "#;

        let file: File = toml::from_str(without).unwrap();

        let ConnectionConfig::Postgres(config) = &file.data_source[0].config else {
            panic!("expected a PostgreSQL data source");
        };
        assert_eq!(config.server.tls, TlsMode::Prefer);
    }

    #[test]
    fn writes_tls_as_a_mode_with_its_ca_file() {
        let modes = [
            (TlsMode::Disable, "mode = \"disable\""),
            (TlsMode::Require, "mode = \"require\""),
            (
                TlsMode::VerifyFull {
                    ca_file: Some("/ca.pem".into()),
                },
                "ca_file = \"/ca.pem\"",
            ),
            (
                TlsMode::VerifyFull { ca_file: None },
                "mode = \"verify-full\"",
            ),
        ];
        for (tls, expected) in modes {
            let file = File {
                data_source: vec![DataSource {
                    id: DataSourceId::generate(),
                    name: "local".to_owned(),
                    color: None,
                    safety: SafetyLevel::Normal,
                    project: None,
                    mcp_login: None,
                    config: ConnectionConfig::Postgres(PostgresConfig {
                        server: ServerConfig {
                            host: "localhost".to_owned(),
                            port: 5432.into(),
                            database: "app".to_owned(),
                            user: "juan".to_owned(),
                            tls,
                            ssh: None,
                            password_command: None,
                        },
                    }),
                }],
            };

            let text = toml::to_string(&file).unwrap();

            assert!(text.contains(expected), "{text}");
            assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
        }
    }

    #[test]
    fn reads_a_mysql_data_source_with_its_driver_tag() {
        let text = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "shop"
            driver = "mysql"
            host = "localhost"
            port = 3306
            database = "shop"
            user = "root"
        "#;

        let file: File = toml::from_str(text).unwrap();

        let config = &file.data_source[0].config;
        assert_eq!(config.driver(), DriverKind::Mysql);
        assert_eq!(config.default_schema(), "shop");
        assert_eq!(config.database(), None);
        assert_eq!(config.to_string(), "root@localhost:3306/shop");
        assert_eq!(
            toml::from_str::<File>(&toml::to_string(&file).unwrap()).unwrap(),
            file
        );
    }

    #[test]
    fn reads_a_sql_server_data_source_with_its_driver_tag() {
        let text = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "sales"
            driver = "sqlserver"
            host = "localhost"
            port = 1433
            database = "sales"
            user = "sa"

            [data_source.tls]
            mode = "require"
        "#;

        let file: File = toml::from_str(text).unwrap();

        let config = &file.data_source[0].config;
        assert_eq!(config.driver(), DriverKind::SqlServer);
        assert_eq!(config.default_schema(), "dbo");
        assert_eq!(config.database(), Some("sales"));
        assert_eq!(config.to_string(), "sa@localhost:1433/sales");
        assert_eq!(
            toml::from_str::<File>(&toml::to_string(&file).unwrap()).unwrap(),
            file
        );
    }

    #[test]
    fn writes_duckdb_files_and_in_memory_databases_with_their_tags() {
        let cases = [
            (
                DuckDbDatabase::File {
                    path: "/data/orders.duckdb".into(),
                    read_only: true,
                },
                "database = \"file\"",
                Some("orders"),
                "orders.duckdb",
                "/data/orders.duckdb (read-only)",
            ),
            (
                DuckDbDatabase::InMemory,
                "database = \"in-memory\"",
                Some("memory"),
                "memory",
                "in memory",
            ),
        ];
        for (database, tag, catalog_name, default_name, shown) in cases {
            let file = File {
                data_source: vec![DataSource {
                    id: DataSourceId::generate(),
                    name: "duck".to_owned(),
                    color: None,
                    safety: SafetyLevel::Normal,
                    project: None,
                    mcp_login: None,
                    config: ConnectionConfig::DuckDb(DuckDbConfig { database }),
                }],
            };

            let text = toml::to_string(&file).unwrap();

            assert!(text.contains("driver = \"duckdb\""), "{text}");
            assert!(text.contains(tag), "{text}");
            assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
            let config = &file.data_source[0].config;
            assert_eq!(config.database(), catalog_name);
            assert_eq!(config.default_schema(), "main");
            assert_eq!(config.default_name(), default_name);
            assert_eq!(config.to_string(), shown);
        }
    }

    #[test]
    fn colour_is_optional_and_written_only_when_set() {
        let without = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "chinook"
            driver = "sqlite"
            path = "/tmp/chinook.db"
        "#;
        let mut file: File = toml::from_str(without).unwrap();
        assert_eq!(file.data_source[0].color, None);
        assert!(!toml::to_string(&file).unwrap().contains("color"));

        file.data_source[0].color = Some(SourceColor::Violet);
        let text = toml::to_string(&file).unwrap();

        assert!(text.contains("color = \"violet\""), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    /// A file as DbRover 0.2.0 wrote it, before projects existed.
    const FILE_FROM_0_2_0: &str = r#"
        # DbRover data sources. Safe to edit by hand while DbRover is closed.
        # Passwords are stored in the OS keychain, never in this file.
        # Comments are not preserved when DbRover saves this file.

        [[data_source]]
        id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
        name = "production"
        color = "red"
        driver = "postgres"
        host = "db.internal"
        port = 5432
        database = "app"
        user = "juan"

        [data_source.tls]
        mode = "verify-full"
        ca_file = "/etc/ssl/internal-ca.pem"

        [[data_source]]
        id = "0d6c2f4e-1b8a-4c3d-9e7f-5a6b7c8d9e0f"
        name = "shop"
        driver = "mysql"
        host = "localhost"
        port = 3306
        database = "shop"
        user = "root"

        [data_source.tls]
        mode = "prefer"

        [[data_source]]
        id = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d"
        name = "chinook.db"
        driver = "sqlite"
        path = "/data/chinook.db"
        read_only = true
    "#;

    #[test]
    fn files_from_0_2_0_load_unchanged_with_every_data_source_in_no_project() {
        let file: File = toml::from_str(FILE_FROM_0_2_0).unwrap();

        assert_eq!(file.data_source.len(), 3);
        assert!(file.data_source.iter().all(|ds| ds.project.is_none()));
        assert_eq!(file.data_source[0].color, Some(SourceColor::Red));
        let text = toml::to_string(&file).unwrap();
        assert!(!text.contains("project"), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    #[test]
    fn writes_the_project_only_when_set() {
        let mut file: File = toml::from_str(FILE_FROM_0_2_0).unwrap();
        let project = ProjectId::generate();
        file.data_source[2].project = Some(project);

        let text = toml::to_string(&file).unwrap();

        assert_eq!(text.matches("project = ").count(), 1, "{text}");
        assert!(text.contains(&format!("project = \"{project}\"")), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    #[test]
    fn describes_the_target_without_secrets() {
        let postgres = ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "db.internal".to_owned(),
                port: 6432.into(),
                database: "app".to_owned(),
                user: "juan".to_owned(),
                tls: TlsMode::Require,
                ssh: None,
                password_command: None,
            },
        });
        let sqlite = ConnectionConfig::Sqlite(SqliteConfig {
            path: "/data/app.db".into(),
            read_only: true,
        });

        assert_eq!(postgres.to_string(), "juan@db.internal:6432/app");
        assert_eq!(sqlite.to_string(), "/data/app.db (read-only)");
    }

    #[test]
    fn safety_is_optional_written_only_when_not_normal() {
        let mut file: File = toml::from_str(FILE_FROM_0_2_0).unwrap();
        assert!(file.data_source.iter().all(|ds| ds.safety.is_normal()));
        assert!(!toml::to_string(&file).unwrap().contains("safety"));

        file.data_source[0].safety = SafetyLevel::ReadOnly;
        file.data_source[1].safety = SafetyLevel::Careful;
        let text = toml::to_string(&file).unwrap();

        assert!(text.contains(r#"safety = "read-only""#), "{text}");
        assert!(text.contains(r#"safety = "careful""#), "{text}");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);
    }

    #[test]
    fn resolves_placeholders_in_every_setting_that_may_hold_one() {
        let text = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "shared"
            driver = "postgres"
            host = "${PGHOST}"
            port = "${PGPORT}"
            database = "app_${ENV}"
            user = "${PGUSER}"

            [data_source.tls]
            mode = "verify-full"
            ca_file = "~/certs/${ENV}.pem"

            [[data_source]]
            id = "0d6c2f4e-1b8a-4c3d-9e7f-5a6b7c8d9e0f"
            name = "local"
            driver = "sqlite"
            path = "${DATA}/shop.db"
        "#;
        let variables = Fixed::new(&[
            ("PGHOST", "db.internal"),
            ("PGPORT", "6432"),
            ("ENV", "staging"),
            ("PGUSER", "juan"),
            ("DATA", "/srv"),
        ]);

        let file: File = toml::from_str(text).unwrap();
        let resolved: Vec<ConnectionConfig> = file
            .data_source
            .iter()
            .map(|ds| ds.config.resolve(&variables).unwrap())
            .collect();

        let ConnectionConfig::Postgres(postgres) = &resolved[0] else {
            panic!("expected a PostgreSQL data source");
        };
        assert_eq!(
            postgres.server.to_string(),
            "juan@db.internal:6432/app_staging"
        );
        assert_eq!(postgres.server.port.number(), Some(6432));
        assert_eq!(
            postgres.server.tls,
            TlsMode::VerifyFull {
                ca_file: Some("/home/juan/certs/staging.pem".into())
            }
        );
        let ConnectionConfig::Sqlite(sqlite) = &resolved[1] else {
            panic!("expected a SQLite data source");
        };
        assert_eq!(sqlite.path, PathBuf::from("/srv/shop.db"));
        let written = toml::to_string(&file).unwrap();
        assert!(written.contains(r#"port = "${PGPORT}""#), "{written}");
        assert_eq!(toml::from_str::<File>(&written).unwrap(), file);
    }

    #[test]
    fn writes_ssh_as_a_table_and_resolves_its_placeholders() {
        let text = r#"
            [[data_source]]
            id = "4b5f9a3e-8a52-4a7e-9c6b-1f0f2b0c9d11"
            name = "behind a bastion"
            driver = "postgres"
            host = "10.0.0.5"
            port = 5432
            database = "app"
            user = "juan"

            [data_source.ssh]
            host = "${BASTION}"
            port = "${BASTION_PORT}"
            user = "deploy"
            login = "key-file"
            key_file = "~/.ssh/id_ed25519"

            [[data_source]]
            id = "0d6c2f4e-1b8a-4c3d-9e7f-5a6b7c8d9e0f"
            name = "through the agent"
            driver = "mysql"
            host = "db"
            port = 3306
            database = "shop"
            user = "root"
            ssh = { host = "jump", port = 22, user = "juan", login = "agent" }
        "#;
        let variables = Fixed::new(&[("BASTION", "bastion.example.com"), ("BASTION_PORT", "2222")]);

        let file: File = toml::from_str(text).unwrap();
        let ConnectionConfig::Postgres(postgres) =
            file.data_source[0].config.resolve(&variables).unwrap()
        else {
            panic!("expected a PostgreSQL data source");
        };
        let ConnectionConfig::Mysql(mysql) = &file.data_source[1].config else {
            panic!("expected a MySQL data source");
        };

        assert_eq!(
            postgres.server.ssh,
            Some(SshConfig {
                host: "bastion.example.com".to_owned(),
                port: 2222.into(),
                user: "deploy".to_owned(),
                login: SshLogin::KeyFile {
                    key_file: "/home/juan/.ssh/id_ed25519".into()
                },
            })
        );
        assert_eq!(
            postgres.server.to_string(),
            "juan@10.0.0.5:5432/app via deploy@bastion.example.com:2222"
        );
        assert_eq!(
            mysql.server.ssh.as_ref().map(|ssh| &ssh.login),
            Some(&SshLogin::Agent)
        );
        let written = toml::to_string(&file).unwrap();
        assert!(written.contains(r#"login = "key-file""#), "{written}");
        assert_eq!(toml::from_str::<File>(&written).unwrap(), file);
    }

    #[test]
    fn names_the_ssh_setting_an_unset_variable_is_in() {
        let config = ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "db".to_owned(),
                port: 5432.into(),
                database: "app".to_owned(),
                user: "juan".to_owned(),
                tls: TlsMode::Prefer,
                ssh: Some(SshConfig {
                    host: "jump".to_owned(),
                    port: Port::Placeholder("${SSH_PORT}".to_owned()),
                    user: "juan".to_owned(),
                    login: SshLogin::Password,
                }),
                password_command: None,
            },
        });

        let error = config.resolve(&Fixed::new(&[])).unwrap_err();

        assert_eq!(
            crate::describe_error(&error),
            "could not resolve the SSH port: ${SSH_PORT} is not set"
        );
    }

    #[test]
    fn names_the_setting_an_unset_or_wrong_variable_is_in() {
        let config = |host: &str, port: Port| {
            ConnectionConfig::Mysql(MysqlConfig {
                server: ServerConfig {
                    host: host.to_owned(),
                    port,
                    database: "shop".to_owned(),
                    user: "root".to_owned(),
                    tls: TlsMode::Prefer,
                    ssh: None,
                    password_command: None,
                },
            })
        };
        let variables = Fixed::new(&[("WORD", "abc")]);
        let cases = [
            (
                config("${DBHOST}", 3306.into()),
                "could not resolve the host: ${DBHOST} is not set",
            ),
            (
                config("localhost", Port::Placeholder("${WORD}".to_owned())),
                "the port resolves to \"abc\", which is not a port number from 1 to 65535",
            ),
        ];
        for (config, expected) in cases {
            let error = config.resolve(&variables).unwrap_err();
            assert_eq!(crate::describe_error(&error), expected);
        }
    }
}
