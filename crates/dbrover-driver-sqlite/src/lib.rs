//! SQLite driver for DbRover, built on `rusqlite`.
//!
//! SQLite calls block, so every one runs on tokio's blocking pool and never stalls the
//! engine's async workers. Must run inside a tokio runtime.

mod catalog;
mod constraints;
mod execution;
mod table;

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dbrover_core::{
    BoxError, CancelHandle, CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, Cell,
    ChildrenPage, ChildrenQuery, Connection, DriverError, Rejection, RelationName, ResultCursor,
    ServerInfo, SqliteConfig, TableConstraints, TableDescription, TableName, TransactionStatus,
};
use rusqlite::OpenFlags;

const PRODUCT: &str = "SQLite";

/// Opens an existing database file.
///
/// Never creates the file: a typo in the path would otherwise silently produce a new, empty
/// database and the user would wonder where their tables went.
///
/// # Errors
///
/// [`DriverError::Connect`] if the file doesn't exist, can't be opened, or isn't a SQLite
/// database.
pub async fn connect(config: &SqliteConfig) -> Result<SqliteConnection, DriverError> {
    let path = config.path.clone();
    let read_only = config.read_only;
    let (connection, version) = tokio::task::spawn_blocking(move || open(&path, read_only))
        .await
        .map_err(connect_error)?
        .map_err(connect_error)?;

    Ok(SqliteConnection {
        cancel: Arc::new(execution::SqliteCancel::new(
            connection.get_interrupt_handle(),
        )),
        connection: Arc::new(Mutex::new(connection)),
        server_info: ServerInfo {
            product: PRODUCT.to_owned(),
            version,
        },
    })
}

/// Creates a new, empty database file at `path`, which must not exist yet.
///
/// The only way DbRover creates a database: [`connect`] never does. Writes the file's header
/// straight away, so it is a recognisable SQLite file even before it has any tables.
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
    // `create_new` fails atomically if the file exists, so an existing database is never
    // overwritten, even when the user confirmed "Replace" in the save dialog.
    std::fs::File::create_new(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            connect_error(format!("{} already exists", path.display()))
        } else {
            connect_error(error)
        }
    })?;
    let connection = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(connect_error)?;
    // A zero-length file is already a valid, empty database to SQLite, but other tools only
    // recognise SQLite files by the header, which SQLite writes on the first change.
    connection
        .execute_batch("PRAGMA user_version = 1; PRAGMA user_version = 0;")
        .map_err(connect_error)?;
    connection
        .close()
        .map_err(|(_, error)| connect_error(error))
}

fn connect_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

fn open(path: &Path, read_only: bool) -> rusqlite::Result<(rusqlite::Connection, String)> {
    let access = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let connection = rusqlite::Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // SQLite opens lazily and accepts any file here; the first query is what fails on a
    // file that isn't a database. Asking for the version doubles as that check.
    let version = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    Ok((connection, version))
}

pub struct SqliteConnection {
    /// Behind a mutex only so it can move into blocking tasks; there is one user at a time.
    connection: Arc<Mutex<rusqlite::Connection>>,
    /// Works without the lock, which is the point: the statement being cancelled holds it.
    cancel: Arc<execution::SqliteCancel>,
    server_info: ServerInfo,
}

impl SqliteConnection {
    async fn run_blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&rusqlite::Connection) -> Result<T, DriverError> + Send + 'static,
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

fn connection_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connection(error.into())
}

fn poisoned() -> DriverError {
    connection_error("a previous operation on this connection panicked")
}

