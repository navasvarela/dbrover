//! How DbRover refuses changes: for the MCP server's read-only projects, and for the consoles
//! of read-only data sources. One implementation, so both refuse the same statements.
//!
//! Changes are refused twice:
//! 1. [`check`] classifies each statement, in its data source's language, before it is sent,
//!    and refuses anything that doesn't only read, or can't be classified;
//! 2. the database refuses what the check can't see, such as `SELECT writes_something()`:
//!    the statement runs in the engine's own read-only mode ([`mode`]), a read-only transaction
//!    or a connection opened read-only; or, on an engine without one, the login's grants do
//!    ([`Guard::Login`]). Each engine's guard, and why, is in `docs/well-rounded.md` (W11).

use std::time::Duration;

use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DriverError, DriverKind, DuckDbConfig,
    DuckDbDatabase, SafetyLevel,
};
use dbrover_language::{QueryLanguage, StatementKind, Unclassified};

/// How an engine refuses writes on its own, whatever the statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyMode {
    /// A transaction opened with `begin` around the work, and rolled back after it.
    Transaction {
        begin: &'static str,
        /// Whether the engine can also be told a time limit for the transaction, which ends
        /// a runaway statement even if a cancel can't get through.
        sets_timeout: bool,
    },
    /// The connection itself is opened read-only, for its whole life: a SQLite or DuckDB file,
    /// or a ClickHouse session whose every request carries the `readonly` setting. See
    /// [`connection_config`].
    Connection,
    /// A writable SQLite connection told to refuse writes with `PRAGMA query_only`, for one
    /// that also runs changes ([`crate::read_write`]), as the MCP server's does when a project
    /// allows them. Weaker than [`Self::Connection`]: a statement could switch it off, so
    /// [`check`] must have refused anything but reading first (`PRAGMA` is administration).
    QueryOnly,
}

/// Where a data source's database is kept, which decides whether DuckDB has a read-only mode
/// for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// On a server, or in a file.
    Kept,
    /// In the connection's memory, gone when it disconnects.
    InMemory,
}

impl Storage {
    pub fn of(config: &ConnectionConfig) -> Self {
        match config {
            ConnectionConfig::DuckDb(DuckDbConfig {
                database: DuckDbDatabase::InMemory,
            }) => Self::InMemory,
            _ => Self::Kept,
        }
    }
}

/// The engine's read-only mode for a database kept in `storage`, or `None` when it has none
/// DbRover can use.
pub fn mode(driver: DriverKind, storage: Storage) -> Option<ReadOnlyMode> {
    match (driver, storage) {
        (DriverKind::Postgres, _) => Some(ReadOnlyMode::Transaction {
            begin: "BEGIN READ ONLY",
            sets_timeout: true,
        }),
        (DriverKind::Mysql, _) => Some(ReadOnlyMode::Transaction {
            begin: "START TRANSACTION READ ONLY",
            sets_timeout: false,
        }),
        (DriverKind::Sqlite | DriverKind::ClickHouse, _) | (DriverKind::DuckDb, Storage::Kept) => {
            Some(ReadOnlyMode::Connection)
        }
        // DuckDB refuses to open a database in memory read-only. SQL Server has no read-only
        // transaction, and Redis, MongoDB, Elasticsearch, Cassandra and DynamoDB no read-only
        // session at all: a user who may only read is their guard (docs/nosql.md, N0a to N5).
        (DriverKind::DuckDb, Storage::InMemory)
        | (
            DriverKind::SqlServer
            | DriverKind::Redis
            | DriverKind::Mongo
            | DriverKind::Elasticsearch
            | DriverKind::Cassandra
            | DriverKind::DynamoDb,
            _,
        ) => None,
    }
}

/// What the database itself does to refuse a write that [`check`] can't see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// The engine's own read-only mode.
    Session(ReadOnlyMode),
    /// The engine has no read-only mode, but has logins: only one whose grants allow nothing
    /// but reading makes the database refuse writes. The MCP server serves such a data source
    /// only through its MCP login (`docs/nosql.md`, N0a).
    Login,
    /// Neither, as for a DuckDB database in memory: the check is all there is, so MCP clients
    /// aren't served.
    CheckOnly,
}

/// The database's own guard on `driver`'s data sources kept in `storage`.
pub fn guard(driver: DriverKind, storage: Storage) -> Guard {
    match mode(driver, storage) {
        Some(mode) => Guard::Session(mode),
        None if driver.has_logins() => Guard::Login,
        None => Guard::CheckOnly,
    }
}

