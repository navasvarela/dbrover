//! DuckDB driver for DbRover, with DuckDB compiled in.
//!
//! DuckDB calls block, so every one runs on tokio's blocking pool and never stalls the engine's
//! async workers, as the SQLite driver does. Must run inside a tokio runtime.

mod bucket;
mod catalog;
mod errors;
mod execution;
mod parquet;
mod table;
mod values;
mod workspace;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::SystemTime;

use async_trait::async_trait;
use bucket::tree::BucketPath;
use dbrover_core::{
    BoxError, CATALOG_PAGE_SIZE, CancelHandle, CatalogNode, CatalogPath, Cell, ChildrenPage,
    ChildrenQuery, Connection, DriverError, DuckDbConfig, DuckDbDatabase, PageToken, Password,
    Rejection, RelationName, ResultCursor, ServerInfo, TableConstraints, TableDescription,
    TableName, TransactionStatus,
};
use duckdb::AccessMode;

pub use bucket::BucketError;
pub use parquet::{ParquetCompression, ParquetWriter};
pub use workspace::{
    DuckDbType, TableLoader, Workspace, WorkspaceLimit, WorkspaceLimits, WorkspacePeaks,
};

const PRODUCT: &str = "DuckDB";

/// How many pages of a bucket's level [`Connection::children`] lists at most, for its callers
/// that want a level whole (the conformance walk, the MCP server's catalog): ten thousand
/// keys. The explorer pages through a level instead, so it sees all of one.
const WHOLE_LEVEL_PAGES: usize = 10;

fn string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

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
    connect_with_secret(config, None).await
}

/// Opens the database as [`connect`] does, and hands DuckDB the credentials of the data
/// source's bucket, if it has one, signing in to it with `secret` where the bucket's sign-in
/// takes one (an access key's).
///
/// An extension DuckDB couldn't download doesn't stop the connection: the database's own
/// tables still work offline, and the bucket's secret is tried again before each statement.
///
/// # Errors
///
/// As for [`connect`], and [`DriverError::Connect`] when signing in to the bucket fails.
pub async fn connect_with_secret(
    config: &DuckDbConfig,
    secret: Option<&Password>,
) -> Result<DuckDbConnection, DriverError> {
    let database = config.database.clone();
    let (connection, shared, version) = tokio::task::spawn_blocking(move || open(&database))
        .await
        .map_err(connect_error)??;

    let mut connection = DuckDbConnection {
        cancel: Arc::new(execution::DuckDbCancel::new(connection.interrupt_handle())),
        connection: Arc::new(Mutex::new(connection)),
        _database: shared,
        server_info: ServerInfo::new(PRODUCT, version),
        bucket: config
            .storage
            .clone()
            .map(|storage| bucket::Bucket::new(storage, secret.cloned())),
    };
    connection
        .make_bucket_secret()
        .await
        .map_err(|error| DriverError::Connect(error.into()))?;
    Ok(connection)
}

/// Settings every database DbRover opens starts with, which can't change once it runs.
///
/// Persistent secrets are off, so DuckDB never writes a bucket's credentials to
/// `~/.duckdb/stored_secrets`, nor loads secrets someone else stored there (C2).
fn duckdb_config() -> Result<duckdb::Config, duckdb::Error> {
    duckdb::Config::default().with("allow_persistent_secrets", "false")
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
            duckdb_config()
                .and_then(duckdb::Connection::open_in_memory_with_flags)
                .map_err(connect_error)?,
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
    let config = duckdb_config()
        .and_then(|config| config.access_mode(access))
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
    /// The data source's bucket, whose secret is made before a statement when it is due.
    bucket: Option<bucket::Bucket>,
}

