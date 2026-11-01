//! Opening connections: resolving the secrets, opening the SSH tunnel if there is one, then
//! dispatching to the right driver. The database password is typed, run for (W9's password
//! command) or looked up.

use std::sync::Arc;
use std::time::Duration;

use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DriverError, Password, PasswordCommand,
    PlaceholderError, Route, ServerConfig, ServerInfo, SshConfig, SshLogin, describe_error,
};
use dbrover_ssh::{HostKey, SshError, Tunnel, TunnelRequest};
use dbrover_store::{Secret, SecretError, SecretStore};
use tokio::task::JoinError;
use tokio::time::timeout;

use crate::environment::ProcessEnvironment;
use crate::events::EventSink;
use crate::password_commands::{PasswordCommandError, PasswordCommands};
use crate::passwords::{self, PasswordNeeded, Prompting};
use crate::read_only;
use crate::tunnels::{Tunnelled, Tunnels};
use crate::{DatabaseFile, Event, PasswordUpdate};

/// Gives up on a connection attempt after this long. Long enough for a slow VPN or a cold
/// cloud database to answer, short enough that a wrong host doesn't leave the UI showing
/// "connecting" for minutes (TCP's own timeout can exceed two minutes).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("could not read the saved password")]
    Password(#[source] SecretError),
    #[error("{} is needed: {reason}", passwords::describe(*secret))]
    PasswordNeeded {
        secret: Secret,
        reason: PasswordNeeded,
    },
    #[error(transparent)]
    PasswordCommand(#[from] PasswordCommandError),
    #[error(transparent)]
    Placeholder(#[from] PlaceholderError),
    #[error(
        "the MCP login {user:?} has no saved password; set it in the data source's settings in \
         DbRover"
    )]
    NoMcpPassword { user: String },
    #[error("could not open the SSH tunnel")]
    Ssh(#[source] SshError),
    /// The driver failed through a tunnel whose SSH server had refused to forward, which the
    /// driver only sees as a closed connection.
    #[error("{reason}")]
    NotForwarded {
        reason: String,
        #[source]
        error: DriverError,
    },
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("could not connect within {0:?}")]
    TimedOut(Duration),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

impl OpenError {
    /// The SSH server's host key, if connecting stopped because nobody has trusted it yet.
    pub(crate) fn unknown_host_key(&self) -> Option<&HostKey> {
        match self {
            Self::Ssh(SshError::UnknownHostKey(key)) => Some(key),
            _ => None,
        }
    }

    /// The password command, if connecting stopped because nobody has trusted it yet.
    pub(crate) fn untrusted_password_command(&self) -> Option<&PasswordCommand> {
        match self {
            Self::PasswordCommand(PasswordCommandError::NotTrusted(command)) => Some(command),
            _ => None,
        }
    }
}

/// Secrets typed in the data source dialog, which the connection test uses instead of the
/// saved ones.
#[derive(Debug, Clone, Default)]
pub(crate) struct TypedSecrets {
    pub(crate) password: PasswordUpdate,
    pub(crate) ssh: PasswordUpdate,
}

/// What opening a connection needs besides the data source.
#[derive(Clone, Copy)]
pub(crate) struct Connector<'a> {
    pub(crate) secrets: &'a Arc<dyn SecretStore>,
    pub(crate) tunnels: &'a Tunnels,
    pub(crate) commands: &'a PasswordCommands,
    pub(crate) prompting: Prompting<'a>,
}

/// Opens the SSH tunnel first, if there is one, then finds the database password and
/// connects: the SSH server is asked about (its host key, its login) before anything is sent
/// to the database or asked for it. Each network step has [`CONNECT_TIMEOUT`] of its own;
/// looking secrets up isn't timed, since the keychain may be waiting on the user.
pub(crate) async fn open(
    data_source: &DataSource,
    typed: &TypedSecrets,
    connector: Connector<'_>,
) -> Result<Box<dyn Connection>, OpenError> {
    let config = data_source.config.resolve(&ProcessEnvironment)?;
    let ssh = config
        .server()
        .and_then(|server| Some((server, server.ssh.as_ref()?)));
    let tunnel = match ssh {
        Some((server, ssh)) => Some(
            timeout(
                CONNECT_TIMEOUT,
                open_tunnel(data_source.id, server, ssh, typed, connector),
            )
            .await
            .map_err(|_| OpenError::TimedOut(CONNECT_TIMEOUT))??,
        ),
        None => None,
    };
    let password = match data_source.config.driver().uses_password() {
        true => database_password(data_source.id, &config, typed, connector).await?,
        false => None,
    };
    let route = tunnel
        .as_ref()
        .map_or(Route::Direct, |tunnel| Route::Tunnel(tunnel.local_addr()));
    let connected = timeout(CONNECT_TIMEOUT, connect(&config, password.as_ref(), route))
        .await
        .map_err(|_| OpenError::TimedOut(CONNECT_TIMEOUT))?;
    let Some(tunnel) = tunnel else {
        return Ok(connected?);
    };
    let connection = connected.map_err(|error| match tunnel.forward_error() {
        Some(reason) => OpenError::NotForwarded { reason, error },
        None => OpenError::Driver(error),
    })?;
    Ok(Box::new(Tunnelled { connection, tunnel }))
}

