use std::path::{Path, PathBuf};

use dbrover_core::conformance;
use dbrover_core::{CatalogNode, FolderKind, NodeKind, Row, ServerConfig, TlsMode};

use super::*;

/// Point the tests at a server, e.g. `clickhouse://dbrover:dbrover@localhost:58123/dbrover` from
/// the repository's `docker-compose.yml` (see `test-servers.env`). The tests are skipped when
/// it isn't set, so `cargo test` works offline.
const TEST_URL_VAR: &str = "DBROVER_TEST_CLICKHOUSE_URL";
/// The same server's HTTPS port, e.g. `clickhouse://dbrover:dbrover@localhost:58443/dbrover`.
const TLS_TEST_URL_VAR: &str = "DBROVER_TEST_CLICKHOUSE_TLS_URL";

/// Sums a trillion numbers: minutes of work, which a cancel cuts short.
const LONG_RUNNING: &str = "SELECT sum(number) FROM numbers(1000000000000)";

struct TestServer {
    config: ClickHouseConfig,
    password: Option<Password>,
}

fn test_server() -> Option<TestServer> {
    server_from(TEST_URL_VAR)
}

fn server_from(variable: &str) -> Option<TestServer> {
    let Ok(url) = std::env::var(variable) else {
        eprintln!("skipping: {variable} is not set");
        return None;
    };
    Some(parse_url(&url))
}

/// The authority that signed the test server's certificate.
fn test_ca_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
}

/// Reads `clickhouse://user:password@host:port/database`.
fn parse_url(url: &str) -> TestServer {
    let rest = url
        .strip_prefix("clickhouse://")
        .expect("clickhouse:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (address, database) = rest.split_once('/').expect("/database in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    TestServer {
        config: ClickHouseConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: port.parse::<u16>().expect("numeric port").into(),
                database: database.to_owned(),
                user: user.to_owned(),
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
            },
            read_only: false,
        },
        password: Some(Password::new(password)),
    }
}

impl TestServer {
    async fn connect(&self) -> ClickHouseConnection {
        connect(&self.config, self.password.as_ref(), Route::Direct)
            .await
            .unwrap_or_else(|error| panic!("{TEST_URL_VAR}: {error:?}"))
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<ClickHouseConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

/// Runs `sql` to its end and returns its rows.
async fn run(connection: &mut ClickHouseConnection, sql: &str) -> Vec<Row> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {}", dbrover_core::describe_error(&error)));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("read rows"));
    }
    cursor.finish().await.expect("finish");
    rows
}

#[tokio::test]
async fn conforms_to_the_connection_lifecycle() {
    let Some(server) = test_server() else { return };
    let connection = server.connect().await;

    assert_eq!(connection.server_info().product, "ClickHouse");
    assert!(
        connection
            .server_info()
            .version
            .starts_with(char::is_numeric),
        "{:?}",
        connection.server_info()
    );
    conformance::check_lifecycle(Box::new(connection)).await;
}

#[tokio::test]
async fn wrong_password_is_a_connect_error() {
    let Some(server) = test_server() else { return };
    let result = connect(
        &server.config,
        Some(&Password::new("definitely wrong")),
        Route::Direct,
    )
    .await;

    conformance::check_connect_failure(result);
}

#[tokio::test]
async fn unreachable_server_is_a_connect_error() {
    let config = ClickHouseConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_owned(),
            // Port 1 (tcpmux) is reserved and has no listener on any normal machine, so the
            // connection is refused immediately instead of timing out.
            port: 1.into(),
            database: "default".to_owned(),
            user: "default".to_owned(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
        },
        read_only: false,
    };

    conformance::check_connect_failure(connect(&config, None, Route::Direct).await);
}

/// HTTPS is a port of its own, which answers only the modes that encrypt: connecting there
/// at all is proof of TLS.
#[tokio::test]
async fn encrypts_as_the_tls_mode_says() {
    let Some(server) = server_from(TLS_TEST_URL_VAR) else {
        return;
    };
    let verified = TlsMode::VerifyFull {
        ca_file: Some(test_ca_file()),
    };
    for tls in [TlsMode::Require, verified] {
        let mut connection = server
            .connect_with(tls.clone())
            .await
            .unwrap_or_else(|error| panic!("{tls:?}: {error:?}"));
        connection.ping().await.expect("ping over TLS");
    }

    // The certificate isn't signed by a public authority, and the port speaks only HTTPS.
    for tls in [TlsMode::VerifyFull { ca_file: None }, TlsMode::Disable] {
        conformance::check_connect_failure(server.connect_with(tls).await);
    }
}

/// Through a tunnel the URL names a local port, while the certificate is still checked
/// against the server's name, so the same server under a name the certificate lacks fails.
#[tokio::test]
async fn connects_through_a_tunnel_checking_tls_against_the_servers_name() {
    let Some(server) = server_from(TLS_TEST_URL_VAR) else {
        return;
    };
    let mut config = server.config.clone();
    config.server.tls = TlsMode::VerifyFull {
        ca_file: Some(test_ca_file()),
    };
    let address = format!(
        "{}:{}",
        config.server.host,
        config.server.port_number().unwrap()
    );
    let route = Route::Tunnel(conformance::forwarder(&address).await);

    let mut connection = connect(&config, server.password.as_ref(), route)
        .await
        .unwrap();
    connection.ping().await.expect("ping through the tunnel");

    config.server.host = "db.invalid".to_owned();
    conformance::check_connect_failure(connect(&config, server.password.as_ref(), route).await);
}

