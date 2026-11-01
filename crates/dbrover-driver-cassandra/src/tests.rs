use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dbrover_core::conformance;
use dbrover_core::{
    CatalogPath, FolderKind, NodeKind, Port, RejectionKind, Row, ServerConfig, TlsMode,
    describe_error,
};
use dbrover_sql::Dialect;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

/// Point the tests at the servers, e.g. `cql://dbrover:dbrover@localhost:59042` from the
/// repository's `docker-compose.yml` (see `test-servers.env`): ScyllaDB with every pull
/// request, Cassandra nightly. Each test runs on every server whose variable is set, and skips
/// when none is, so `cargo test` works offline.
const TEST_URL_VARS: [&str; 2] = ["DBROVER_TEST_SCYLLA_URL", "DBROVER_TEST_CASSANDRA_URL"];

/// Held by every test that makes or drops a keyspace, or walks the whole catalog: a walk that
/// meets another test's keyspace while it is dropped fails to describe it.
static SCHEMA: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// ScyllaDB over TLS, `cqls://…`, its certificate signed by the test authority.
const TLS_URL_VAR: &str = "DBROVER_TEST_SCYLLA_TLS_URL";

struct TestServer {
    name: &'static str,
    config: CassandraConfig,
    password: Option<Password>,
}

impl TestServer {
    async fn connect(&self) -> CassandraConnection {
        self.connect_with(self.config.clone())
            .await
            .unwrap_or_else(|error| panic!("{}: connect: {}", self.name, describe_error(&error)))
    }

    async fn connect_with(
        &self,
        config: CassandraConfig,
    ) -> Result<CassandraConnection, DriverError> {
        connect(&config, self.password.as_ref(), Route::Direct).await
    }

    /// A keyspace of the test's own, made empty, so tests running at once don't meet. One
    /// replica; on ScyllaDB without tablets, which its materialized views and indexes need.
    async fn keyspace(&self, connection: &mut CassandraConnection, name: &str) -> String {
        let keyspace = format!("dbrover_{name}");
        run(connection, &format!("DROP KEYSPACE IF EXISTS {keyspace}")).await;
        let tablets = if connection.server_info().product == execution::SCYLLADB {
            " AND tablets = {'enabled': false}"
        } else {
            ""
        };
        run(
            connection,
            &format!(
                "CREATE KEYSPACE {keyspace} WITH replication = \
                 {{'class': 'NetworkTopologyStrategy', 'replication_factor': 1}}{tablets}"
            ),
        )
        .await;
        keyspace
    }
}

/// The servers whose variable is set.
fn test_servers() -> Vec<TestServer> {
    let servers: Vec<TestServer> = TEST_URL_VARS
        .into_iter()
        .filter_map(|name| Some((name, test_url(name)?)))
        .map(|(name, url)| parse_url(name, &url))
        .collect();
    if servers.is_empty() {
        eprintln!("skipping: none of {TEST_URL_VARS:?} is set");
    }
    servers
}

/// The URL in the variable `name`, unless it is unset or empty.
fn test_url(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|url| !url.is_empty())
}

/// `cql://user:password@host:port`, or `cqls://…` for TLS checked against the test authority.
fn parse_url(name: &'static str, url: &str) -> TestServer {
    let (tls, rest) = match url.split_once("://") {
        Some(("cqls", rest)) => (
            TlsMode::VerifyFull {
                ca_file: Some(test_ca_file()),
            },
            rest,
        ),
        Some(("cql", rest)) => (TlsMode::Disable, rest),
        _ => panic!("{name}: a cql:// or cqls:// URL"),
    };
    let rest = rest.trim_end_matches('/');
    let (credentials, address) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
    let (host, port) = address.rsplit_once(':').expect("host:port in the URL");
    TestServer {
        name,
        config: CassandraConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: Port::from(port.parse::<u16>().expect("a port")),
                database: String::new(),
                user: user.to_owned(),
                tls,
                ssh: None,
                password_command: None,
            },
            local_datacenter: String::new(),
            statement_timeout: None,
        },
        password: (!password.is_empty()).then(|| Password::new(password.to_owned())),
    }
}

/// The authority that signed the ScyllaDB test server's certificate.
fn test_ca_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
}

