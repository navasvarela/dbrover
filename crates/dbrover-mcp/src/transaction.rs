//! Running each call in a transaction of its own, read-only or not, with a time limit. The
//! transactions themselves are the engine's (`dbrover_engine::read_only`, shared with the
//! consoles of read-only data sources, and `dbrover_engine::read_write`); this is the MCP
//! server's use of them.
//!
//! Every call is wrapped the same way:
//! 1. start the engine's transaction for the call's [`Access`] ([`begin`]);
//! 2. run the work, each statement under a [`Watchdog`] that cancels it at the time limit;
//! 3. roll back a read, or a change that failed; commit a change that succeeded ([`end`]).
//!
//! The parser has already refused statements the project doesn't allow, but it can't see
//! inside a function; for reading, the read-only transaction is what stops
//! `SELECT writes_something()`, or, on an engine without one, the MCP login's grants.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use dbrover_core::{
    CancelHandle, ColumnInfo, Connection, ConnectionConfig, DriverError, Row, describe_error,
};
use dbrover_engine::read_only::{self, Guard, ReadOnlyMode, Storage};
use dbrover_engine::read_write::{self, WriteMode};

/// How long a cancelled statement gets to give up before its connection is given up on.
const CANCEL_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("cancelled after the statement timeout of {0:?}")]
    TimedOut(Duration),
    /// The engine has no transaction for the call's access: changes on an engine without a
    /// read-write mode (see `read_write::mode`).
    #[error("DbRover has no transaction to run this in on {0} for MCP clients")]
    NoTransaction(&'static str),
}

impl RunError {
    /// Whether the connection should be closed rather than reused: it failed, or a statement
    /// on it may still be running.
    pub(crate) fn breaks_connection(&self) -> bool {
        matches!(
            self,
            Self::TimedOut(_) | Self::Driver(DriverError::Connection(_) | DriverError::Lost(_))
        )
    }
}

/// A query's rows, cut at the row limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rows {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) rows: Vec<Row>,
    /// More rows were left unread.
    pub(crate) truncated: bool,
    /// Rows changed or returned, as the database reports it, when all were read.
    pub(crate) row_count: Option<u64>,
}

/// Whether a call only reads, or may change something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// In the database's own read-only mode, then rolled back.
    Read,
    /// In an ordinary transaction, committed if every statement succeeds.
    Write,
}

/// The engine's transaction for one call: the engine module that opens and ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallMode {
    Read(ReadOnlyMode),
    /// Reading on an engine without a read-only mode, through the data source's MCP login
    /// (`read_only::Guard::Login`): nothing is opened or rolled back, and the login's grants
    /// refuse writes.
    ReadAsLogin,
    Write(WriteMode),
}

/// How a call with `access` runs on a data source connected with `config`, or `None` when its
/// database has no such mode. `writable` says whether the connection was opened to run
/// changes too (`Changes::open_writable`).
pub(crate) fn mode(config: &ConnectionConfig, access: Access, writable: bool) -> Option<CallMode> {
    let driver = config.driver();
    match access {
        Access::Read => match read_only::guard(driver, Storage::of(config)) {
            Guard::Session(_) if writable => {
                read_only::mode_on_writable_connection(driver).map(CallMode::Read)
            }
            Guard::Session(mode) => Some(CallMode::Read(mode)),
            Guard::Login => Some(CallMode::ReadAsLogin),
            Guard::CheckOnly => None,
        },
        Access::Write => read_write::mode(driver).map(CallMode::Write),
    }
}

/// What a call's transaction is set up with, besides its mode.
pub(crate) struct CallSettings<'a> {
    pub(crate) timeout: Duration,
    /// Sets the schemas unqualified names resolve in, for the project's allow-list
    /// (`Scope::search_path`); only on engines with a `search_path`.
    pub(crate) search_path: Option<&'a str>,
}