/// ClickHouse has no transactions to check, and no key that tells a table's rows apart to
/// describe for editing.
#[tokio::test]
async fn runs_pages_cancels_and_reports_error_positions() {
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    conformance::check_execution(&mut connection, conformance::Spelling::STANDARD).await;
    conformance::check_validate(&mut connection, conformance::Spelling::STANDARD).await;
    conformance::check_cancel(&mut connection, LONG_RUNNING).await;

    Box::new(connection).close().await.unwrap();
}

/// A result of millions of rows pages like any other, and abandoning it stops the query at
/// once, rather than leaving the session busy until the server has sent the rest.
#[tokio::test]
async fn pages_through_millions_of_rows_and_abandons_the_rest() {
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    let started = std::time::Instant::now();

    let mut cursor = connection
        .execute("SELECT number, toString(number) FROM numbers(10000000)")
        .await
        .expect("select");
    for page in 0..3 {
        let rows = cursor.next_page(500).await.expect("page");
        assert_eq!(rows.len(), 500);
        assert_eq!(
            rows[0][0].as_deref(),
            Some((page * 500).to_string().as_str())
        );
    }
    assert!(!cursor.is_exhausted());
    assert!(matches!(
        cursor.finish().await,
        Err(DriverError::Statement { .. })
    ));
    connection.ping().await.expect("the session is free again");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

/// What a value's type promises reaches the grid: every digit of a 64-bit integer or a
/// decimal, a decimal's trailing zeros, `NULL` apart from text, and arrays, maps and dates as
/// ClickHouse writes them.
#[tokio::test]
async fn keeps_values_as_their_types_write_them() {
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    let rows = run(
        &mut connection,
        "SELECT toUInt64(18446744073709551615), toDecimal64(12.5, 2), \
         CAST(NULL AS Nullable(String)), 'it''s', [1, 2], map('a', 1), \
         toDate('2026-10-24'), toInt128('-170141183460469231731687303715884105728')",
    )
    .await;

    let expected = [
        Some("18446744073709551615"),
        Some("12.50"),
        None,
        Some("it's"),
        Some("[1,2]"),
        Some(r#"{"a":1}"#),
        Some("2026-10-24"),
        Some("-170141183460469231731687303715884105728"),
    ];
    assert_eq!(
        rows,
        vec![expected.map(|value| value.map(str::to_owned)).to_vec()]
    );
}

/// The session keeps what a console expects between statements: the current database, settings
/// and temporary tables. Statements without a result report the rows they wrote.
#[tokio::test]
async fn keeps_the_session_between_statements() {
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    run(&mut connection, "USE system").await;
    let database = run(&mut connection, "SELECT currentDatabase()").await;
    assert_eq!(database, vec![vec![Some("system".to_owned())]]);

    run(&mut connection, "SET max_threads = 3").await;
    let setting = run(&mut connection, "SELECT getSetting('max_threads')").await;
    assert_eq!(setting, vec![vec![Some("3".to_owned())]]);

    run(
        &mut connection,
        "CREATE TEMPORARY TABLE dbrover_kept (n Int8)",
    )
    .await;
    let cursor = connection
        .execute("INSERT INTO dbrover_kept VALUES (1), (2)")
        .await
        .expect("insert");
    assert!(cursor.columns().is_empty());
    assert_eq!(cursor.finish().await.expect("finish insert"), Some(2));

    let described = connection
        .describe_table(&RelationName {
            schema: None,
            name: dbrover_core::Identifier {
                name: "dbrover_kept".to_owned(),
                quoted: false,
            },
        })
        .await
        .expect("describe");
    let described = described.expect("the temporary table is found");
    let names: Vec<&str> = described.column_names().collect();
    assert_eq!(names, ["n"]);
    assert!(described.primary_key.is_empty());
}

/// What running `sql` fails with, whether the request or reading its answer fails.
async fn failure(connection: &mut ClickHouseConnection, sql: &str) -> String {
    let error = match connection.execute(sql).await {
        Ok(cursor) => cursor.finish().await.err(),
        Err(error) => Some(error),
    };
    let error = error.unwrap_or_else(|| panic!("{sql} succeeded"));
    dbrover_core::describe_error(&error)
}

/// A read-only connection, as a read-only data source's sessions open: ClickHouse itself
/// refuses a change and lifting the setting, whatever DbRover's check would have said, while
/// reading, `USE`, a query's own settings and cancelling work as usual.
#[tokio::test]
async fn a_read_only_connection_refuses_changes_with_clickhouses_own_error() {
    let Some(server) = test_server() else { return };
    let mut writer = server.connect().await;
    // In `default`, where the catalog test, listing `dbrover`'s tables, doesn't see it.
    let table = format!("default.dbrover_read_only_{}", Uuid::new_v4().simple());
    run(
        &mut writer,
        &format!("CREATE TABLE {table} (n UInt8) ENGINE = Memory"),
    )
    .await;
    let mut config = server.config.clone();
    config.read_only = true;
    let mut reader = connect(&config, server.password.as_ref(), Route::Direct)
        .await
        .expect("connect read-only");

    let count = run(&mut reader, &format!("SELECT count() FROM {table}")).await;
    assert_eq!(count, vec![vec![Some("0".to_owned())]]);
    run(&mut reader, "SELECT 1 SETTINGS max_threads = 1").await;
    run(&mut reader, "USE system").await;
    let refused = [
        format!("INSERT INTO {table} VALUES (1)"),
        format!("DROP TABLE {table}"),
        "SELECT 1 SETTINGS readonly = 0".to_owned(),
        "SET readonly = 0".to_owned(),
    ];
    for sql in refused {
        let error = failure(&mut reader, &sql).await;
        assert!(error.contains("in readonly mode"), "{sql}: {error}");
    }
    conformance::check_cancel(&mut reader, LONG_RUNNING).await;

    run(&mut writer, &format!("DROP TABLE {table}")).await;
}

/// One object of every kind the explorer shows.
const FIXTURE: &[&str] = &[
    "DROP DICTIONARY IF EXISTS dbrover_customers",
    "DROP VIEW IF EXISTS dbrover_order_totals",
    "DROP VIEW IF EXISTS dbrover_big_orders",
    "DROP TABLE IF EXISTS dbrover_orders",
    "CREATE TABLE dbrover_orders (
        id UInt64,
        customer String,
        total Nullable(Decimal(10, 2)),
        INDEX dbrover_orders_customer customer TYPE bloom_filter GRANULARITY 1
    ) ENGINE = MergeTree ORDER BY id",
    "CREATE VIEW dbrover_big_orders AS SELECT id, total FROM dbrover_orders WHERE total > 100",
    "CREATE MATERIALIZED VIEW dbrover_order_totals ENGINE = SummingMergeTree ORDER BY customer
        AS SELECT customer, sum(total) AS total FROM dbrover_orders GROUP BY customer",
    "CREATE DICTIONARY dbrover_customers (id UInt64, customer String) PRIMARY KEY id
        SOURCE(CLICKHOUSE(TABLE 'dbrover_orders')) LAYOUT(FLAT()) LIFETIME(0)",
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
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    for statement in FIXTURE {
        run(&mut connection, statement).await;
    }

    let nodes = conformance::walk_catalog(&mut connection, 5).await;
    conformance::check_invalid_path(&mut connection).await;

    let database = &server.config.server.database;
    let schema = CatalogPath::root().child(NodeKind::Schema, database);
    assert!(labels_under(&nodes, &CatalogPath::root()).contains(&database.as_str()));
    let system = nodes
        .iter()
        .find(|node| node.path == CatalogPath::root().child(NodeKind::Schema, "system"))
        .expect("system is listed");
    assert!(system.system);
    let inner_tables: Vec<&CatalogNode> = nodes
        .iter()
        .filter(|node| node.label.starts_with(".inner"))
        .collect();
    assert!(!inner_tables.is_empty() && inner_tables.iter().all(|node| node.system));

    let orders = folder(&schema, FolderKind::Tables).child(NodeKind::Table, "dbrover_orders");
    let objects = [
        (
            FolderKind::Tables,
            NodeKind::Table,
            "dbrover_orders",
            "ORDER BY id",
        ),
        (
            FolderKind::Views,
            NodeKind::View,
            "dbrover_big_orders",
            "total > 100",
        ),
        (
            FolderKind::MaterializedViews,
            NodeKind::MaterializedView,
            "dbrover_order_totals",
            "SummingMergeTree",
        ),
        (
            FolderKind::Dictionaries,
            NodeKind::Dictionary,
            "dbrover_customers",
            "LAYOUT(FLAT())",
        ),
    ];
    for (folder_kind, kind, name, in_definition) in objects {
        let parent = folder(&schema, folder_kind);
        let listed: Vec<&str> = nodes
            .iter()
            .filter(|node| node.path.parent().as_ref() == Some(&parent) && !node.system)
            .map(|node| node.label.as_str())
            .collect();
        assert_eq!(listed, [name], "under {parent}");
        let path = parent.child(kind, name);
        let definition = connection.definition(&path).await.unwrap();
        assert!(definition.contains(in_definition), "{path}: {definition}");
    }
    let columns = labels_under(&nodes, &folder(&orders, FolderKind::Columns));
    assert_eq!(columns, ["id", "customer", "total"]);
    let indexes = folder(&orders, FolderKind::Indexes);
    let index = nodes
        .iter()
        .find(|node| node.path.parent().as_ref() == Some(&indexes))
        .expect("the index is listed");
    assert_eq!(
        (index.label.as_str(), index.detail.as_deref()),
        ("dbrover_orders_customer", Some("bloom_filter (customer)"))
    );

    for statement in &FIXTURE[..4] {
        run(&mut connection, statement).await;
    }
}