/// The login that makes a [`Guard::Login`] engine refuse writes, as the read-only level's
/// advice names it.
pub fn read_only_login(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Redis => "an ACL user with only +@read",
        DriverKind::Mongo => "a user with only the read role",
        DriverKind::Elasticsearch => {
            "a user or API key with only the monitor cluster privilege and read, \
             view_index_metadata and monitor on its indices"
        }
        DriverKind::Cassandra => "a role with only the SELECT permission",
        DriverKind::DynamoDb => {
            "a profile whose IAM policy allows only dynamodb:PartiQLSelect, \
             dynamodb:DescribeTable and dynamodb:ListTables"
        }
        DriverKind::Postgres
        | DriverKind::Sqlite
        | DriverKind::Mysql
        | DriverKind::SqlServer
        | DriverKind::DuckDb
        | DriverKind::ClickHouse => "a user with read-only grants",
    }
}

/// Whether the MCP server serves a data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServing {
    /// Through the engine's read-only mode, or through the data source's MCP login.
    Served,
    /// The engine has no read-only mode: the data source is served once it has an MCP login
    /// whose grants allow only reading.
    NeedsMcpLogin,
    /// The database has no read-only mode, and the engine no logins.
    NotServed,
}

impl McpServing {
    pub fn of(data_source: &DataSource) -> Self {
        let config = &data_source.config;
        match guard(config.driver(), Storage::of(config)) {
            Guard::Session(_) => Self::Served,
            Guard::Login if data_source.mcp_login.is_some() => Self::Served,
            Guard::Login => Self::NeedsMcpLogin,
            Guard::CheckOnly => Self::NotServed,
        }
    }

    /// Why the data source isn't served, for the log and the project's settings.
    pub fn reason(self, driver: DriverKind) -> Option<String> {
        let engine = driver.display_name();
        match self {
            Self::Served => None,
            Self::NeedsMcpLogin => Some(format!(
                "{engine} has no read-only mode, so it is served only through an MCP login \
                 whose grants allow nothing but reading"
            )),
            Self::NotServed => Some(format!(
                "{engine} can't open this database read-only, and has no logins to read as"
            )),
        }
    }
}

/// The read-only mode on a connection that also runs changes, or `None` where a connection
/// must be opened read-only to refuse them. A read-only transaction works on any connection;
/// a SQLite file opened to run changes is told [`ReadOnlyMode::QueryOnly`] instead. ClickHouse
/// and DuckDB have no such switch, and no transaction DbRover runs changes in, so a connection
/// that runs them is never opened there.
pub fn mode_on_writable_connection(driver: DriverKind) -> Option<ReadOnlyMode> {
    match (mode(driver, Storage::Kept)?, driver) {
        (ReadOnlyMode::Connection, DriverKind::Sqlite) => Some(ReadOnlyMode::QueryOnly),
        (ReadOnlyMode::Connection, _) => None,
        (other, _) => Some(other),
    }
}

/// The settings to connect with: a SQLite or DuckDB file is opened read-only, and a ClickHouse
/// session sends the `readonly` setting with every request, each the engine's own equivalent
/// of a read-only transaction. Other engines connect as usual.
pub fn connection_config(mut data_source: DataSource) -> DataSource {
    match &mut data_source.config {
        ConnectionConfig::Sqlite(config) => config.read_only = true,
        ConnectionConfig::ClickHouse(config) => config.read_only = true,
        ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::File { read_only, .. },
        }) => *read_only = true,
        _ => {}
    }
    data_source
}

/// The settings a data source's own sessions connect with, and its connection is tested
/// with: a read-only one's as [`connection_config`] says, so a test connects as they will.
pub fn session_config(data_source: DataSource) -> DataSource {
    match data_source.safety {
        SafetyLevel::ReadOnly => connection_config(data_source),
        SafetyLevel::Normal | SafetyLevel::Careful => data_source,
    }
}

/// Why [`check`] refused a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The statement changes something.
    Changes(StatementKind),
    /// What the statement does can't be known.
    Unknown(Unclassified),
}

impl Refusal {
    /// What the statement does, as in "this statement changes data", or why that is unknown.
    pub fn reason(&self) -> String {
        match self {
            Self::Changes(kind) => format!("this statement {}", kind.description()),
            Self::Unknown(unclassified) => unclassified.to_string(),
        }
    }
}

/// Accepts `sql`, one statement, only if it only reads.
///
/// # Errors
///
/// [`Refusal`] for a statement that changes anything, controls the transaction, or can't be
/// classified.
pub fn check(sql: &str, language: QueryLanguage) -> Result<(), Refusal> {
    match language.classify(sql) {
        Ok(StatementKind::Read) => Ok(()),
        Ok(kind) => Err(Refusal::Changes(kind)),
        Err(unclassified) => Err(Refusal::Unknown(unclassified)),
    }
}

