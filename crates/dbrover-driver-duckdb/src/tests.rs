use dbrover_core::conformance::{self, Spelling, UnquotedNames};
use dbrover_core::{FolderKind, NodeKind};

use super::*;

/// Runs far longer than the conformance check allows: a trillion-row cross join.
const LONG_RUNNING: &str = "SELECT sum(a.range * b.range) FROM range(1000000) a, range(1000000) b";

fn in_memory() -> DuckDbConfig {
    DuckDbConfig {
        database: DuckDbDatabase::InMemory,
    }
}

fn file(path: &Path, read_only: bool) -> DuckDbConfig {
    DuckDbConfig {
        database: DuckDbDatabase::File {
            path: path.to_owned(),
            read_only,
        },
    }
}

async fn run(connection: &mut DuckDbConnection, sql: &str) -> Option<u64> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {}", dbrover_core::describe_error(&error)));
    while !cursor.is_exhausted() {
        cursor.next_page(500).await.unwrap();
    }
    cursor.finish().await.unwrap()
}

async fn rows(connection: &mut DuckDbConnection, sql: &str) -> Vec<Vec<Option<String>>> {
    let mut cursor = connection.execute(sql).await.unwrap();
    let rows = cursor.next_page(1000).await.unwrap();
    cursor.finish().await.unwrap();
    rows
}

