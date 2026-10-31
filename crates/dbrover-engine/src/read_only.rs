//! How DbRover refuses changes: for the MCP server's read-only projects, and for the consoles
//! of read-only data sources. One implementation, so both refuse the same statements.
//!
//! Changes are refused twice:
//! 1. [`check`] classifies each statement, in its data source's language, before it is sent,
//!    and refuses anything that doesn't only read, or can't be classified;
//! 2. the database refuses what the check can't see, such as `SELECT writes_something()`:
//!    the statement runs in the engine's own read-only mode ([`mode`]), a read-only transaction
//!    or a read-only SQLite connection; or, on an engine without one, the login's grants do
//!    ([`Guard::Login`]).

use std::time::Duration;

use dbrover_core::{Connection, ConnectionConfig, DataSource, DriverError, DriverKind};
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
    /// The connection itself is opened read-only; see [`connection_config`].
    Connection,
    /// A writable SQLite connection told to refuse writes with `PRAGMA query_only`, for one
    /// that also runs changes ([`crate::read_write`]), as the MCP server's does when a project
    /// allows them. Weaker than [`Self::Connection`]: a statement could switch it off, so
    /// [`check`] must have refused anything but reading first (`PRAGMA` is administration).
    QueryOnly,
}

/// The engine's read-only mode, or `None` when it has none DbRover can use.
pub fn mode(driver: DriverKind) -> Option<ReadOnlyMode> {
    match driver {
        DriverKind::Postgres => Some(ReadOnlyMode::Transaction {
            begin: "BEGIN READ ONLY",
            sets_timeout: true,
        }),
        DriverKind::Mysql => Some(ReadOnlyMode::Transaction {
            begin: "START TRANSACTION READ ONLY",
            sets_timeout: false,
        }),
        DriverKind::Sqlite => Some(ReadOnlyMode::Connection),
        // SQL Server has no read-only transaction, DuckDB's read-only mode is a choice of the
        // data source, not of the connection MCP would open, and ClickHouse's (the `readonly`
        // setting) isn't wired into its driver yet. The plan (docs/mcp-server.md) has each
        // such engine name its equivalent, such as a read-only login, before MCP serves it.
        // Redis, MongoDB and Elasticsearch have no read-only session at all; a user who may
        // only read is their guard (docs/nosql.md, N1, N2 and N3).
        DriverKind::SqlServer
        | DriverKind::DuckDb
        | DriverKind::ClickHouse
        | DriverKind::Redis
        | DriverKind::Mongo
        | DriverKind::Elasticsearch => None,
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
    /// Neither, as for a DuckDB file: the check is all there is, so MCP clients aren't served.
    CheckOnly,
}

/// The database's own guard on `driver`'s data sources.
pub fn guard(driver: DriverKind) -> Guard {
    match mode(driver) {
        Some(mode) => Guard::Session(mode),
        None if driver.uses_password() => Guard::Login,
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
            "a user or API key with only the read and view_index_metadata privileges"
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
    /// The engine has neither a read-only mode nor logins.
    NotServed,
}

impl McpServing {
    pub fn of(data_source: &DataSource) -> Self {
        match guard(data_source.config.driver()) {
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
            Self::NotServed => Some(format!("DbRover can't make {engine} read-only yet")),
        }
    }
}

/// The read-only mode on a connection that also runs changes: as [`mode`], except that a
/// SQLite file can't be opened read-only, so [`ReadOnlyMode::QueryOnly`] stands in.
pub fn mode_on_writable_connection(driver: DriverKind) -> Option<ReadOnlyMode> {
    match mode(driver)? {
        ReadOnlyMode::Connection => Some(ReadOnlyMode::QueryOnly),
        other => Some(other),
    }
}

/// The settings to connect with: a SQLite file is opened read-only, the engine's own
/// equivalent of a read-only transaction. Other engines connect as usual.
pub fn connection_config(mut data_source: DataSource) -> DataSource {
    if let ConnectionConfig::Sqlite(config) = &mut data_source.config {
        config.read_only = true;
    }
    data_source
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
        let guards = DriverKind::ALL.map(|driver| (driver, guard(driver)));
        for (driver, guard) in guards {
            let expected = match driver {
                DriverKind::Postgres | DriverKind::Mysql | DriverKind::Sqlite => {
                    matches!(guard, Guard::Session(_))
                }
                DriverKind::SqlServer
                | DriverKind::ClickHouse
                | DriverKind::Redis
                | DriverKind::Mongo
                | DriverKind::Elasticsearch => guard == Guard::Login,
                DriverKind::DuckDb => guard == Guard::CheckOnly,
            };
            assert!(expected, "{driver:?}: {guard:?}");
        }
        assert_eq!(
            read_only_login(DriverKind::Redis),
            "an ACL user with only +@read"
        );
    }
}
