//! Redis, Valkey and Dragonfly driver for DbRover, on the community `redis` client
//! (`docs/nosql.md`, N1).
//!
//! A connection is one session with the server, opened on a stream DbRover makes (TCP, an SSH
//! tunnel, TLS from `dbrover-tls`) and spoken to in RESP3, whose maps, sets and doubles tell
//! the grid a reply's shape. Each command is sent as typed, its arguments read as `redis-cli`
//! reads them (`dbrover_language::redis`). Redis has no transactions to roll back (`MULTI`
//! only queues commands), so a connection is never inside one.

mod catalog;
mod errors;
mod execution;
mod keys;
mod ranges;
mod reply;
mod stream;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    BoxError, CancelHandle, CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, Connection,
    DriverError, Password, RedisConfig, Rejection, RejectionKind, RelationName, ResultCursor,
    Route, ServerConfig, ServerInfo, TableConstraints, TableDescription, TableName,
    TransactionStatus,
};
use dbrover_language::redis as language;
use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::{AsyncConnectionConfig, Cmd, ProtocolVersion, RedisConnectionInfo, Value};
use tokio::task::JoinHandle;

use crate::execution::{RedisCancel, ReplyCursor, Scan, ScanCursor, Shared, Stopping};
use crate::keys::{Level, Listings};
use crate::ranges::{Range, RangeCursor};

/// Opens a session with the server in `config`, through `route`.
///
/// # Errors
///
/// [`DriverError::Connect`] if the database number isn't one, the server is unreachable, the
/// TLS handshake or certificate check fails, the server refuses the user or password, or the
/// database doesn't exist.
pub async fn connect(
    config: &RedisConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<RedisConnection, DriverError> {
    let database = database_number(&config.server.database).map_err(errors::connect)?;
    let login = Login {
        server: config.server.clone(),
        password: password.cloned(),
        route,
        separator: config.separator.clone(),
    };
    let mut session = Session::open(&login, database)
        .await
        .map_err(errors::connect)?;
    // `INFO` is in the `dangerous` category, which a user with only `+@read`, as an MCP login
    // should be, may not run: the server is then named without its version.
    let info = match session.query(Cmd::new().arg("INFO").arg("server")).await {
        Ok(info) => info,
        Err(error) if error.is::<errors::ServerFailure>() => Value::Nil,
        Err(error) => return Err(errors::connect(error)),
    };
    Ok(RedisConnection {
        session,
        shared: Arc::new(Shared::new(login)),
        database,
        listings: Listings::default(),
        server_info: server_info(&reply::text(&info).unwrap_or_default()),
    })
}

/// The database number a data source names, `0` when it names none.
fn database_number(text: &str) -> Result<i64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    text.parse::<i64>()
        .ok()
        .filter(|number| *number >= 0)
        .ok_or_else(|| {
            format!("the database must be a number such as 0, not \u{201c}{text}\u{201d}")
        })
}

/// Which server answered, from `INFO server`: Valkey and Dragonfly also report the Redis
/// version they are compatible with, so their own lines are looked for first.
fn server_info(info: &str) -> ServerInfo {
    let field = |name: &str| {
        info.lines()
            .find_map(|line| line.trim().strip_prefix(name)?.strip_prefix(':'))
            .map(str::to_owned)
    };
    let (product, version) = if let Some(version) = field("dragonfly_version") {
        ("Dragonfly", Some(version))
    } else if let Some(version) = field("valkey_version") {
        ("Valkey", Some(version))
    } else {
        ("Redis", field("redis_version"))
    };
    ServerInfo::new(product, version.unwrap_or_else(|| "unknown".to_owned()))
}

/// What it takes to open a session: kept to open another after a cancel that had to stop
/// waiting, and for the second session that sends `CLIENT UNBLOCK` or `SCRIPT KILL`.
pub(crate) struct Login {
    server: ServerConfig,
    password: Option<Password>,
    route: Route,
    /// What the explorer groups keys by.
    separator: String,
}

/// One connection to the server.
pub(crate) struct Session {
    pub(crate) connection: MultiplexedConnection,
    /// The server's id for this session, for `CLIENT UNBLOCK`; `None` when the login may not
    /// ask for it, as a user with only `+@read` may not.
    pub(crate) client: Option<i64>,
    /// Reads the server's replies for `connection`, until the session ends.
    reader: JoinHandle<()>,
}