fn text(values: &[Option<&str>]) -> Vec<Option<String>> {
    values
        .iter()
        .map(|value| value.map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn conforms_to_the_connection_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lifecycle.duckdb");
    create_database(&path).await.unwrap();
    for config in [in_memory(), file(&path, false), file(&path, true)] {
        let connection = connect(&config).await.unwrap();

        assert_eq!(connection.server_info().product, PRODUCT);
        assert!(
            connection.server_info().version.starts_with("1.5."),
            "{}",
            connection.server_info()
        );
        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn creates_an_empty_database_but_never_overwrites_one_or_opens_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.duckdb");

    conformance::check_connect_failure(connect(&file(&path, false)).await);
    assert!(!path.exists(), "connecting created the file");

    create_database(&path).await.unwrap();
    let contents = std::fs::read(&path).unwrap();
    assert!(!contents.is_empty());
    conformance::check_lifecycle(Box::new(connect(&file(&path, false)).await.unwrap())).await;

    conformance::check_connect_failure(create_database(&path).await.map(|()| "created"));
    assert_eq!(std::fs::read(&path).unwrap(), contents);

    let not_duckdb = dir.path().join("notes.txt");
    std::fs::write(&not_duckdb, "not a database, just some text").unwrap();
    conformance::check_connect_failure(connect(&file(&not_duckdb, false)).await);
}

#[tokio::test]
async fn runs_pages_cancels_and_reports_error_positions() {
    let mut connection = connect(&in_memory()).await.unwrap();

    conformance::check_execution(&mut connection, Spelling::STANDARD).await;
    conformance::check_transactions(&mut connection, Spelling::STANDARD).await;
    conformance::check_describe_table(
        &mut connection,
        UnquotedNames::IgnoreCase,
        Spelling::STANDARD,
    )
    .await;
    conformance::check_validate(&mut connection, Spelling::STANDARD).await;
    conformance::check_cancel(&mut connection, LONG_RUNNING).await;

    Box::new(connection).close().await.unwrap();
}

/// A cancel with nothing running doesn't stop the next statement.
#[tokio::test]
async fn a_cancel_while_idle_is_forgotten() {
    let mut connection = connect(&in_memory()).await.unwrap();
    connection.cancel_handle().cancel().await.unwrap();

    assert_eq!(
        rows(&mut connection, "SELECT 42").await,
        [text(&[Some("42")])]
    );
}

/// The exit criterion of DB3: query a Parquet file from an in-memory database.
/// Whichever way DuckDB got into the build (compiled, or DuckDB's prebuilt library; see
/// docs/duckdb-prebuilt.md), it is the release libduckdb-sys's bindings were generated for,
/// with Parquet and JSON inside. A library from another release, or one without an extension,
/// would mostly still work: the extension would be downloaded from DuckDB's extension
/// repository the first time a query needs it. Update the version here with the `duckdb` crate.
#[tokio::test]
async fn links_duckdb_1_5_6_with_parquet_and_json() {
    let mut connection = connect(&in_memory()).await.unwrap();

    let version = rows(
        &mut connection,
        "SELECT library_version FROM pragma_version()",
    )
    .await;
    let linked = rows(
        &mut connection,
        "SELECT extension_name FROM duckdb_extensions() \
         WHERE install_mode = 'STATICALLY_LINKED' ORDER BY 1",
    )
    .await;
    let json = rows(
        &mut connection,
        r#"SELECT json_extract('{"a": [1, 2]}', '$.a[1]')"#,
    )
    .await;

    assert_eq!(version, [text(&[Some("v1.5.6")])]);
    for extension in ["json", "parquet"] {
        assert!(linked.contains(&text(&[Some(extension)])), "{linked:?}");
    }
    assert_eq!(json, [text(&[Some("2")])]);
}

#[tokio::test]
async fn queries_a_parquet_file_from_memory() {
    let dir = tempfile::tempdir().unwrap();
    let parquet = dir.path().join("orders.parquet");
    let mut connection = connect(&in_memory()).await.unwrap();
    run(
        &mut connection,
        &format!(
            "COPY (SELECT range AS id, 'order ' || range AS name FROM range(3)) \
             TO '{}' (FORMAT parquet)",
            parquet.display()
        ),
    )
    .await;

    let read = rows(
        &mut connection,
        &format!(
            "SELECT id, name FROM read_parquet('{}') ORDER BY id",
            parquet.display()
        ),
    )
    .await;

    assert_eq!(
        read,
        [
            text(&[Some("0"), Some("order 0")]),
            text(&[Some("1"), Some("order 1")]),
            text(&[Some("2"), Some("order 2")]),
        ]
    );
}

#[tokio::test]
async fn counts_changed_rows_but_shows_returned_ones() {
    let mut connection = connect(&in_memory()).await.unwrap();
    run(&mut connection, "CREATE TABLE t (n integer)").await;

    assert_eq!(
        run(&mut connection, "INSERT INTO t VALUES (1), (2), (3)").await,
        Some(3)
    );
    assert_eq!(
        run(&mut connection, "UPDATE t SET n = n + 1 WHERE n > 1").await,
        Some(2)
    );
    let returned = rows(&mut connection, "DELETE FROM t WHERE n = 1 RETURNING n").await;
    assert_eq!(returned, [text(&[Some("1")])]);
    // Shaped like a status, but a query: its row is shown.
    let counted = rows(&mut connection, "SELECT count(*) AS \"Count\" FROM t").await;
    assert_eq!(counted, [text(&[Some("2")])]);
    for status in [
        "BEGIN",
        "SET threads = 2",
        "ROLLBACK",
        "CREATE VIEW v AS SELECT 1",
    ] {
        let cursor = connection.execute(status).await.unwrap();
        assert!(cursor.columns().is_empty(), "{status}");
        assert_eq!(cursor.finish().await.unwrap(), None, "{status}");
    }
}

#[tokio::test]
async fn shows_values_as_duckdb_does() {
    let mut connection = connect(&in_memory()).await.unwrap();

    let values = rows(
        &mut connection,
        "SELECT true, 12.50::decimal(10, 2), 'it''s', '\\xDE\\xAD'::blob, \
         DATE '2026-10-24', TIMESTAMP '2026-10-24 07:28:23.93', [1, 2, 3], \
         {'a': 1, 'b': 'x'}, 12345678901234567890123456789012345678::hugeint, \
         TIMESTAMPTZ '2026-10-24 07:28:23+02', NULL",
    )
    .await;

    assert_eq!(
        values,
        [text(&[
            Some("true"),
            Some("12.50"),
            Some("it's"),
            Some("\\xDE\\xAD"),
            Some("2026-10-24"),
            Some("2026-10-24 07:28:23.930"),
            Some("[1, 2, 3]"),
            Some("{a: 1, b: x}"),
            Some("12345678901234567890123456789012345678"),
            Some("2026-10-24 05:28:23+00:00"),
            None,
        ])]
    );
}

/// One object of every kind the explorer shows.
const FIXTURE: &[&str] = &[
    "CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        customer VARCHAR NOT NULL,
        total DECIMAL(10, 2) CHECK (total >= 0)
    )",
    "CREATE INDEX orders_customer ON orders (customer)",
    "CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100",
    "CREATE MACRO add_one(a) AS a + 1",
    "CREATE MACRO recent(n) AS TABLE SELECT * FROM orders ORDER BY id DESC LIMIT n",
    "CREATE SEQUENCE order_numbers START 1",
];

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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.duckdb");
    create_database(&path).await.unwrap();
    let mut connection = connect(&file(&path, false)).await.unwrap();
    for statement in FIXTURE {
        run(&mut connection, statement).await;
    }

    let nodes = conformance::walk_catalog(&mut connection, 6).await;
    conformance::check_invalid_path(&mut connection).await;

    let database = CatalogPath::root().child(NodeKind::Database, "shop");
    assert!(labels_under(&nodes, &CatalogPath::root()).contains(&"shop"));
    let schema = database.child(NodeKind::Schema, "main");
    let orders = folder(&schema, FolderKind::Tables).child(NodeKind::Table, "orders");
    let exactly: [(CatalogPath, &[&str]); 7] = [
        (folder(&schema, FolderKind::Tables), &["orders"]),
        (folder(&schema, FolderKind::Views), &["big_orders"]),
        (
            folder(&schema, FolderKind::Functions),
            &["add_one", "recent"],
        ),
        (folder(&schema, FolderKind::Sequences), &["order_numbers"]),
        (
            folder(&orders, FolderKind::Columns),
            &["id", "customer", "total"],
        ),
        (folder(&orders, FolderKind::Indexes), &["orders_customer"]),
        (folder(&database, FolderKind::Tables), &[]),
    ];
    for (parent, labels) in exactly {
        assert_eq!(labels_under(&nodes, &parent), labels, "under {parent}");
    }
    let constraints = labels_under(&nodes, &folder(&orders, FolderKind::Constraints));
    assert_eq!(constraints.len(), 2, "{constraints:?}");

    let definitions = [
        (orders.clone(), "CREATE TABLE orders"),
        (
            folder(&schema, FolderKind::Views).child(NodeKind::View, "big_orders"),
            "CREATE VIEW big_orders",
        ),
        (
            folder(&schema, FolderKind::Functions).child(NodeKind::Function, "recent"),
            "CREATE MACRO main.recent(n) AS TABLE",
        ),
        (
            folder(&schema, FolderKind::Sequences).child(NodeKind::Sequence, "order_numbers"),
            "CREATE SEQUENCE order_numbers",
        ),
        (
            folder(&orders, FolderKind::Indexes).child(NodeKind::Index, "orders_customer"),
            "CREATE INDEX orders_customer",
        ),
    ];
    for (path, expected) in definitions {
        let definition = connection.definition(&path).await.unwrap();
        assert!(definition.contains(expected), "{path}: {definition}");
    }

    Box::new(connection).close().await.unwrap();
}