/// Opens the read-only transaction, if the engine uses one, with `timeout` as its time limit
/// where the engine can be told one.
///
/// # Errors
///
/// [`DriverError`] if the database refuses to open it.
pub async fn begin(
    connection: &mut dyn Connection,
    mode: ReadOnlyMode,
    timeout: Option<Duration>,
) -> Result<(), DriverError> {
    let (begin, sets_timeout) = match mode {
        ReadOnlyMode::Transaction {
            begin,
            sets_timeout,
        } => (begin, sets_timeout),
        ReadOnlyMode::QueryOnly => return run_control(connection, "PRAGMA query_only = ON").await,
        ReadOnlyMode::Connection => return Ok(()),
    };
    run_control(connection, begin).await?;
    if let Some(timeout) = timeout.filter(|_| sets_timeout) {
        let set = format!("SET LOCAL statement_timeout = {}", timeout.as_millis());
        run_control(connection, &set).await?;
    }
    Ok(())
}

/// Rolls back what [`begin`] opened.
///
/// # Errors
///
/// [`DriverError`] if the rollback fails, which leaves the connection in an unknown state.
pub async fn rollback(
    connection: &mut dyn Connection,
    mode: ReadOnlyMode,
) -> Result<(), DriverError> {
    match mode {
        ReadOnlyMode::Transaction { .. } => run_control(connection, "ROLLBACK").await,
        ReadOnlyMode::Connection | ReadOnlyMode::QueryOnly => Ok(()),
    }
}

pub(crate) async fn run_control(
    connection: &mut dyn Connection,
    sql: &str,
) -> Result<(), DriverError> {
    connection.execute(sql).await?.finish().await.map(drop)
}

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    #[test]
    fn refuses_whatever_does_not_only_read() {
        let postgres = QueryLanguage::Sql(Dialect::Postgres);
        let cases = [
            ("SELECT * FROM orders", None),
            (
                "INSERT INTO orders VALUES (1)",
                Some(Refusal::Changes(StatementKind::Write)),
            ),
            (
                "CREATE TABLE t (id int)",
                Some(Refusal::Changes(StatementKind::Schema)),
            ),
            ("BEGIN", Some(Refusal::Changes(StatementKind::Transaction))),
            (
                "SET search_path TO x",
                Some(Refusal::Changes(StatementKind::Admin)),
            ),
        ];
        for (sql, expected) in cases {
            assert_eq!(check(sql, postgres).err(), expected, "{sql}");
        }
        assert!(matches!(
            check("SELEC nonsense", postgres),
            Err(Refusal::Unknown(_))
        ));
        assert_eq!(
            Refusal::Changes(StatementKind::Write).reason(),
            "this statement changes data"
        );
    }

    /// What a Redis or MongoDB console will go through: the language's own classification,
    /// with anything it can't classify refused.
    #[test]
    fn refuses_whatever_does_not_only_read_in_every_language() {
        let cases = [
            ("get user:1", None),
            ("set user:1 x", Some(Refusal::Changes(StatementKind::Write))),
            ("drop index", Some(Refusal::Changes(StatementKind::Schema))),
            ("begin", Some(Refusal::Changes(StatementKind::Transaction))),
            ("flush", Some(Refusal::Changes(StatementKind::Admin))),
        ];
        for (command, expected) in cases {
            assert_eq!(
                check(command, QueryLanguage::Test).err(),
                expected,
                "{command}"
            );
        }
        assert!(matches!(
            check("shutdown", QueryLanguage::Test),
            Err(Refusal::Unknown(_))
        ));
    }

    #[test]
    fn guards_each_engine_with_its_read_only_mode_or_a_login() {
        let guards = DriverKind::ALL.map(|driver| (driver, guard(driver, Storage::Kept)));
        for (driver, guard) in guards {
            let expected = match driver {
                DriverKind::Postgres
                | DriverKind::Mysql
                | DriverKind::Sqlite
                | DriverKind::ClickHouse
                | DriverKind::DuckDb => matches!(guard, Guard::Session(_)),
                DriverKind::SqlServer
                | DriverKind::Redis
                | DriverKind::Mongo
                | DriverKind::Elasticsearch
                | DriverKind::Cassandra
                | DriverKind::DynamoDb => guard == Guard::Login,
            };
            assert!(expected, "{driver:?}: {guard:?}");
        }
        assert_eq!(
            guard(DriverKind::DuckDb, Storage::InMemory),
            Guard::CheckOnly
        );
        assert_eq!(
            read_only_login(DriverKind::Redis),
            "an ACL user with only +@read"
        );
    }

    /// Only SQLite can be told to refuse writes on a connection opened to run them; on
    /// ClickHouse, refusing them means opening the connection read-only.
    #[test]
    fn refuses_writes_on_a_writable_connection_only_where_the_engine_can_be_told_to() {
        let cases = [
            (DriverKind::Sqlite, Some(ReadOnlyMode::QueryOnly)),
            (DriverKind::ClickHouse, None),
            (
                DriverKind::Postgres,
                mode(DriverKind::Postgres, Storage::Kept),
            ),
            (DriverKind::DuckDb, None),
            (DriverKind::SqlServer, None),
        ];
        for (driver, expected) in cases {
            assert_eq!(mode_on_writable_connection(driver), expected, "{driver:?}");
        }
    }
}