/// The password typed in the dialog, or else the one the data source's password command
/// prints, or else the one the session or the keychain holds.
///
/// A typed password wins over the command so the MCP login, whose own password is passed in
/// as typed, signs in with that rather than with a token made for the data source's user.
async fn database_password(
    id: DataSourceId,
    config: &ConnectionConfig,
    typed: &TypedSecrets,
    connector: Connector<'_>,
) -> Result<Option<Password>, OpenError> {
    let command = config
        .server()
        .and_then(|server| server.password_command.as_ref());
    match (command, &typed.password) {
        (Some(command), PasswordUpdate::Keep) => Ok(Some(
            connector.commands.password(id, command, config).await?,
        )),
        (_, typed_password) => {
            passwords::resolve(
                id,
                Secret::Database,
                connector.secrets,
                typed_password,
                connector.prompting,
            )
            .await
        }
    }
}

/// The data source's tunnel to `server`, signing in with the secret its SSH login needs.
///
/// A key file's passphrase is only looked for in the keychain once the key turns out to be
/// encrypted, so an unencrypted key never touches the keychain.
async fn open_tunnel(
    id: DataSourceId,
    server: &ServerConfig,
    ssh: &SshConfig,
    typed: &TypedSecrets,
    connector: Connector<'_>,
) -> Result<Arc<Tunnel>, OpenError> {
    let target_port = server.port_number()?;
    let lease = |secret: Option<Password>| async move {
        connector
            .tunnels
            .lease(
                id,
                TunnelRequest {
                    ssh,
                    secret: secret.as_ref(),
                    target_host: &server.host,
                    target_port,
                    known_hosts: connector.tunnels.known_hosts(),
                },
            )
            .await
    };
    let resolve = || {
        passwords::resolve(
            id,
            Secret::Ssh,
            connector.secrets,
            &typed.ssh,
            connector.prompting,
        )
    };
    let first = match ssh.login {
        SshLogin::Password => resolve().await?,
        SshLogin::KeyFile { .. } => {
            passwords::known(id, Secret::Ssh, &typed.ssh, connector.prompting)
        }
        SshLogin::Agent => None,
    };
    let tried_without = first.is_none();
    match lease(first).await {
        Err(SshError::SecretNeeded(_)) if tried_without => {
            let secret = resolve().await?;
            lease(secret).await.map_err(OpenError::Ssh)
        }
        result => result.map_err(OpenError::Ssh),
    }
}

/// Opens a connection to a saved data source with its saved password, or the one its trusted
/// password command prints, if it needs one, and through its SSH tunnel, if it has one.
///
/// For other front ends than the app's UI, such as `dbrover mcp`, so every way in connects
/// through the same drivers, keychain, password commands, tunnels and timeout.
///
/// # Errors
///
/// [`OpenError`] if a secret can't be read, the password command isn't trusted or fails, the
/// tunnel or the driver fails, or connecting takes longer than the connect timeout.
pub async fn open_data_source(
    data_source: &DataSource,
    secrets: &Arc<dyn SecretStore>,
    tunnels: &Tunnels,
    commands: &PasswordCommands,
) -> Result<Box<dyn Connection>, OpenError> {
    let connector = Connector {
        secrets,
        tunnels,
        commands,
        prompting: Prompting::Never,
    };
    open(data_source, &TypedSecrets::default(), connector).await
}

/// Opens a connection for `dbrover mcp`: as the data source's MCP login when it has one,
/// with that login's own saved password, and otherwise as [`open_data_source`] does.
///
/// An MCP login without a saved password is an error rather than a reason to fall back to
/// the data source's own login, whose grants are what the MCP login exists to narrow.
///
/// # Errors
///
/// [`OpenError`] as for [`open_data_source`], and [`OpenError::NoMcpPassword`].
pub async fn open_data_source_for_mcp(
    data_source: &DataSource,
    secrets: &Arc<dyn SecretStore>,
    tunnels: &Tunnels,
    commands: &PasswordCommands,
) -> Result<Box<dyn Connection>, OpenError> {
    let Some(login) = &data_source.mcp_login else {
        return open_data_source(data_source, secrets, tunnels, commands).await;
    };
    let Some(config) = data_source.config.with_user(&login.user) else {
        return open_data_source(data_source, secrets, tunnels, commands).await;
    };
    let connector = Connector {
        secrets,
        tunnels,
        commands,
        prompting: Prompting::Never,
    };
    // A DynamoDB MCP login is an AWS profile, which needs no password of DbRover's.
    if !config.driver().uses_password() {
        let as_mcp = DataSource {
            config,
            ..data_source.clone()
        };
        return open(&as_mcp, &TypedSecrets::default(), connector).await;
    }
    let id = data_source.id;
    let store = Arc::clone(secrets);
    let password = tokio::task::spawn_blocking(move || store.get(id, Secret::McpLogin))
        .await
        .map_err(OpenError::Task)?
        .map_err(OpenError::Password)?
        .ok_or_else(|| OpenError::NoMcpPassword {
            user: login.user.clone(),
        })?;
    let as_mcp = DataSource {
        config,
        ..data_source.clone()
    };
    let typed = TypedSecrets {
        password: PasswordUpdate::Set(password),
        ssh: PasswordUpdate::Keep,
    };
    open(&as_mcp, &typed, connector).await
}

