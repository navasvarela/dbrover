use dbrover_core::conformance::{self, Spelling, UnquotedNames};
use dbrover_core::{FolderKind, NodeKind, ServerConfig, TlsMode};

use super::*;

/// Point the tests at a server, e.g. `sqlserver://sa:DbRover-Test-2026@localhost:51433/dbrover`
/// from the repository's `docker-compose.yml` (see `test-servers.env`). Each test is skipped
/// when it is unset, so `cargo test` works offline.
const TEST_URL_VAR: &str = "DBROVER_TEST_SQLSERVER_URL";

/// Waits far longer than the conformance check allows, without returning anything first, so
/// the cancel reaches a statement the server is still running.
const LONG_RUNNING: &str = "WAITFOR DELAY '00:01:00'";

/// Held by every test that talks to the test server. They share one database, and the catalog
/// walk asked for the definition of a view that another test (the `describe_table` check) had
/// just dropped, which failed CI now and then. Tests that only need no server don't take it.
static SHARED_DATABASE: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

struct TestServer {
    config: SqlServerConfig,
    password: Option<Password>,
}

fn test_server() -> Option<TestServer> {
    let Ok(url) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    Some(parse_url(&url))
}

/// Reads `sqlserver://user:password@host:port/database`.
fn parse_url(url: &str) -> TestServer {
    let rest = url.strip_prefix("sqlserver://").expect("sqlserver:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (address, database) = rest.split_once('/').expect("/database in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    TestServer {
        config: SqlServerConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: port.parse::<u16>().expect("numeric port").into(),
                database: database.to_owned(),
                user: user.to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        },
        password: Some(Password::new(password)),
    }
}

impl TestServer {
    async fn connect(&self) -> SqlServerConnection {
        self.connect_with(TlsMode::default())
            .await
            .unwrap_or_else(|error| panic!("{TEST_URL_VAR}: {error:?}"))
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<SqlServerConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

/// The first row of `sql`, as text.
async fn first_row(connection: &mut SqlServerConnection, sql: &str) -> Vec<Option<String>> {
    let mut cursor = connection.execute(sql).await.unwrap();
    let rows = cursor.next_page(1).await.unwrap();
    cursor.finish().await.unwrap();
    rows.into_iter().next().unwrap_or_default()
}

async fn run(connection: &mut SqlServerConnection, sql: &str) -> Option<u64> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {}", dbrover_core::describe_error(&error)));
    while !cursor.is_exhausted() {
        cursor.next_page(500).await.unwrap();
    }
    cursor.finish().await.unwrap()
}

/// The server's own certificate can't be verified, so this checks the route alone.
#[tokio::test]
async fn connects_through_a_tunnel() {
    let Some(server) = test_server() else { return };
    let address = format!(
        "{}:{}",
        server.config.server.host,
        server.config.server.port_number().unwrap()
    );
    let route = Route::Tunnel(conformance::forwarder(&address).await);

    let connection = connect(&server.config, server.password.as_ref(), route)
        .await
        .unwrap();

    conformance::check_lifecycle(Box::new(connection)).await;
}

/// What `KILL`, or a server restart, looks like to the client.
#[tokio::test]
async fn reports_a_session_the_server_ended_as_lost() {
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    let killer = server.connect().await;
    let spid = first_row(&mut connection, "SELECT CAST(@@SPID AS varchar(10))").await;
    let spid = spid.into_iter().next().flatten().unwrap();

    let killer = &tokio::sync::Mutex::new(killer);
    first_row(
        &mut *killer.lock().await,
        &format!("KILL {spid}; SELECT 'killed'"),
    )
    .await;
    let sql = &format!(
        "SELECT CAST(COUNT(*) AS varchar(10)) FROM sys.dm_exec_sessions WHERE session_id = {spid}"
    );
    conformance::wait_until("the session ending", || async move {
        first_row(&mut *killer.lock().await, sql).await == vec![Some("0".to_owned())]
    })
    .await;

    conformance::check_lost(Box::new(connection)).await;
}

#[tokio::test]
async fn conforms_to_the_connection_lifecycle() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let connection = server.connect().await;

    assert_eq!(connection.server_info().product, "SQL Server");
    assert!(
        connection.server_info().version.starts_with("16."),
        "{}",
        connection.server_info()
    );
    conformance::check_lifecycle(Box::new(connection)).await;
}

