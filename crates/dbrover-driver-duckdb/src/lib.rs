//! DuckDB driver for DbRover, with DuckDB compiled in.
//!
//! DuckDB calls block, so every one runs on tokio's blocking pool and never stalls the engine's
//! async workers, as the SQLite driver does. Must run inside a tokio runtime.

mod catalog;
mod errors;
mod execution;
mod parquet;
mod table;
mod values;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use async_trait::async_trait;
use dbrover_core::{
    BoxError, CancelHandle, CatalogNode, CatalogPath, Cell, Connection, DriverError, DuckDbConfig,
    DuckDbDatabase, Rejection, RelationName, ResultCursor, ServerInfo, TableConstraints,
    TableDescription, TableName, TransactionStatus,
};
use duckdb::AccessMode;

pub use parquet::{ParquetCompression, ParquetWriter};

const PRODUCT: &str = "DuckDB";

/// Opens a database file, or a new database in memory.
///
/// Never creates a file: a typo in the path would otherwise silently produce a new, empty
/// database, as it would with SQLite. [`create_database`] does that, on request.
///
/// # Errors
///
/// [`DriverError::Connect`] if the file doesn't exist, isn't a DuckDB database, another
/// process holds it open for writing, or this process has it open the other way (see
/// [`SharedDatabase`]).
pub async fn connect(config: &DuckDbConfig) -> Result<DuckDbConnection, DriverError> {
    let database = config.database.clone();
    let (connection, shared, version) = tokio::task::spawn_blocking(move || open(&database))
        .await
        .map_err(connect_error)??;

    Ok(DuckDbConnection {
        cancel: Arc::new(execution::DuckDbCancel::new(connection.interrupt_handle())),
        connection: Arc::new(Mutex::new(connection)),
        _database: shared,
        server_info: ServerInfo {
            product: PRODUCT.to_owned(),
            version,
        },
    })
}

/// A connection, the file's shared database it belongs to (`None` in memory), and DuckDB's
/// version.
type Opened = (duckdb::Connection, Option<Arc<SharedDatabase>>, String);

fn open(database: &DuckDbDatabase) -> Result<Opened, DriverError> {
    let (connection, shared) = match database {
        DuckDbDatabase::File { path, read_only } => {
            let (connection, shared) = open_file(path, *read_only)?;
            (connection, Some(shared))
        }
        DuckDbDatabase::InMemory => (
            duckdb::Connection::open_in_memory().map_err(connect_error)?,
            None,
        ),
    };
    // `v1.5.6`, as `PRAGMA version` and the command-line client show it.
    let version: String = connection
        .query_row("SELECT version()", [], |row| row.get(0))
        .map_err(connect_error)?;
    let version = version.strip_prefix('v').unwrap_or(&version).to_owned();
    Ok((connection, shared, version))
}

/// A database file this process has open, held by a connection kept only to open more
/// connections to it. Every [`DuckDbConnection`] to the file holds it, so the database closes,
/// and lets go of the file, with the last of them.
///
/// Why one database per file: DuckDB keeps a database's data, and its log of committed
/// changes, in the process that opened it. Two databases opened on one file each see only
/// their own changes (and can overwrite each other's), so the explorer's connection and a
/// console's must share one: rows imported through the explorer then show in the console.
///
/// That holds for a read-only database beside a writable one too, which DuckDB allows in one
/// process: the reader wouldn't see the writer's later commits, and could read blocks the
/// writer's checkpoint has reused. So a file is open one way at a time, as DuckDB's lock makes
/// it between processes: a second configuration of an open file is refused.
struct SharedDatabase {
    first: Mutex<duckdb::Connection>,
    read_only: bool,
}

/// The databases open in this process, by file. Entries whose last connection has closed are
/// dead, and replaced when the file is opened again.
type OpenDatabases = HashMap<PathBuf, Weak<SharedDatabase>>;

static OPEN_DATABASES: LazyLock<Mutex<OpenDatabases>> = LazyLock::new(Mutex::default);

/// A new connection to the file's database, opening the database if this process hasn't.
fn open_file(
    path: &Path,
    read_only: bool,
) -> Result<(duckdb::Connection, Arc<SharedDatabase>), DriverError> {
    if !path.is_file() {
        return Err(connect_error(format!("{} doesn't exist", path.display())));
    }
    let key = fs::canonicalize(path).map_err(connect_error)?;
    let mut databases = OPEN_DATABASES.lock().map_err(|_| poisoned())?;
    if let Some(shared) = databases.get(&key).and_then(Weak::upgrade) {
        if shared.read_only != read_only {
            return Err(connect_error(open_the_other_way(path, shared.read_only)));
        }
        let connection = shared
            .first
            .lock()
            .map_err(|_| poisoned())?
            .try_clone()
            .map_err(connect_error)?;
        return Ok((connection, shared));
    }
    let access = if read_only {
        AccessMode::ReadOnly
    } else {
        AccessMode::ReadWrite
    };
    let config = duckdb::Config::default()
        .access_mode(access)
        .map_err(connect_error)?;
    let first = duckdb::Connection::open_with_flags(path, config).map_err(connect_error)?;
    let connection = first.try_clone().map_err(connect_error)?;
    let shared = Arc::new(SharedDatabase {
        first: Mutex::new(first),
        read_only,
    });
    databases.insert(key, Arc::downgrade(&shared));
    Ok((connection, shared))
}