/// Runs `cql` to its end and returns its rows.
async fn run(connection: &mut CassandraConnection, cql: &str) -> Vec<Row> {
    let mut cursor = connection
        .execute(cql)
        .await
        .unwrap_or_else(|error| panic!("{cql}: {}", describe_error(&error)));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("a page"));
    }
    cursor.finish().await.expect("finish");
    rows
}

fn texts(rows: &[Row]) -> Vec<Vec<&str>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.as_deref().unwrap_or("NULL"))
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn connects_and_says_which_server_it_reached() {
    for server in test_servers() {
        let connection = server.connect().await;
        let info = connection.server_info().clone();
        let expected = match server.name {
            "DBROVER_TEST_SCYLLA_URL" => execution::SCYLLADB,
            _ => "Apache Cassandra",
        };
        assert_eq!(info.product, expected, "{}", server.name);
        assert!(
            info.version.starts_with(|c: char| c.is_ascii_digit()),
            "{info}"
        );
        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn reports_what_stops_it_connecting() {
    for server in test_servers() {
        let mut wrong = server.config.clone();
        wrong.server.user = "nobody".to_owned();
        conformance::check_connect_failure(server.connect_with(wrong).await);
        let mut closed = server.config.clone();
        closed.server.port = Port::from(1);
        conformance::check_connect_failure(server.connect_with(closed).await);
        let mut keyspace = server.config.clone();
        keyspace.server.database = "dbrover_no_such_keyspace".to_owned();
        conformance::check_connect_failure(server.connect_with(keyspace).await);
    }
}

#[tokio::test]
async fn lists_keyspaces_and_their_tables() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "catalog").await;
        run(
            &mut connection,
            &format!("CREATE TABLE {keyspace}.orders (id int PRIMARY KEY, total decimal)"),
        )
        .await;
        let nodes = conformance::walk_catalog(&mut connection, 3).await;
        let tables = CatalogPath::root()
            .child(NodeKind::Schema, &keyspace)
            .child(NodeKind::Folder(FolderKind::Tables), "tables");
        assert!(
            nodes
                .iter()
                .any(|node| node.path == tables.child(NodeKind::Table, "orders")),
            "{}: {nodes:#?}",
            server.name
        );
        let system = nodes
            .iter()
            .find(|node| node.label == "system_schema")
            .expect("system_schema");
        assert!(system.system);
        conformance::check_invalid_path(&mut connection).await;
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

/// 1,200 rows of one partition come back in clustering order, a page at a time.
#[tokio::test]
async fn pages_a_large_table_in_order() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "paging").await;
        let table = format!("{keyspace}.numbers");
        run(
            &mut connection,
            &format!("CREATE TABLE {table} (p int, n int, PRIMARY KEY (p, n))"),
        )
        .await;
        for batch in 0..12 {
            let inserts: String = (0..100)
                .map(|n| {
                    format!(
                        "INSERT INTO {table} (p, n) VALUES (0, {});\n",
                        batch * 100 + n
                    )
                })
                .collect();
            run(
                &mut connection,
                &format!("BEGIN UNLOGGED BATCH\n{inserts}APPLY BATCH"),
            )
            .await;
        }
        let mut cursor = connection
            .execute(&format!("SELECT n FROM {table} WHERE p = 0"))
            .await
            .expect("select");
        let mut numbers = Vec::new();
        let mut pages = 0;
        while !cursor.is_exhausted() {
            let page = cursor.next_page(500).await.expect("a page");
            assert!(page.len() <= 500);
            numbers.extend(page.into_iter().map(|row| row[0].clone()));
            pages += 1;
        }
        cursor.finish().await.expect("finish");
        let expected: Vec<Option<String>> = (0..1200).map(|n| Some(n.to_string())).collect();
        assert_eq!(numbers, expected, "{}", server.name);
        assert_eq!(pages, 3);
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