#[tokio::test]
async fn wrong_password_is_a_connect_error() {
    let _shared = SHARED_DATABASE.lock().await;
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
    let config = SqlServerConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_owned(),
            // Port 1 (tcpmux) is reserved and has no listener on any normal machine, so the
            // connection is refused immediately instead of timing out.
            port: 1.into(),
            database: "master".to_owned(),
            user: "sa".to_owned(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        },
    };

    conformance::check_connect_failure(connect(&config, None, Route::Direct).await);
}

#[tokio::test]
async fn runs_pages_cancels_and_reports_error_positions() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    conformance::check_execution(&mut connection, Spelling::SQL_SERVER).await;
    conformance::check_transactions(&mut connection, Spelling::SQL_SERVER).await;
    // The container's collation, SQL_Latin1_General_CP1_CI_AS, ignores case.
    conformance::check_describe_table(
        &mut connection,
        UnquotedNames::IgnoreCase,
        Spelling::SQL_SERVER,
    )
    .await;
    conformance::check_validate(&mut connection, Spelling::SQL_SERVER).await;
    conformance::check_cancel(&mut connection, LONG_RUNNING).await;

    Box::new(connection).close().await.unwrap();
}

/// Cancelling a result whose rows trickle in, a packet at a time with pauses between, still
/// stops between two rows. The client is then most often waiting halfway through a row for
/// the next packet, and dropping that read used to lose the half already read, leaving the
/// connection out of step with the server (7 runs in 8 against a container).
#[tokio::test]
async fn cancels_a_result_that_trickles_in() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    let handle = connection.cancel_handle();

    // Billions of combinations, of which one in 49,999 is a row: a few rows a millisecond.
    let mut cursor = connection
        .execute(
            "SELECT a.object_id, b.name FROM sys.all_objects a \
             CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c \
             WHERE (CAST(a.object_id AS bigint) + b.object_id + c.object_id) % 49999 = 0",
        )
        .await
        .unwrap();
    let first = cursor.next_page(100).await.unwrap();
    assert_eq!(first.len(), 100, "rows are arriving");
    // Cancelled while a page is being read, so a read is under way when the cancel arrives.
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        handle.cancel().await
    });
    let started = std::time::Instant::now();
    // The cancel ends the result: the page being read fails, or `finish` reports it.
    let outcome = async {
        while !cursor.is_exhausted() {
            cursor.next_page(100_000_000).await?;
        }
        cursor.finish().await
    }
    .await;
    canceller.await.unwrap().unwrap();
    assert!(
        matches!(outcome, Err(DriverError::Statement { .. })),
        "{outcome:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the result took {:?} to end",
        started.elapsed()
    );

    connection.ping().await.unwrap();
    assert_eq!(
        first_row(&mut connection, "SELECT 42").await,
        [Some("42".to_owned())]
    );
}

/// Cancelling while a page of rows is arriving stops between two rows and leaves the
/// connection usable, as cancelling a statement that hasn't answered yet does.
#[tokio::test]
async fn cancels_a_result_that_is_still_arriving() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    let handle = connection.cancel_handle();

    let mut cursor = connection
        .execute(
            "SELECT a.object_id, b.name FROM sys.all_objects a \
             CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c",
        )
        .await
        .unwrap();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        handle.cancel().await
    });
    // Far more rows than arrive in the time before the cancel.
    let read = cursor.next_page(100_000_000).await;
    assert!(
        matches!(read, Err(DriverError::Statement { .. })),
        "{:?}",
        read.map(|rows| rows.len())
    );
    canceller.await.unwrap().unwrap();
    let _ = cursor.finish().await;

    connection.ping().await.unwrap();
    assert_eq!(
        first_row(&mut connection, "SELECT 42").await,
        [Some("42".to_owned())]
    );
}