#[async_trait]
impl Connection for SqliteConnection {
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

    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let (path, query) = (path.clone(), query.clone());
        self.run_blocking(move |connection| catalog::children_page(connection, &path, &query))
            .await
    }

    async fn search_catalog(
        &mut self,
        search: &CatalogSearch,
    ) -> Result<Option<CatalogMatches>, DriverError> {
        let search = search.clone();
        self.run_blocking(move |connection| catalog::search(connection, &search))
            .await
            .map(Some)
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
        self.run_blocking(move |connection| constraints::describe_constraints(connection, &table))
            .await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        let sql = sql.to_owned();
        self.run_blocking(move |connection| execution::validate(connection, &sql))
            .await
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        self.run_blocking(|connection| {
            Ok(if connection.is_autocommit() {
                TransactionStatus::Idle
            } else {
                TransactionStatus::InTransaction
            })
        })
        .await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        let Ok(mutex) = Arc::try_unwrap(self.connection) else {
            // Only `run_blocking` clones the Arc, and it has finished by the time `close`
            // can be called, so this is unreachable. Dropping the handle still closes.
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
mod tests {
    use dbrover_core::{FolderKind, NodeKind, conformance};

    use super::*;

    fn database_file() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .unwrap();
        file
    }

    #[tokio::test]
    async fn pages_a_schemas_tables_by_name_with_the_filter_in_the_query() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let creates: String = (0..2_500)
            .map(|n| format!("CREATE TABLE t_{n:04} (id INTEGER);"))
            .collect();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch(&format!(
                "BEGIN; {creates} CREATE TABLE \"Odd_%\" (id); COMMIT;"
            ))
            .unwrap();
        let mut connection = connect(&SqliteConfig {
            path: file.path().to_owned(),
            read_only: true,
        })
        .await
        .unwrap();
        let tables = CatalogPath::root()
            .child(NodeKind::Schema, "main")
            .child(NodeKind::Folder(FolderKind::Tables), "tables");

        let labels =
            conformance::check_children_pages(&mut connection, &tables, 1_000, 3, "t_12").await;
        assert_eq!(labels.len(), 2_501);
        let wildcard = ChildrenQuery {
            filter: Some("_%".to_owned()),
            after: None,
            limit: None,
        };
        let odd = connection.children_page(&tables, &wildcard).await.unwrap();
        assert_eq!(odd.nodes.len(), 1, "% and _ match themselves");
    }

    /// C0g: a search finds tables and views in every attached schema, as their levels list
    /// them, cut at the limit when there are more.
    #[tokio::test]
    async fn searches_every_schema_without_loading_it() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.db");
        let creates: String = (0..2_500)
            .map(|n| format!("CREATE TABLE t_{n:04} (id INTEGER);"))
            .collect();
        rusqlite::Connection::open(&main)
            .unwrap()
            .execute_batch(&format!(
                "BEGIN; {creates} CREATE VIEW Orders_Open AS SELECT 1; COMMIT;"
            ))
            .unwrap();
        let mut connection = connect(&SqliteConfig {
            path: main,
            read_only: false,
        })
        .await
        .unwrap();
        let archive = dir.path().join("archive.db");
        rusqlite::Connection::open(&archive)
            .unwrap()
            .execute_batch("CREATE TABLE old_orders (id INTEGER)")
            .unwrap();
        let attach = format!(
            "ATTACH DATABASE '{}' AS archive",
            archive.display().to_string().replace('\'', "''")
        );
        connection
            .execute(&attach)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();

        let orders = conformance::check_catalog_search(&mut connection, "ORDERS").await;
        let found: Vec<(&str, Option<&str>)> = orders
            .nodes
            .iter()
            .map(|node| (node.label.as_str(), node.detail.as_deref()))
            .collect();
        assert_eq!(
            found,
            [
                ("Orders_Open", Some("main")),
                ("old_orders", Some("archive"))
            ]
        );
        assert!(!orders.more);

        let hundred = conformance::check_catalog_search(&mut connection, "t_12").await;
        assert_eq!((hundred.nodes.len(), hundred.more), (100, false));
        let many = connection
            .search_catalog(&CatalogSearch::new("t_1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((many.nodes.len(), many.more), (100, true));
    }

    #[tokio::test]
    async fn conforms_to_the_connection_lifecycle() {
        for read_only in [false, true] {
            let file = database_file();
            let config = SqliteConfig {
                path: file.path().to_owned(),
                read_only,
            };

            let connection = connect(&config).await.unwrap();

            assert_eq!(connection.server_info().product, PRODUCT);
            conformance::check_lifecycle(Box::new(connection)).await;
        }
    }

    #[tokio::test]
    async fn creates_an_empty_database_but_never_overwrites_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.db");

        create_database(&path).await.unwrap();

        let header = std::fs::read(&path).unwrap();
        assert!(header.starts_with(b"SQLite format 3\0"), "missing header");
        let connection = connect(&SqliteConfig {
            path: path.clone(),
            read_only: false,
        })
        .await
        .unwrap();
        conformance::check_lifecycle(Box::new(connection)).await;

        conformance::check_connect_failure(create_database(&path).await.map(|()| "created"));
        assert_eq!(std::fs::read(&path).unwrap(), header);
        conformance::check_connect_failure(
            create_database(&dir.path().join("missing/new.db"))
                .await
                .map(|()| "created"),
        );
    }

    #[tokio::test]
    async fn missing_file_is_a_connect_error_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");

        let result = connect(&SqliteConfig {
            path: path.clone(),
            read_only: false,
        })
        .await;

        conformance::check_connect_failure(result);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn non_database_file_is_a_connect_error() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "not a database, just some text").unwrap();

        let result = connect(&SqliteConfig {
            path: file.path().to_owned(),
            read_only: false,
        })
        .await;

        conformance::check_connect_failure(result);
    }

    /// One object of every kind the explorer shows.
    const FIXTURE: &str = "
        CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE);
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL REFERENCES customers (id),
            total REAL
        );
        CREATE INDEX orders_customer ON orders (customer_id);
        CREATE TRIGGER orders_touch AFTER UPDATE ON orders BEGIN SELECT 1; END;
        CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100;
    ";

    fn labels_under<'a>(nodes: &'a [CatalogNode], parent: &CatalogPath) -> Vec<&'a str> {
        nodes
            .iter()
            .filter(|node| node.path.parent().as_ref() == Some(parent))
            .map(|node| node.label.as_str())
            .collect()
    }

    fn folder(parent: &CatalogPath, folder: FolderKind) -> CatalogPath {
        parent.child(NodeKind::Folder(folder), folder.label())
    }

    #[tokio::test]
    async fn walks_the_catalog_and_shows_definitions() {
        let file = tempfile::NamedTempFile::new().unwrap();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch(FIXTURE)
            .unwrap();
        let mut connection = connect(&SqliteConfig {
            path: file.path().to_owned(),
            read_only: true,
        })
        .await
        .unwrap();

        let nodes = conformance::walk_catalog(&mut connection, 5).await;
        conformance::check_invalid_path(&mut connection).await;

        let main = CatalogPath::root().child(NodeKind::Schema, "main");
        let tables = folder(&main, FolderKind::Tables);
        let orders = tables.child(NodeKind::Table, "orders");
        let sqlite_sequence = nodes
            .iter()
            .find(|node| node.path == tables.child(NodeKind::Table, "sqlite_sequence"))
            .unwrap();
        assert!(sqlite_sequence.system);
        let expected: [(CatalogPath, &[&str]); 6] = [
            (tables.clone(), &["customers", "orders", "sqlite_sequence"]),
            (folder(&main, FolderKind::Views), &["big_orders"]),
            (
                folder(&orders, FolderKind::Columns),
                &["id", "customer_id", "total"],
            ),
            (folder(&orders, FolderKind::Indexes), &["orders_customer"]),
            (
                folder(&orders, FolderKind::Constraints),
                &["(customer_id) → customers(id)"],
            ),
            (folder(&orders, FolderKind::Triggers), &["orders_touch"]),
        ];
        for (parent, labels) in expected {
            assert_eq!(labels_under(&nodes, &parent), labels, "under {parent}");
        }

        let view = folder(&main, FolderKind::Views).child(NodeKind::View, "big_orders");
        assert_eq!(
            connection.definition(&view).await.unwrap(),
            "CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100;"
        );
        let customers_columns = folder(
            &tables.child(NodeKind::Table, "customers"),
            FolderKind::Columns,
        );
        let id = nodes
            .iter()
            .find(|node| node.path == customers_columns.child(NodeKind::Column, "id"))
            .unwrap();
        assert_eq!(id.detail.as_deref(), Some("integer, primary key"));
    }

    #[tokio::test]
    async fn runs_pages_cancels_and_reports_error_positions() {
        let file = database_file();
        let mut connection = connect(&SqliteConfig {
            path: file.path().to_owned(),
            read_only: false,
        })
        .await
        .unwrap();

        conformance::check_execution(&mut connection, conformance::Spelling::STANDARD).await;
        conformance::check_transactions(&mut connection, conformance::Spelling::STANDARD).await;
        conformance::check_describe_table(
            &mut connection,
            conformance::UnquotedNames::IgnoreCase,
            conformance::Spelling::STANDARD,
        )
        .await;
        conformance::check_validate(&mut connection, conformance::Spelling::STANDARD).await;
        // Counts an endless sequence: only an interrupt stops it.
        conformance::check_cancel(
            &mut connection,
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) \
             SELECT count(*) FROM c",
        )
        .await;
    }

    #[tokio::test]
    async fn describes_an_autoincrement_key_as_an_identity_column() {
        let file = database_file();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch(
                "CREATE TABLE tickets (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT)",
            )
            .unwrap();
        let mut connection = connect(&SqliteConfig {
            path: file.path().to_owned(),
            read_only: false,
        })
        .await
        .unwrap();
        let name = |name: &str| RelationName {
            schema: None,
            name: dbrover_core::Identifier {
                name: name.to_owned(),
                quoted: false,
            },
        };
        let numbered = |table: Option<TableDescription>| -> Vec<bool> {
            table
                .unwrap()
                .columns
                .iter()
                .map(|column| column.identity.is_some())
                .collect()
        };
        let tickets = connection.describe_table(&name("tickets")).await.unwrap();
        let plain = connection.describe_table(&name("t")).await.unwrap();
        assert_eq!(numbered(tickets), [true, false]);
        assert_eq!(numbered(plain), [false], "a rowid key isn't AUTOINCREMENT");
    }
}