impl Session {
    pub(crate) async fn open(login: &Login, database: i64) -> Result<Self, BoxError> {
        let stream = stream::open(&login.server, login.route).await?;
        let mut info = RedisConnectionInfo::default()
            .set_db(database)
            .set_protocol(ProtocolVersion::RESP3)
            .set_lib_name("dbrover", env!("CARGO_PKG_VERSION"));
        if !login.server.user.is_empty() {
            info = info.set_username(&login.server.user);
        }
        if let Some(password) = &login.password {
            info = info.set_password(password.expose());
        }
        // The client's own time limits would fail a `BLPOP` that waits longer than half a
        // second; how long a command may take is the user's to decide, and ⌘. to end.
        let config = AsyncConnectionConfig::new()
            .set_response_timeout(None)
            .set_connection_timeout(None);
        let (connection, reader) =
            MultiplexedConnection::new_with_config(&info, stream, config).await?;
        let mut session = Self {
            connection,
            client: None,
            reader: tokio::spawn(reader),
        };
        session.client = match session.query(Cmd::new().arg("CLIENT").arg("ID")).await {
            Ok(Value::Int(id)) => Some(id),
            _ => None,
        };
        Ok(session)
    }

    /// Sends `command` and returns its reply, an error the server answered with included.
    async fn query(&mut self, command: &Cmd) -> Result<Value, BoxError> {
        match self.connection.req_packed_command(command).await? {
            Value::ServerError(error) => {
                Err(errors::ServerFailure(reply::error_text(&error)).into())
            }
            value => Ok(value),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// A session with a Redis, Valkey or Dragonfly server.
pub struct RedisConnection {
    session: Session,
    shared: Arc<Shared>,
    /// The database the session uses: the data source's, or the last one `SELECT` chose,
    /// which a new session selects again.
    database: i64,
    listings: Listings,
    server_info: ServerInfo,
}

impl RedisConnection {
    /// The databases that hold keys, from `INFO keyspace`; for a user who may not run `INFO`,
    /// as one with only `+@read` may not, the data source's own, counted with `DBSIZE`.
    async fn databases(&mut self) -> Result<Vec<CatalogNode>, DriverError> {
        let own = database_number(&self.shared.login().server.database).unwrap_or_default();
        let mut command = Cmd::new();
        command.arg("INFO").arg("keyspace");
        let info = match self.catalog_query(&command).await {
            Ok(info) => reply::text(&info).unwrap_or_default(),
            Err(DriverError::Connection(error)) if error.is::<errors::ServerFailure>() => {
                self.select(own).await?;
                let mut command = Cmd::new();
                command.arg("DBSIZE");
                let keys = match self.catalog_query(&command).await? {
                    Value::Int(keys) => keys,
                    _ => 0,
                };
                format!("db{own}:keys={keys}")
            }
            Err(error) => return Err(error),
        };
        Ok(catalog::databases(&info, own))
    }

    /// The session, replaced first if a cancel left it waiting for a reply nobody reads.
    async fn session(&mut self) -> Result<&mut Session, DriverError> {
        if self.shared.take_stale() {
            self.session = Session::open(self.shared.login(), self.database)
                .await
                .map_err(DriverError::Lost)?;
        }
        Ok(&mut self.session)
    }

    /// Runs one command, as arguments, and returns its reply.
    async fn run(&mut self, arguments: &[Vec<u8>]) -> Result<Value, DriverError> {
        let named = language::command(arguments);
        let name = named
            .as_ref()
            .map_or_else(String::new, |(name, _)| name.as_str().to_owned());
        let top = name.split('|').next().unwrap_or_default().to_owned();
        let stopping = Stopping::of(&top, named.map(|(_, spec)| spec));
        let mut command = Cmd::new();
        for argument in arguments {
            command.arg(&argument[..]);
        }
        let shared = Arc::clone(&self.shared);
        let session = self.session().await?;
        let client = session.client;
        let reply = shared
            .run(&mut session.connection, client, stopping, &command)
            .await?;
        if top == "select"
            && let Some(database) = arguments
                .get(1)
                .and_then(|number| std::str::from_utf8(number).ok()?.parse().ok())
        {
            self.database = database;
        }
        Ok(reply)
    }
}

#[async_trait]
impl Connection for RedisConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Any answer proves the connection works, a refusal included: a user with only `+@read`
    /// may not run `PING`.
    async fn ping(&mut self) -> Result<(), DriverError> {
        match self.run(&[b"PING".to_vec()]).await {
            Ok(_) | Err(DriverError::Statement { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let whole = ChildrenQuery {
            filter: None,
            after: None,
            limit: None,
        };
        Ok(self.children_page(path, &whole).await?.nodes)
    }

    /// The databases whole, and the keys of a database or a group a page at a time.
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        if path.is_root() {
            return Ok(ChildrenPage::whole(self.databases().await?, query));
        }
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        self.key_page(path, &level, query).await
    }

    /// Keys and databases have no source to show.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        Err(DriverError::InvalidPath(path.clone()))
    }

    async fn execute(&mut self, command: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let arguments = language::arguments(command).map_err(errors::statement)?;
        let Some(name) = arguments.first() else {
            return Err(errors::statement("there is no command"));
        };
        let name = String::from_utf8_lossy(name).to_ascii_lowercase();
        if let Some(scan) = Scan::of(&arguments) {
            let shared = Arc::clone(&self.shared);
            let session = self.session().await?;
            let cursor = ScanCursor::start(scan, arguments, session, shared).await?;
            return Ok(Box::new(cursor));
        }
        if let Some((key, range)) = Range::of(&arguments) {
            let shared = Arc::clone(&self.shared);
            let session = self.session().await?;
            let cursor = RangeCursor::start(key, range, session, shared).await?;
            return Ok(Box::new(cursor));
        }
        let reply = self.run(&arguments).await?;
        Ok(Box::new(ReplyCursor::new(reply::table(&name, reply))))
    }

    /// Redis has no tables, so no rows to edit through one.
    async fn describe_table(
        &mut self,
        _table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        Ok(None)
    }

    async fn describe_constraints(
        &mut self,
        _table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        Ok(TableConstraints::default())
    }

    /// Checks the command's name and argument count against the command table, without
    /// sending anything: Redis has no way to prepare a command without running it.
    async fn validate(&mut self, command: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(rejection(command))
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(RedisCancel(Arc::clone(&self.shared)))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Why the server would refuse `command` before running it, as far as the command table can
/// tell: an unknown name, or the wrong number of arguments. A module's command the table
/// doesn't list is let through, since the server may have the module.
fn rejection(command: &str) -> Option<Rejection> {
    let arguments = match language::arguments(command) {
        Ok(arguments) => arguments,
        Err(error) => {
            return Some(Rejection {
                message: error.to_string(),
                position: None,
                kind: RejectionKind::Syntax,
            });
        }
    };
    let (name, spec) = language::command(&arguments)?;
    (!spec.accepts(arguments.len())).then(|| Rejection {
        message: format!("wrong number of arguments for '{name}' command"),
        position: None,
        kind: RejectionKind::Other,
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn tells_redis_valkey_and_dragonfly_apart() {
        let redis = "# Server\r\nredis_version:8.2.1\r\nredis_mode:standalone\r\n";
        let valkey =
            "# Server\r\nredis_version:7.2.4\r\nserver_name:valkey\r\nvalkey_version:9.0.0\r\n";
        let dragonfly = "# Server\r\nredis_version:7.4.0\r\ndragonfly_version:df-v1.34.1\r\n";
        assert_eq!(server_info(redis).to_string(), "Redis 8.2.1");
        assert_eq!(server_info(valkey).to_string(), "Valkey 9.0.0");
        assert_eq!(server_info(dragonfly).to_string(), "Dragonfly df-v1.34.1");
        assert_eq!(server_info("").to_string(), "Redis unknown");
    }

    #[test]
    fn reads_the_database_number() {
        assert_eq!(database_number(""), Ok(0));
        assert_eq!(database_number(" 3 "), Ok(3));
        assert!(database_number("app").is_err());
        assert!(database_number("-1").is_err());
    }

    #[test]
    fn validates_names_and_argument_counts_without_a_server() {
        assert_eq!(rejection("GET user:42"), None);
        assert_eq!(rejection("HSET h f v g w"), None);
        assert_eq!(rejection("JSON.GET doc $"), None);
        let wrong = rejection("GET").map(|rejection| rejection.message);
        assert_eq!(
            wrong.as_deref(),
            Some("wrong number of arguments for 'GET' command")
        );
        let unclosed = rejection("GET \"open").map(|rejection| rejection.kind);
        assert_eq!(unclosed, Some(RejectionKind::Syntax));
    }
}