/// Starts the call's transaction.
pub(crate) async fn begin(
    connection: &mut dyn Connection,
    mode: CallMode,
    settings: &CallSettings<'_>,
) -> Result<(), DriverError> {
    match mode {
        CallMode::Read(mode) => read_only::begin(connection, mode, Some(settings.timeout)).await?,
        CallMode::Write(mode) => {
            read_write::begin(connection, mode, Some(settings.timeout)).await?;
        }
        CallMode::ReadAsLogin => {}
    }
    if let Some(search_path) = settings.search_path {
        connection.execute(search_path).await?.finish().await?;
    }
    Ok(())
}

/// Ends what [`begin`] started, and hands back the work's outcome: a successful change is
/// committed, anything else rolled back. A failed rollback leaves the connection in an
/// unknown state, so it counts as a broken connection; a failed commit is the call's error.
pub(crate) async fn end<T>(
    connection: &mut dyn Connection,
    mode: CallMode,
    outcome: Result<T, RunError>,
) -> Result<T, RunError> {
    if let (CallMode::Write(_), Ok(_)) = (mode, &outcome) {
        return match read_write::commit(connection).await {
            Ok(()) => outcome,
            Err(error) => {
                rollback(connection, mode).await.map_err(RunError::Driver)?;
                Err(RunError::Driver(error))
            }
        };
    }
    match (outcome, rollback(connection, mode).await) {
        (outcome, Ok(())) => outcome,
        (Err(error), Err(rollback)) => {
            tracing::warn!("the rollback failed too: {}", describe_error(&rollback));
            Err(error)
        }
        (Ok(_), Err(rollback)) => Err(RunError::Driver(rollback)),
    }
}

async fn rollback(connection: &mut dyn Connection, mode: CallMode) -> Result<(), DriverError> {
    let rolled_back = match mode {
        CallMode::Read(mode) => read_only::rollback(connection, mode).await,
        CallMode::Write(_) => read_write::rollback(connection).await,
        CallMode::ReadAsLogin => Ok(()),
    };
    rolled_back.map_err(|error| DriverError::Connection(Box::new(error)))
}

/// Cancels a call's statements that run past the time limit.
#[derive(Clone)]
pub(crate) struct Watchdog {
    cancel: Arc<dyn CancelHandle>,
    timeout: Duration,
}

impl Watchdog {
    pub(crate) fn new(cancel: Arc<dyn CancelHandle>, timeout: Duration) -> Self {
        Self { cancel, timeout }
    }

    /// Runs `work`, cancelling it once the time limit has passed.
    pub(crate) async fn watch<T>(
        &self,
        work: impl Future<Output = Result<T, DriverError>>,
    ) -> Result<T, RunError> {
        let mut work = std::pin::pin!(work);
        tokio::select! {
            outcome = &mut work => return outcome.map_err(RunError::Driver),
            () = tokio::time::sleep(self.timeout) => {}
        }
        if let Err(error) = self.cancel.cancel().await {
            tracing::warn!("could not cancel the statement: {}", describe_error(&error));
        }
        // Whatever the work returns now, it ended because of the cancel. If it doesn't end,
        // it is dropped, and the caller closes the connection it was running on.
        let _ = tokio::time::timeout(CANCEL_GRACE, work).await;
        Err(RunError::TimedOut(self.timeout))
    }
}

/// Runs a statement and reads up to `max_rows` of its rows.
pub(crate) async fn fetch(
    connection: &mut dyn Connection,
    sql: &str,
    max_rows: usize,
) -> Result<Rows, DriverError> {
    let mut cursor = connection.execute(sql).await?;
    let columns = cursor.columns().to_vec();
    // One row past the limit tells "exactly max_rows" from "more than max_rows", which a
    // cursor can't always tell before it is asked for another row.
    let wanted = max_rows.saturating_add(1);
    let mut rows = Vec::new();
    while rows.len() < wanted && !cursor.is_exhausted() {
        let page = cursor.next_page(wanted - rows.len()).await?;
        if page.is_empty() {
            break;
        }
        rows.extend(page);
    }
    let truncated = rows.len() > max_rows;
    rows.truncate(max_rows);
    // Unread rows are abandoned with the cursor rather than read to the end; the rollback
    // or commit that follows ends the statement either way.
    let row_count = if truncated {
        None
    } else {
        cursor.finish().await?
    };
    Ok(Rows {
        columns,
        rows,
        truncated,
        row_count,
    })
}