/// Why a file this process has open can't be opened the other way too.
fn open_the_other_way(path: &Path, open_read_only: bool) -> String {
    let (open, wanted) = if open_read_only {
        ("read-only", "for writing")
    } else {
        ("for writing", "read-only")
    };
    format!(
        "{} is already open {open} in DbRover, and DuckDB opens a file one way at a time: \
         disconnect the data source that has it open to open it {wanted}",
        path.display()
    )
}

/// Creates a new, empty database file at `path`, which must not exist yet.
///
/// The only way DbRover creates a database: [`connect`] never does. A checkpoint writes the
/// file's header straight away, so it is a DuckDB file even before it has any tables.
///
/// # Errors
///
/// [`DriverError::Connect`] if `path` already exists, its folder doesn't, or it can't be
/// written.
pub async fn create_database(path: &Path) -> Result<(), DriverError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || create(&path))
        .await
        .map_err(connect_error)?
}

fn create(path: &Path) -> Result<(), DriverError> {
    // DuckDB opens an existing file rather than failing, so the check comes first. Between it
    // and the open, another program could create the file; the save dialog that chose the
    // name has already asked about replacing an existing one.
    if path.exists() {
        return Err(connect_error(format!("{} already exists", path.display())));
    }
    let connection = duckdb::Connection::open(path).map_err(connect_error)?;
    connection
        .execute_batch("CHECKPOINT")
        .map_err(connect_error)?;
    connection
        .close()
        .map_err(|(_, error)| connect_error(error))
}

fn connect_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

fn connection_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connection(error.into())
}

/// DuckDB's answer to anything but `ROLLBACK` in a transaction an error has spoilt.
fn is_aborted_transaction(error: &duckdb::Error) -> bool {
    error.to_string().contains("Current transaction is aborted")
}

fn poisoned() -> DriverError {
    connection_error("a previous operation on this connection panicked")
}

pub struct DuckDbConnection {
    /// Behind a mutex only so it can move into blocking tasks; there is one user at a time.
    connection: Arc<Mutex<duckdb::Connection>>,
    /// Works without the lock, which is the point: the statement being cancelled holds it.
    cancel: Arc<execution::DuckDbCancel>,
    /// Keeps the file's database open for the other connections to it; see
    /// [`SharedDatabase`].
    _database: Option<Arc<SharedDatabase>>,
    server_info: ServerInfo,
}

impl DuckDbConnection {
    async fn run_blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&duckdb::Connection) -> Result<T, DriverError> + Send + 'static,
    ) -> Result<T, DriverError> {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let connection = connection.lock().map_err(|_| poisoned())?;
            work(&connection)
        })
        .await
        .map_err(connection_error)?
    }
}

#[async_trait]
impl Connection for DuckDbConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.run_blocking(|connection| {
            connection
                .query_row("SELECT 1", [], |_| Ok(()))
                .map_err(connection_error)
        })
        .await
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let path = path.clone();
        self.run_blocking(move |connection| catalog::children(connection, &path))
            .await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        let path = path.clone();
        self.run_blocking(move |connection| catalog::definition(connection, &path))
            .await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.connection, &self.cancel, sql, &[]).await
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.connection, &self.cancel, sql, values).await
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        let table = table.clone();
        self.run_blocking(move |connection| table::describe_table(connection, &table))
            .await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        let table = table.clone();
        self.run_blocking(move |connection| table::describe_constraints(connection, &table))
            .await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        let sql = sql.to_owned();
        self.run_blocking(move |connection| execution::validate(connection, &sql))
            .await
    }

    /// Whether two statements in a row run in the same transaction: each runs in one of its
    /// own unless a transaction is open. `duckdb::Connection::is_autocommit` can't say, as it
    /// always answers yes. After an error inside a transaction DuckDB accepts nothing but
    /// `ROLLBACK`, so the question itself fails, which means just that.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        self.run_blocking(|connection| {
            let transaction_id =
                || connection.query_row("SELECT txid_current()", [], |row| row.get::<_, i64>(0));
            match (transaction_id(), transaction_id()) {
                (Ok(first), Ok(second)) if first == second => Ok(TransactionStatus::InTransaction),
                (Ok(_), Ok(_)) => Ok(TransactionStatus::Idle),
                (Err(error), _) | (_, Err(error)) if is_aborted_transaction(&error) => {
                    Ok(TransactionStatus::Failed)
                }
                (Err(error), _) | (_, Err(error)) => Err(connection_error(error)),
            }
        })
        .await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        let Ok(mutex) = Arc::try_unwrap(self.connection) else {
            // Only `run_blocking` and a statement's thread clone the Arc, and both have
            // finished by the time `close` can be called. Dropping the handle still closes.
            return Ok(());
        };
        let connection = mutex.into_inner().map_err(|_| poisoned())?;
        tokio::task::spawn_blocking(move || connection.close().map_err(|(_, error)| error))
            .await
            .map_err(connection_error)?
            .map_err(connection_error)
    }
}

#[cfg(test)]
mod tests;