#[tokio::test]
async fn counts_changed_rows_and_shows_values_as_sql_server_does() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    run(&mut connection, "CREATE TABLE #counted (n int)").await;
    let inserted = run(&mut connection, "INSERT INTO #counted VALUES (1), (2), (3)").await;
    assert_eq!(inserted, Some(3));
    assert_eq!(
        run(&mut connection, "UPDATE #counted SET n = n + 1 WHERE n > 1").await,
        Some(2)
    );

    let values = first_row(
        &mut connection,
        "SELECT CAST(1 AS bit), CAST(12.5 AS decimal(10, 2)), CAST(3.5 AS money), \
         N'it''s', 0x1FA0, CAST('2026-10-24T07:28:23.930' AS datetime), \
         CAST('2026-10-24T07:28:23.9300000' AS datetime2), CAST('2026-10-24' AS date), \
         CAST('2026-10-24 07:28:00' AS smalldatetime), \
         CAST('2026-10-24 09:28:23 +02:00' AS datetimeoffset(0)), \
         CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier), NULL",
    )
    .await;
    let expected = [
        Some("1"),
        Some("12.50"),
        Some("3.5000"),
        Some("it's"),
        Some("0x1FA0"),
        Some("2026-10-24 07:28:23.930"),
        Some("2026-10-24 07:28:23.9300000"),
        Some("2026-10-24"),
        Some("2026-10-24 07:28:00"),
        Some("2026-10-24 09:28:23 +02:00"),
        Some("6F9619FF-8B86-D011-B42D-00C04FC964FF"),
        None,
    ];
    assert_eq!(
        values,
        expected.map(|value| value.map(str::to_owned)).to_vec()
    );
    Box::new(connection).close().await.unwrap();
}

/// The session options a client library sets: `"…"` is an identifier, and `= NULL` is never
/// true.
#[tokio::test]
async fn sets_the_session_options_client_libraries_set() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;

    let row = first_row(
        &mut connection,
        "SELECT \"name\" FROM (SELECT 'quoted' AS name) t \
         WHERE CASE WHEN NULL = NULL THEN 1 ELSE 0 END = 0",
    )
    .await;
    assert_eq!(row, [Some("quoted".to_owned())]);
}