/// The explorer and a console connect to one file: each sees what the other commits, and the
/// file lets go once both have closed.
#[tokio::test]
async fn connections_to_one_file_share_its_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.duckdb");
    create_database(&path).await.unwrap();
    let mut explorer = connect(&file(&path, false)).await.unwrap();
    let mut console = connect(&file(&path, false)).await.unwrap();

    run(&mut explorer, "CREATE TABLE t (n integer)").await;
    run(&mut explorer, "INSERT INTO t VALUES (1)").await;
    assert_eq!(
        rows(&mut console, "SELECT n FROM t").await,
        [text(&[Some("1")])]
    );
    Box::new(explorer).close().await.unwrap();
    run(&mut console, "INSERT INTO t VALUES (2)").await;
    Box::new(console).close().await.unwrap();

    let mut reopened = connect(&file(&path, true)).await.unwrap();
    assert_eq!(
        rows(&mut reopened, "SELECT count(*) FROM t").await,
        [text(&[Some("2")])]
    );
}

/// What running `sql` fails with, whether starting it or reading it to its end fails.
async fn failure(connection: &mut DuckDbConnection, sql: &str) -> String {
    let error = match connection.execute(sql).await {
        Ok(cursor) => cursor.finish().await.err(),
        Err(error) => Some(error),
    };
    let error = error.unwrap_or_else(|| panic!("{sql} succeeded"));
    dbrover_core::describe_error(&error)
}

/// A file opened read-only, as a read-only data source's sessions open it: DuckDB itself
/// refuses a change, a write hidden in a function included, while reading works.
#[tokio::test]
async fn a_read_only_file_refuses_changes_with_duckdbs_own_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orders.duckdb");
    create_database(&path).await.unwrap();
    let mut writer = connect(&file(&path, false)).await.unwrap();
    run(&mut writer, "CREATE TABLE orders (id integer)").await;
    run(&mut writer, "CREATE SEQUENCE orders_id").await;
    Box::new(writer).close().await.unwrap();

    let mut reader = connect(&file(&path, true)).await.unwrap();
    assert_eq!(
        rows(&mut reader, "SELECT count(*) FROM orders").await,
        [text(&[Some("0")])]
    );
    for sql in [
        "INSERT INTO orders VALUES (1)",
        // Reads as a SELECT, but advances the sequence.
        "SELECT nextval('orders_id')",
    ] {
        let error = failure(&mut reader, sql).await;
        assert!(error.contains("read-only mode"), "{sql}: {error}");
    }
    Box::new(reader).close().await.unwrap();
}

/// A file is open one way at a time in this process, as DuckDB's lock makes it between
/// processes: neither read-only beside a writer, nor for writing beside a reader.
#[tokio::test]
async fn opens_a_file_one_way_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lake.duckdb");
    create_database(&path).await.unwrap();

    let writer = connect(&file(&path, false)).await.unwrap();
    let refused = connect(&file(&path, true)).await.err().unwrap();
    let message = dbrover_core::describe_error(&refused);
    assert!(
        message.contains("is already open for writing in DbRover"),
        "{message}"
    );
    Box::new(writer).close().await.unwrap();

    let reader = connect(&file(&path, true)).await.unwrap();
    let second_reader = connect(&file(&path, true)).await.unwrap();
    let refused = connect(&file(&path, false)).await.err().unwrap();
    let message = dbrover_core::describe_error(&refused);
    assert!(message.contains("is already open read-only"), "{message}");
    Box::new(second_reader).close().await.unwrap();
    Box::new(reader).close().await.unwrap();

    connect(&file(&path, false)).await.unwrap();
}