impl DuckDbConnection {
    /// Makes the bucket's secret if it was never made or is about to expire. An extension
    /// DuckDB couldn't download is logged and left for the statement that needs it to report,
    /// which [`errors`] explains; the secret is tried again before the next statement.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when signing in to the bucket fails, or DuckDB refuses the
    /// secret for another reason.
    async fn make_bucket_secret(&mut self) -> Result<(), DriverError> {
        let Some(bucket) = &mut self.bucket else {
            return Ok(());
        };
        let Some(statement) = bucket
            .due_secret(SystemTime::now())
            .await
            .map_err(connection_error)?
        else {
            return Ok(());
        };
        let made = self
            .run_blocking(move |connection| {
                connection
                    .execute_batch(statement.expose())
                    .map_err(errors::secret_error)
            })
            .await;
        match made {
            Ok(()) => {
                if let Some(bucket) = &mut self.bucket {
                    bucket.made();
                }
                Ok(())
            }
            Err(error) if errors::is_missing_extension(&error) => {
                tracing::warn!(
                    "the bucket's secret waits for a DuckDB extension: {}",
                    dbrover_core::describe_error(&error)
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// What `path` is in the data source's bucket, if it is in it.
    fn bucket_path(&self, path: &CatalogPath) -> Option<BucketPath> {
        let bucket = self.bucket.as_ref()?;
        BucketPath::of(path, &bucket.storage().url())
    }

    /// A page of the bucket's level at `path`, whose keys start with `prefix`.
    async fn bucket_level(
        &mut self,
        path: &CatalogPath,
        prefix: &str,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let Some(bucket) = &mut self.bucket else {
            return Err(DriverError::InvalidPath(path.clone()));
        };
        let lister = bucket
            .lister(SystemTime::now())
            .await
            .map_err(connection_error)?;
        let limit = query
            .limit
            .unwrap_or(CATALOG_PAGE_SIZE)
            .min(CATALOG_PAGE_SIZE);
        let after = query.after.as_ref().map(PageToken::as_str);
        let level = lister
            .list(prefix, after, limit)
            .await
            .map_err(|error| connection_error(BucketError::from(error)))?;
        Ok(ChildrenPage {
            nodes: bucket::tree::level_nodes(path, prefix, &level, query),
            next: level.next.map(PageToken::new),
        })
    }

    /// Every node of the bucket's level at `path`, a page at a time, up to
    /// [`WHOLE_LEVEL_PAGES`] pages.
    async fn whole_bucket_level(
        &mut self,
        path: &CatalogPath,
        prefix: &str,
    ) -> Result<Vec<CatalogNode>, DriverError> {
        let mut query = ChildrenQuery::default();
        let mut nodes = Vec::new();
        for _ in 0..WHOLE_LEVEL_PAGES {
            let page = self.bucket_level(path, prefix, &query).await?;
            nodes.extend(page.nodes);
            match page.next {
                Some(next) => query = query.after(next),
                None => return Ok(nodes),
            }
        }
        tracing::warn!(
            "{path} has more than {} entries; listing the first of them",
            nodes.len()
        );
        Ok(nodes)
    }

    /// The columns of the file at `url`, as DuckDB reads them from its footer (Parquet) or its
    /// first rows (CSV, JSON).
    async fn file_columns(
        &mut self,
        path: &CatalogPath,
        url: String,
    ) -> Result<Vec<CatalogNode>, DriverError> {
        self.make_bucket_secret().await?;
        let path = path.clone();
        self.run_blocking(move |connection| {
            let sql = format!("DESCRIBE SELECT * FROM {}", string_literal(&url));
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| errors::statement_error(error, &sql))?;
            let columns = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| errors::statement_error(error, &sql))?;
            columns
                .map(|column| {
                    let (name, data_type) =
                        column.map_err(|error| errors::statement_error(error, &sql))?;
                    Ok(bucket::tree::column_node(&path, &name, &data_type))
                })
                .collect()
        })
        .await
    }

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
        match self.bucket_path(path) {
            Some(BucketPath::Level { prefix }) => {
                return self.whole_bucket_level(path, &prefix).await;
            }
            Some(BucketPath::File { url }) => return self.file_columns(path, url).await,
            None => {}
        }
        let owned = path.clone();
        let mut nodes = self
            .run_blocking(move |connection| catalog::children(connection, &owned))
            .await?;
        if path.segments().is_empty()
            && let Some(bucket) = &self.bucket
        {
            nodes.push(bucket::tree::bucket_node(bucket.storage()));
        }
        Ok(nodes)
    }

    /// A bucket's level a page at a time, from its own listing; every other level whole.
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        match self.bucket_path(path) {
            Some(BucketPath::Level { prefix }) => self.bucket_level(path, &prefix, query).await,
            _ => Ok(ChildrenPage::whole(self.children(path).await?, query)),
        }
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        let path = path.clone();
        self.run_blocking(move |connection| catalog::definition(connection, &path))
            .await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.make_bucket_secret().await?;
        execution::execute(&self.connection, &self.cancel, sql, &[]).await
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.make_bucket_secret().await?;
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
mod bucket_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod workspace_tests;
