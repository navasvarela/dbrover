//! How the MCP server's `execute` runs statements that change something: in one ordinary
//! transaction, committed only if every statement succeeds. The counterpart of
//! [`crate::read_only`], beside it so a connection's two kinds of transaction are set up in
//! one place.
//!
//! What may run is decided before this, by `dbrover_sql::analyze` and the project's
//! permissions; this only opens and ends the transaction.

use std::time::Duration;

use dbrover_core::{Connection, DriverError, DriverKind};

use crate::read_only::run_control;

/// How an engine opens a transaction that may change things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMode {
    begin: &'static [&'static str],
    /// Whether the engine can also be told a time limit for the transaction's statements.
    sets_timeout: bool,
}

/// The engine's read-write transaction, or `None` for engines DbRover doesn't run changes on
/// for MCP clients, which serve such an engine for reading only.
pub fn mode(driver: DriverKind) -> Option<WriteMode> {
    match driver {
        DriverKind::Postgres => Some(WriteMode {
            begin: &["BEGIN READ WRITE"],
            sets_timeout: true,
        }),
        DriverKind::Mysql => Some(WriteMode {
            begin: &["START TRANSACTION READ WRITE"],
            sets_timeout: false,
        }),
        // Undoes the `query_only` a read on the same connection set
        // (`ReadOnlyMode::QueryOnly`).
        DriverKind::Sqlite => Some(WriteMode {
            begin: &["PRAGMA query_only = OFF", "BEGIN"],
            sets_timeout: false,
        }),
        // ClickHouse has no transactions in practice, SQL Server's and DuckDB's are left for
        // later (docs/nosql.md, N0a), and the others have none DbRover uses.
        DriverKind::SqlServer
        | DriverKind::DuckDb
        | DriverKind::ClickHouse
        | DriverKind::Redis
        | DriverKind::Mongo
        | DriverKind::Elasticsearch => None,
    }
}

/// Opens the transaction, with `timeout` as each statement's time limit where the engine can
/// be told one.
///
/// # Errors
///
/// [`DriverError`] if the database refuses to open it.
pub async fn begin(
    connection: &mut dyn Connection,
    mode: WriteMode,
    timeout: Option<Duration>,
) -> Result<(), DriverError> {
    for statement in mode.begin {
        run_control(connection, statement).await?;
    }
    if let Some(timeout) = timeout.filter(|_| mode.sets_timeout) {
        let set = format!("SET LOCAL statement_timeout = {}", timeout.as_millis());
        run_control(connection, &set).await?;
    }
    Ok(())
}

/// Commits what [`begin`] opened.
///
/// # Errors
///
/// [`DriverError`] if the commit fails; the transaction is then rolled back by the database.
pub async fn commit(connection: &mut dyn Connection) -> Result<(), DriverError> {
    run_control(connection, "COMMIT").await
}

/// Rolls back what [`begin`] opened.
///
/// # Errors
///
/// [`DriverError`] if the rollback fails, which leaves the connection in an unknown state.
pub async fn rollback(connection: &mut dyn Connection) -> Result<(), DriverError> {
    run_control(connection, "ROLLBACK").await
}