/// One object of every kind the explorer shows, each created in a batch of its own: a view,
/// a procedure, a function and a trigger must each be alone in theirs.
const FIXTURE: &[&str] = &[
    "DROP SYNONYM IF EXISTS dbo.dbrover_orders_alias",
    "DROP SEQUENCE IF EXISTS dbo.dbrover_order_numbers",
    "DROP VIEW IF EXISTS dbo.dbrover_big_orders",
    "DROP FUNCTION IF EXISTS dbo.dbrover_add",
    "DROP PROCEDURE IF EXISTS dbo.dbrover_touch",
    "DROP TABLE IF EXISTS dbo.dbrover_orders",
    "CREATE TABLE dbo.dbrover_orders (
        id INT IDENTITY PRIMARY KEY,
        customer NVARCHAR(50) NOT NULL,
        total DECIMAL(10, 2) CONSTRAINT dbrover_total_positive CHECK (total >= 0),
        INDEX dbrover_orders_customer (customer)
    )",
    "CREATE TRIGGER dbo.dbrover_orders_touch ON dbo.dbrover_orders AFTER UPDATE AS
        SET NOCOUNT ON",
    "CREATE VIEW dbo.dbrover_big_orders AS SELECT id, total FROM dbo.dbrover_orders WHERE total > 100",
    "CREATE FUNCTION dbo.dbrover_add(@a INT, @b INT) RETURNS INT AS BEGIN RETURN @a + @b END",
    "CREATE PROCEDURE dbo.dbrover_touch AS SELECT 1; SELECT 2;",
    "CREATE SEQUENCE dbo.dbrover_order_numbers AS bigint START WITH 1",
    "CREATE SYNONYM dbo.dbrover_orders_alias FOR dbo.dbrover_orders",
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
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    let mut connection = server.connect().await;
    for statement in FIXTURE {
        run(&mut connection, statement).await;
    }

    let nodes = conformance::walk_catalog(&mut connection, 6).await;
    conformance::check_invalid_path(&mut connection).await;

    let database = CatalogPath::root().child(NodeKind::Database, &server.config.server.database);
    let master = nodes
        .iter()
        .find(|node| node.path == CatalogPath::root().child(NodeKind::Database, "master"))
        .expect("master is listed");
    assert!(master.system);
    let sys = nodes
        .iter()
        .find(|node| node.path == database.child(NodeKind::Schema, "sys"))
        .expect("sys is listed");
    assert!(sys.system);

    let schema = database.child(NodeKind::Schema, "dbo");
    let orders = folder(&schema, FolderKind::Tables).child(NodeKind::Table, "dbrover_orders");
    let contains: [(CatalogPath, &str); 7] = [
        (folder(&schema, FolderKind::Tables), "dbrover_orders"),
        (folder(&schema, FolderKind::Views), "dbrover_big_orders"),
        (folder(&schema, FolderKind::Functions), "dbrover_add"),
        (folder(&schema, FolderKind::Procedures), "dbrover_touch"),
        (
            folder(&schema, FolderKind::Sequences),
            "dbrover_order_numbers",
        ),
        (
            folder(&schema, FolderKind::Synonyms),
            "dbrover_orders_alias",
        ),
        (
            folder(&orders, FolderKind::Triggers),
            "dbrover_orders_touch",
        ),
    ];
    for (parent, label) in contains {
        assert!(
            labels_under(&nodes, &parent).contains(&label),
            "{label} under {parent}"
        );
    }
    assert_eq!(
        labels_under(&nodes, &folder(&orders, FolderKind::Columns)),
        ["id", "customer", "total"]
    );
    let details: Vec<Option<&str>> = nodes
        .iter()
        .filter(|node| node.path.parent().as_ref() == Some(&folder(&orders, FolderKind::Columns)))
        .map(|node| node.detail.as_deref())
        .collect();
    assert_eq!(
        details,
        [
            Some("int, not null"),
            Some("nvarchar(50), not null"),
            Some("decimal(10, 2)")
        ]
    );
    let indexes = labels_under(&nodes, &folder(&orders, FolderKind::Indexes));
    assert!(indexes.contains(&"dbrover_orders_customer"), "{indexes:?}");
    let constraints = labels_under(&nodes, &folder(&orders, FolderKind::Constraints));
    assert!(
        constraints.contains(&"dbrover_total_positive"),
        "{constraints:?}"
    );

    let definitions = [
        (
            folder(&schema, FolderKind::Views).child(NodeKind::View, "dbrover_big_orders"),
            "CREATE VIEW dbo.dbrover_big_orders",
        ),
        (
            folder(&schema, FolderKind::Functions).child(NodeKind::Function, "dbrover_add"),
            "RETURN @a + @b",
        ),
        (
            folder(&schema, FolderKind::Procedures).child(NodeKind::Procedure, "dbrover_touch"),
            "SELECT 2",
        ),
        (
            folder(&schema, FolderKind::Synonyms).child(NodeKind::Synonym, "dbrover_orders_alias"),
            "CREATE SYNONYM [dbo].[dbrover_orders_alias] FOR [dbo].[dbrover_orders];",
        ),
        (
            folder(&orders, FolderKind::Triggers).child(NodeKind::Trigger, "dbrover_orders_touch"),
            "SET NOCOUNT ON",
        ),
    ];
    for (path, expected) in definitions {
        let definition = connection.definition(&path).await.unwrap();
        assert!(definition.contains(expected), "{path}: {definition}");
    }

    Box::new(connection).close().await.unwrap();
}

/// SQL Server makes a self-signed certificate when it is given none, so every mode but
/// `verify-full` connects, and `encrypt_option` says whether the session is encrypted.
/// Connecting with TLS also proves ring was installed as rustls's process-wide provider.
#[tokio::test]
async fn encrypts_as_the_tls_mode_says() {
    let _shared = SHARED_DATABASE.lock().await;
    let Some(server) = test_server() else { return };
    for (tls, encrypted) in [
        (TlsMode::Disable, "FALSE"),
        (TlsMode::Prefer, "TRUE"),
        (TlsMode::Require, "TRUE"),
    ] {
        let mut connection = server
            .connect_with(tls.clone())
            .await
            .unwrap_or_else(|error| panic!("{tls:?}: {error:?}"));

        let row = first_row(
            &mut connection,
            "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID",
        )
        .await;
        assert_eq!(row, [Some(encrypted.to_owned())], "{tls:?}");
        Box::new(connection).close().await.unwrap();
    }

    // The certificate isn't signed by a public authority.
    let unverified = server
        .connect_with(TlsMode::VerifyFull { ca_file: None })
        .await;
    conformance::check_connect_failure(unverified);
}