/// Every CQL type comes back as CQL writes it, and its column with its type's name;
/// collections, tuples and user-defined types as JSON.
#[tokio::test]
async fn writes_every_type_as_cql_reads_it() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "types").await;
        run(
            &mut connection,
            &format!("CREATE TYPE {keyspace}.address (city text, zip int)"),
        )
        .await;
        let columns = [
            ("a_ascii", "ascii", "'plain'", "plain"),
            (
                "a_bigint",
                "bigint",
                "-9223372036854775808",
                "-9223372036854775808",
            ),
            ("a_blob", "blob", "0xcafe", "0xcafe"),
            ("a_boolean", "boolean", "true", "true"),
            ("a_date", "date", "'2026-11-01'", "2026-11-01"),
            ("a_decimal", "decimal", "12.50", "12.50"),
            ("a_double", "double", "2.5", "2.5"),
            ("a_duration", "duration", "1h30m", "1h30m"),
            ("a_float", "float", "1.5", "1.5"),
            ("a_inet", "inet", "'10.0.0.1'", "10.0.0.1"),
            ("a_int", "int", "42", "42"),
            ("a_smallint", "smallint", "-7", "-7"),
            ("a_text", "text", "'it''s'", "it's"),
            ("a_time", "time", "'09:30:00.5'", "09:30:00.500000000"),
            (
                "a_timestamp",
                "timestamp",
                "'2026-11-01 09:30:00.123+0000'",
                "2026-11-01 09:30:00.123+0000",
            ),
            (
                "a_timeuuid",
                "timeuuid",
                "50554d6e-29bb-11e5-b345-feff819cdc9f",
                "50554d6e-29bb-11e5-b345-feff819cdc9f",
            ),
            ("a_tinyint", "tinyint", "8", "8"),
            (
                "a_uuid",
                "uuid",
                "123e4567-e89b-12d3-a456-426614174000",
                "123e4567-e89b-12d3-a456-426614174000",
            ),
            (
                "a_varint",
                "varint",
                "123456789012345678901234567890",
                "123456789012345678901234567890",
            ),
            ("a_list", "list<int>", "[1, 2]", "[1, 2]"),
            ("a_set", "set<text>", "{'a', 'b'}", r#"["a", "b"]"#),
            (
                "a_map",
                "map<text, int>",
                "{'one': 1, 'two': 2}",
                r#"{"one": 1, "two": 2}"#,
            ),
            ("a_tuple", "tuple<int, text>", "(1, 'x')", r#"[1, "x"]"#),
            (
                "a_address",
                "frozen<address>",
                "{city: 'Oviedo', zip: 33001}",
                r#"{"city": "Oviedo", "zip": 33001}"#,
            ),
        ];
        let definitions: Vec<String> = columns
            .iter()
            .map(|(name, column_type, _, _)| format!("{name} {column_type}"))
            .collect();
        run(
            &mut connection,
            &format!(
                "CREATE TABLE {keyspace}.everything (id int PRIMARY KEY, {})",
                definitions.join(", ")
            ),
        )
        .await;
        let names: Vec<&str> = columns.iter().map(|(name, ..)| *name).collect();
        let literals: Vec<&str> = columns.iter().map(|(_, _, literal, _)| *literal).collect();
        run(
            &mut connection,
            &format!(
                "INSERT INTO {keyspace}.everything (id, {}) VALUES (1, {})",
                names.join(", "),
                literals.join(", ")
            ),
        )
        .await;
        let select = format!("SELECT {} FROM {keyspace}.everything", names.join(", "));
        let mut cursor = connection.execute(&select).await.expect("select");
        let types: Vec<Option<String>> = cursor
            .columns()
            .iter()
            .map(|column| column.type_name.clone())
            .collect();
        let rows = cursor.next_page(10).await.expect("the row");
        cursor.finish().await.expect("finish");
        let row = rows.first().expect("one row");
        for (index, (name, column_type, _, expected)) in columns.iter().enumerate() {
            assert_eq!(
                row[index].as_deref(),
                Some(*expected),
                "{}: {name}",
                server.name
            );
            let declared = types[index].as_deref().unwrap_or_default();
            let expected_type = column_type.replace("frozen<address>", "address");
            assert_eq!(declared, expected_type, "{}: {name}'s type", server.name);
        }
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

#[tokio::test]
async fn points_at_a_syntax_error_and_stays_usable() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        let sql = "SELECT key FROM system.local WHERE key = = 'local'";
        let error = match connection.execute(sql).await {
            Ok(_) => panic!("{}: {sql} ran", server.name),
            Err(error) => error,
        };
        match error {
            DriverError::Statement { position, .. } => {
                assert_eq!(position, sql.rfind('='), "{}", server.name);
            }
            other => panic!("{}: {other:?}", server.name),
        }
        connection.ping().await.expect("usable after an error");
    }
}