/// The one place that maps a configuration to its driver.
async fn connect(
    config: &ConnectionConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<Box<dyn Connection>, DriverError> {
    Ok(match config {
        ConnectionConfig::Postgres(config) => {
            Box::new(dbrover_driver_postgres::connect(config, password, route).await?)
        }
        ConnectionConfig::Sqlite(config) => Box::new(dbrover_driver_sqlite::connect(config).await?),
        ConnectionConfig::Mysql(config) => {
            Box::new(dbrover_driver_mysql::connect(config, password, route).await?)
        }
        ConnectionConfig::SqlServer(config) => {
            Box::new(dbrover_driver_sqlserver::connect(config, password, route).await?)
        }
        ConnectionConfig::DuckDb(config) => Box::new(dbrover_driver_duckdb::connect(config).await?),
        ConnectionConfig::ClickHouse(config) => {
            Box::new(dbrover_driver_clickhouse::connect(config, password, route).await?)
        }
        ConnectionConfig::Redis(config) => {
            Box::new(dbrover_driver_redis::connect(config, password, route).await?)
        }
        ConnectionConfig::Mongo(config) => {
            Box::new(dbrover_driver_mongodb::connect(config, password, route).await?)
        }
        ConnectionConfig::Elasticsearch(config) => {
            Box::new(dbrover_driver_elasticsearch::connect(config, password, route).await?)
        }
        ConnectionConfig::Cassandra(config) => {
            Box::new(dbrover_driver_cassandra::connect(config, password, route).await?)
        }
        // AWS's SDK reaches the region's endpoint or the one set, and signs every request:
        // SSH tunnels aren't offered for DynamoDB, so the route is always direct.
        ConnectionConfig::DynamoDb(config) => {
            Box::new(dbrover_driver_dynamodb::connect(config).await?)
        }
    })
}

/// What testing a connection from the dialog needs besides the data source and its secrets.
pub(crate) struct TestServices {
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) tunnels: Tunnels,
    /// Already [`PasswordCommands::shown`]: the dialog shows the command it tests.
    pub(crate) commands: PasswordCommands,
    pub(crate) events: EventSink,
}

/// Connects, pings and disconnects, then reports the outcome as [`Event::ConnectionTested`].
pub(crate) async fn test_connection(
    data_source: DataSource,
    typed: TypedSecrets,
    services: TestServices,
) {
    let TestServices {
        secrets,
        tunnels,
        commands,
        events,
    } = services;
    tracing::info!(data_source = %data_source.name, "testing connection to {}", data_source.config);
    let data_source = read_only::session_config(data_source);
    let connector = Connector {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
        prompting: Prompting::Never,
    };
    let result = open_ping_close(&data_source, &typed, connector).await;
    match &result {
        Ok(server) => tracing::info!(
            data_source = %data_source.name,
            "test connection succeeded: {server}"
        ),
        Err(error) => tracing::warn!(
            data_source = %data_source.name,
            "test connection failed: {}",
            describe_error(error)
        ),
    }
    if let Err(error) = &result
        && let Some(key) = error.unknown_host_key()
    {
        events.send(Event::HostKeyUnknown {
            data_source: data_source.id,
            key: key.clone(),
        });
    }
    events.send(Event::ConnectionTested {
        id: data_source.id,
        result: result.map_err(|error| describe_error(&error)),
    });
}

async fn open_ping_close(
    data_source: &DataSource,
    typed: &TypedSecrets,
    connector: Connector<'_>,
) -> Result<ServerInfo, OpenError> {
    let mut connection = open(data_source, typed, connector).await?;
    connection.ping().await?;
    let server = connection.server_info().clone();
    connection.close().await?;
    Ok(server)
}

/// Creates an empty database file, then reports it as [`Event::DatabaseFileCreated`].
pub(crate) async fn create_database_file(file: DatabaseFile, events: EventSink) {
    let result = match &file {
        DatabaseFile::Sqlite(path) => dbrover_driver_sqlite::create_database(path).await,
        DatabaseFile::DuckDb(path) => dbrover_driver_duckdb::create_database(path).await,
    };
    let engine = file.driver().display_name();
    let path = file.path().display();
    match &result {
        Ok(()) => tracing::info!("created {engine} database {path}"),
        Err(error) => tracing::warn!(
            "could not create {engine} database {path}: {}",
            describe_error(error)
        ),
    }
    events.send(Event::DatabaseFileCreated {
        file,
        result: result.map_err(|error| describe_error(&error)),
    });
}