/// Preparing catches a misspelt column and points at it, and a syntax error, without running
/// anything; a good statement is accepted.
#[tokio::test]
async fn validates_by_preparing() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "validate").await;
        run(
            &mut connection,
            &format!("CREATE TABLE {keyspace}.orders (id int PRIMARY KEY, total int)"),
        )
        .await;
        let good = format!("SELECT id, total FROM {keyspace}.orders WHERE id = 1");
        assert_eq!(connection.validate(&good).await.expect("validate"), None);
        let misspelt = format!("SELECT id, totl FROM {keyspace}.orders WHERE id = 1");
        let rejection = connection
            .validate(&misspelt)
            .await
            .expect("validate")
            .expect("a rejection");
        assert_eq!(rejection.kind, RejectionKind::UnknownName, "{rejection:?}");
        assert_eq!(rejection.position, misspelt.find("totl"), "{rejection:?}");
        let broken = format!("SELECT id FROM {keyspace}.orders WHERE id = = 1");
        let rejection = connection
            .validate(&broken)
            .await
            .expect("validate")
            .expect("a rejection");
        assert_eq!(rejection.kind, RejectionKind::Syntax, "{rejection:?}");
        assert_eq!(rejection.position, broken.rfind('='), "{rejection:?}");
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

/// A keyspace in the data source is where unqualified names resolve, and `USE` changes it.
#[tokio::test]
async fn starts_in_the_data_sources_keyspace() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "keyspace").await;
        run(
            &mut connection,
            &format!("CREATE TABLE {keyspace}.notes (id int PRIMARY KEY, note text)"),
        )
        .await;
        run(
            &mut connection,
            &format!("INSERT INTO {keyspace}.notes (id, note) VALUES (1, 'hi')"),
        )
        .await;
        let mut config = server.config.clone();
        config.server.database.clone_from(&keyspace);
        let mut inside = server.connect_with(config).await.expect("connect");
        assert_eq!(
            texts(&run(&mut inside, "SELECT note FROM notes").await),
            [["hi"]]
        );
        let mut outside = server.connect().await;
        run(&mut outside, &format!("USE {keyspace}")).await;
        assert_eq!(
            texts(&run(&mut outside, "SELECT note FROM notes").await),
            [["hi"]]
        );
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

/// On ScyllaDB, a statement timeout goes with each `SELECT` as `USING TIMEOUT`, which the
/// server takes.
#[tokio::test]
async fn runs_selects_with_the_statement_timeout() {
    for server in test_servers() {
        let mut config = server.config.clone();
        config.statement_timeout = NonZeroU32::new(5);
        let mut connection = server.connect_with(config).await.expect("connect");
        assert_eq!(
            texts(&run(&mut connection, "SELECT key FROM system.local").await),
            [["local"]],
            "{}",
            server.name
        );
    }
}

#[tokio::test]
async fn connects_over_tls_checking_the_servers_name() {
    let Some(url) = test_url(TLS_URL_VAR) else {
        eprintln!("skipping: {TLS_URL_VAR} is not set");
        return;
    };
    let server = parse_url(TLS_URL_VAR, &url);
    let mut connection = server.connect().await;
    connection.ping().await.expect("ping over TLS");

    let mut require = server.config.clone();
    require.server.tls = TlsMode::Require;
    server
        .connect_with(require)
        .await
        .expect("require accepts any certificate");

    let mut public = server.config.clone();
    public.server.tls = TlsMode::VerifyFull { ca_file: None };
    conformance::check_connect_failure(server.connect_with(public).await);
}

/// ⌘. stops waiting for an answer that doesn't come, through a tunnel that holds the node's
/// answers back, and the connection works again once they flow.
#[tokio::test]
async fn cancel_stops_waiting_at_once() {
    for server in test_servers() {
        let address = format!(
            "{}:{}",
            server.config.server.host,
            server.config.server.port_number().expect("a port")
        );
        let (local, held) = holding_forwarder(&address).await;
        let mut connection = connect(
            &server.config,
            server.password.as_ref(),
            Route::Tunnel(local),
        )
        .await
        .unwrap_or_else(|error| panic!("{}: connect: {}", server.name, describe_error(&error)));
        connection.ping().await.expect("ping through the tunnel");

        held.store(true, Ordering::SeqCst);
        let handle = connection.cancel_handle();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.cancel().await
        });
        let started = Instant::now();
        let result = connection.execute("SELECT key FROM system.local").await;
        assert!(
            matches!(result, Err(DriverError::Statement { .. })),
            "{}: {:?}",
            server.name,
            result.err()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        canceller.await.expect("cancel task").expect("cancel");

        held.store(false, Ordering::SeqCst);
        connection.ping().await.expect("usable after cancel");
    }
}

/// A port that forwards every connection to `server`, like an SSH tunnel's local end, whose
/// answers from the server wait while the flag it returns is set.
async fn holding_forwarder(server: &str) -> (std::net::SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let local = listener.local_addr().expect("local address");
    let held = Arc::new(AtomicBool::new(false));
    let server = server.to_owned();
    let flag = Arc::clone(&held);
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let Ok(outbound) = TcpStream::connect(&server).await else {
                continue;
            };
            let (mut from_client, mut to_client) = inbound.into_split();
            let (mut from_server, mut to_server) = outbound.into_split();
            tokio::spawn(async move {
                let _ = tokio::io::copy(&mut from_client, &mut to_server).await;
            });
            let flag = Arc::clone(&flag);
            tokio::spawn(async move {
                let mut buffer = vec![0; 64 * 1024];
                while let Ok(read) = from_server.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    while flag.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    if to_client.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (local, held)
}

/// Every kind of object a keyspace holds is listed in its folder, a table's columns in key
/// order with their roles, and each definition runs again in an emptied keyspace.
#[tokio::test]
async fn lists_every_kind_of_object_with_definitions_that_run_again() {
    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "explorer").await;
        let language = if connection.server_info().product == execution::SCYLLADB {
            ("lua", "return input * 2")
        } else {
            ("java", "return input * 2;")
        };
        for statement in [
            format!("CREATE TYPE {keyspace}.address (city text, zip int)"),
            format!(
                "CREATE TABLE {keyspace}.orders (customer text, placed timestamp, id int, \
                 total int, note text static, tags map<text, int>, home frozen<address>, \
                 PRIMARY KEY ((customer), placed, id)) \
                 WITH CLUSTERING ORDER BY (placed DESC, id ASC)"
            ),
            format!("CREATE INDEX by_total ON {keyspace}.orders (total)"),
            format!(
                "CREATE MATERIALIZED VIEW {keyspace}.orders_by_id AS SELECT id, customer, placed, total FROM {keyspace}.orders \
                 WHERE id IS NOT NULL AND customer IS NOT NULL AND placed IS NOT NULL \
                 PRIMARY KEY (id, customer, placed)"
            ),
            format!(
                "CREATE FUNCTION {keyspace}.twice (input int) RETURNS NULL ON NULL INPUT \
                 RETURNS int LANGUAGE {} AS '{}'",
                language.0, language.1
            ),
            format!(
                "CREATE FUNCTION {keyspace}.plus (sum int, input int) CALLED ON NULL INPUT \
                 RETURNS int LANGUAGE {} AS '{}'",
                language.0,
                if language.0 == "lua" {
                    "return (sum or 0) + (input or 0)"
                } else {
                    "return (sum == null ? 0 : sum) + (input == null ? 0 : input);"
                }
            ),
            format!("CREATE AGGREGATE {keyspace}.total (int) SFUNC plus STYPE int INITCOND 0"),
        ] {
            run(&mut connection, &statement).await;
        }

        let nodes = conformance::walk_catalog(&mut connection, 5).await;
        let root = CatalogPath::root().child(NodeKind::Schema, &keyspace);
        let labels_in = |parent: &CatalogPath| -> Vec<(String, Option<String>)> {
            nodes
                .iter()
                .filter(|node| node.path.parent().as_ref() == Some(parent))
                .map(|node| (node.label.clone(), node.detail.clone()))
                .collect()
        };
        let folder = |kind: FolderKind| root.child(NodeKind::Folder(kind), kind.label());
        let named = |labels: Vec<(String, Option<String>)>| -> Vec<String> {
            labels.into_iter().map(|(label, _)| label).collect()
        };
        assert_eq!(named(labels_in(&folder(FolderKind::Tables))), ["orders"]);
        assert_eq!(
            named(labels_in(&folder(FolderKind::MaterializedViews))),
            ["orders_by_id"],
            "{}: the index's own view is left out",
            server.name
        );
        assert_eq!(named(labels_in(&folder(FolderKind::Types))), ["address"]);
        assert_eq!(
            named(labels_in(&folder(FolderKind::Functions))),
            ["plus(int, int)", "twice(int)"]
        );
        assert_eq!(
            named(labels_in(&folder(FolderKind::Aggregates))),
            ["total(int)"]
        );
        let orders = folder(FolderKind::Tables).child(NodeKind::Table, "orders");
        let columns = labels_in(&orders.child(NodeKind::Folder(FolderKind::Columns), "columns"));
        let expected = [
            ("customer", "text · partition key"),
            ("placed", "timestamp · clustering key, descending"),
            ("id", "int · clustering key"),
            ("note", "text · static"),
            ("home", "frozen<address>"),
            ("tags", "map<text, int>"),
            ("total", "int"),
        ];
        let columns: Vec<(&str, &str)> = columns
            .iter()
            .map(|(label, detail)| (label.as_str(), detail.as_deref().unwrap_or_default()))
            .collect();
        assert_eq!(columns, expected, "{}", server.name);
        let indexes = labels_in(&orders.child(NodeKind::Folder(FolderKind::Indexes), "indexes"));
        assert_eq!(
            indexes,
            [("by_total".to_owned(), Some("on total".to_owned()))],
            "{}",
            server.name
        );

        let mut definitions = Vec::new();
        for path in [
            folder(FolderKind::Types).child(NodeKind::UserType, "address"),
            folder(FolderKind::Functions).child(NodeKind::Function, "plus(int, int)"),
            folder(FolderKind::Functions).child(NodeKind::Function, "twice(int)"),
            folder(FolderKind::Aggregates).child(NodeKind::Aggregate, "total(int)"),
            orders.clone(),
        ] {
            let definition = connection.definition(&path).await.expect("a definition");
            definitions.push(definition);
        }
        let keyspace_definition = connection.definition(&root).await.expect("the keyspace");
        assert!(
            keyspace_definition.starts_with("CREATE KEYSPACE"),
            "{}: {keyspace_definition}",
            server.name
        );
        assert!(
            !keyspace_definition.contains("CREATE TABLE"),
            "{keyspace_definition}"
        );

        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
        run(&mut connection, keyspace_definition.trim_end_matches(';')).await;
        for definition in &definitions {
            for statement in definition.split(";\n").map(str::trim) {
                let statement = statement.trim_end_matches(';');
                if !statement.is_empty() {
                    run(&mut connection, statement).await;
                }
            }
        }
        let again = conformance::walk_catalog(&mut connection, 5).await;
        assert_eq!(
            again.len(),
            nodes.len(),
            "{}: the definitions made the keyspace again",
            server.name
        );
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}

/// A table is described with its key, partition then clustering columns, and each column's CQL
/// type; the grid's statements, written from that description, edit a value of every type and
/// read it back as it was typed, add a row and delete it. A view isn't a table to edit.
#[tokio::test]
async fn edits_rows_by_their_primary_key() {
    use dbrover_core::{Identifier, RelationName};
    use dbrover_sql::{ColumnValue, RowSql};

    let _schema = SCHEMA.lock().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keyspace = server.keyspace(&mut connection, "edits").await;
        run(
            &mut connection,
            &format!("CREATE TYPE {keyspace}.address (city text, zip int)"),
        )
        .await;
        let columns = [
            ("a_int", "int", "1", "-42"),
            ("a_bigint", "bigint", "1", "9223372036854775807"),
            ("a_decimal", "decimal", "1.0", "12.50"),
            ("a_double", "double", "1.0", "2.5"),
            ("a_boolean", "boolean", "false", "true"),
            ("a_text", "text", "'a'", "it's"),
            ("a_date", "date", "'2026-01-01'", "2026-11-01"),
            (
                "a_timestamp",
                "timestamp",
                "'2026-01-01 00:00:00+0000'",
                "2026-11-01 09:30:00.123+0000",
            ),
            (
                "a_uuid",
                "uuid",
                "123e4567-e89b-12d3-a456-426614174000",
                "00000000-0000-0000-0000-000000000001",
            ),
            ("a_blob", "blob", "0x00", "0xcafe"),
            ("a_duration", "duration", "1h", "1d2h"),
            ("a_inet", "inet", "'10.0.0.1'", "::1"),
            ("a_list", "list<int>", "[1]", "[3, 2]"),
            ("a_map", "map<text, int>", "{'a': 1}", r#"{"b": 2, "c": 3}"#),
            (
                "a_address",
                "frozen<address>",
                "{city: 'a', zip: 1}",
                r#"{"city": "O'Hare", "zip": 60666}"#,
            ),
        ];
        let definitions: Vec<String> = columns
            .iter()
            .map(|(name, column_type, ..)| format!("{name} {column_type}"))
            .collect();
        run(
            &mut connection,
            &format!(
                "CREATE TABLE {keyspace}.t (p text, c int, {}, PRIMARY KEY (p, c))",
                definitions.join(", ")
            ),
        )
        .await;
        let names: Vec<&str> = columns.iter().map(|(name, ..)| *name).collect();
        let initial: Vec<&str> = columns.iter().map(|(_, _, initial, _)| *initial).collect();
        run(
            &mut connection,
            &format!(
                "INSERT INTO {keyspace}.t (p, c, {}) VALUES ('x', 1, {})",
                names.join(", "),
                initial.join(", ")
            ),
        )
        .await;

        let relation = |schema: Option<&str>, name: &str| RelationName {
            schema: schema.map(|schema| Identifier {
                name: schema.to_owned(),
                quoted: false,
            }),
            name: Identifier {
                name: name.to_owned(),
                quoted: false,
            },
        };
        let table = connection
            .describe_table(&relation(Some(&keyspace), "T"))
            .await
            .expect("describe")
            .expect("a table");
        assert_eq!(table.primary_key, ["p", "c"], "{}", server.name);
        assert_eq!(
            table
                .columns
                .iter()
                .find(|column| column.name == "a_map")
                .map(|column| column.data_type.as_str()),
            Some("map<text, int>")
        );

        let sql = RowSql::for_table(Dialect::Cassandra, &table);
        let key_values = [Some("x".to_owned()), Some("1".to_owned())];
        let key = [
            ColumnValue {
                column: "p",
                value: &key_values[0],
            },
            ColumnValue {
                column: "c",
                value: &key_values[1],
            },
        ];
        for (name, _, _, edited) in columns {
            let value = Some(edited.to_owned());
            let update = sql.update(
                &[ColumnValue {
                    column: name,
                    value: &value,
                }],
                &key,
            );
            run(&mut connection, &update).await;
            let read = run(
                &mut connection,
                &format!("SELECT {name} FROM {keyspace}.t WHERE p = 'x' AND c = 1"),
            )
            .await;
            assert_eq!(texts(&read), [[edited]], "{}: {update}", server.name);
        }

        let added = [
            Some("y".to_owned()),
            Some("2".to_owned()),
            Some("[7]".to_owned()),
        ];
        let insert = sql.insert(&[
            ColumnValue {
                column: "p",
                value: &added[0],
            },
            ColumnValue {
                column: "c",
                value: &added[1],
            },
            ColumnValue {
                column: "a_list",
                value: &added[2],
            },
        ]);
        run(&mut connection, &insert).await;
        let count = format!("SELECT count(*) FROM {keyspace}.t");
        assert_eq!(texts(&run(&mut connection, &count).await), [["2"]]);
        run(
            &mut connection,
            &sql.delete(&[
                ColumnValue {
                    column: "p",
                    value: &added[0],
                },
                ColumnValue {
                    column: "c",
                    value: &added[1],
                },
            ]),
        )
        .await;
        assert_eq!(texts(&run(&mut connection, &count).await), [["1"]]);

        run(
            &mut connection,
            &format!(
                "CREATE MATERIALIZED VIEW {keyspace}.by_c AS SELECT p, c FROM {keyspace}.t \
                 WHERE c IS NOT NULL AND p IS NOT NULL PRIMARY KEY (c, p)"
            ),
        )
        .await;
        assert_eq!(
            connection
                .describe_table(&relation(Some(&keyspace), "by_c"))
                .await
                .expect("describe"),
            None
        );
        run(&mut connection, &format!("USE {keyspace}")).await;
        assert!(
            connection
                .describe_table(&relation(None, "t"))
                .await
                .expect("describe")
                .is_some(),
            "an unqualified name resolves in the session's keyspace"
        );
        run(&mut connection, &format!("DROP KEYSPACE {keyspace}")).await;
    }
}
