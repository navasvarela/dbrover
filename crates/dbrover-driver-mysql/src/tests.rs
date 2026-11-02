use dbrover_core::conformance::{self, UnquotedNames};
use dbrover_core::{FolderKind, NodeKind, ServerConfig, TlsMode};

use super::*;

/// Point the tests at servers, e.g. `mysql://dbrover:dbrover@localhost:55306/dbrover` from the
/// repository's `docker-compose.yml` (see `test-servers.env`). Each test runs against every
/// server that is set and is skipped when none is, so `cargo test` works offline.
const TEST_URL_VARS: &[&str] = &["DBROVER_TEST_MYSQL_URL", "DBROVER_TEST_MARIADB_URL"];

/// `SLEEP()` alone in a statement returns 1 when killed instead of failing, so it sits in a
/// condition, where a kill makes the whole statement fail.
const LONG_RUNNING: &str = "SELECT 1 FROM DUAL WHERE SLEEP(60) = 0";

/// Held by every test that talks to the test server. They share one database, and the catalog
/// walk asked for the definition of a view that another test (the `describe_table` check) had
/// just dropped, which failed CI now and then. Tests that only need no server don't take it.
static SHARED_DATABASE: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

struct TestServer {
    variable: &'static str,
    config: MysqlConfig,
    password: Option<Password>,
}

fn test_servers() -> Vec<TestServer> {
    let servers: Vec<TestServer> = TEST_URL_VARS
        .iter()
        .filter_map(|variable| {
            let url = std::env::var(variable).ok()?;
            Some(parse_url(variable, &url))
        })
        .collect();
    if servers.is_empty() {
        eprintln!("skipping: none of {TEST_URL_VARS:?} is set");
    }
    servers
}

/// Reads `mysql://user:password@host:port/database`.
fn parse_url(variable: &'static str, url: &str) -> TestServer {
    let rest = url.strip_prefix("mysql://").expect("mysql:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (address, database) = rest.split_once('/').expect("/database in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    TestServer {
        variable,
        config: MysqlConfig {
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
    async fn connect(&self) -> MysqlConnection {
        self.connect_with(TlsMode::default())
            .await
            .unwrap_or_else(|error| panic!("{}: {error:?}", self.variable))
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<MysqlConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

/// One text value from the first row of `sql`.
async fn first_value(connection: &MysqlConnection, sql: &str) -> Option<String> {
    connection
        .conn
        .lock()
        .await
        .query_first::<mysql_async::Row, _>(sql)
        .await
        .unwrap()
        .and_then(|row| row.get_opt::<Option<String>, _>(row.len() - 1))
        .and_then(Result::ok)
        .flatten()
}

/// The test servers' self-signed certificates can't be verified, so this checks the route
/// alone; TLS against the server's name is the PostgreSQL and ClickHouse drivers' test.
#[tokio::test]
async fn connects_through_a_tunnel() {
    let _shared = SHARED_DATABASE.lock().await;
    for server in test_servers() {
        let address = format!(
            "{}:{}",
            server.config.server.host,
            server.config.server.port_number().unwrap()
        );
        let route = Route::Tunnel(conformance::forwarder(&address).await);

        let connection = connect(&server.config, server.password.as_ref(), route)
            .await
            .unwrap_or_else(|error| panic!("{}: {error:?}", server.variable));

        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn conforms_to_the_connection_lifecycle() {
    let _shared = SHARED_DATABASE.lock().await;
    for server in test_servers() {
        let connection = server.connect().await;

        let product = connection.server_info().product.clone();
        assert!(
            ["MySQL", "MariaDB"].contains(&product.as_str()),
            "{product}"
        );
        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

/// What `KILL`, or a server restart, looks like to the client.
#[tokio::test]
async fn reports_a_session_the_server_ended_as_lost() {
    let _shared = SHARED_DATABASE.lock().await;
    for server in test_servers() {
        let connection = server.connect().await;
        let killer = server.connect().await;
        let id = connection.cancel.connection_id;

        killer
            .conn
            .lock()
            .await
            .query_drop(format!("KILL {id}"))
            .await
            .unwrap();
        let sql = format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}");
        conformance::wait_until("the session ending", || async {
            first_value(&killer, &sql).await.as_deref() == Some("0")
        })
        .await;

        conformance::check_lost(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn wrong_password_is_a_connect_error() {
    let _shared = SHARED_DATABASE.lock().await;
    for server in test_servers() {
        let result = connect(
            &server.config,
            Some(&Password::new("definitely wrong")),
            Route::Direct,
        )
        .await;

        conformance::check_connect_failure(result);
    }
}

#[tokio::test]
async fn unreachable_server_is_a_connect_error() {
    let config = MysqlConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_owned(),
            // Port 1 (tcpmux) is reserved and has no listener on any normal machine, so the
            // connection is refused immediately instead of timing out.
            port: 1.into(),
            database: "mysql".to_owned(),
            user: "root".to_owned(),
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
    for server in test_servers() {
        let mut connection = server.connect().await;
        let names = match first_value(&connection, "SELECT @@lower_case_table_names").await {
            Some(setting) if setting != "0" => UnquotedNames::IgnoreCase,
            _ => UnquotedNames::ExactCase,
        };

        conformance::check_execution(&mut connection, conformance::Spelling::STANDARD).await;
        conformance::check_transactions(&mut connection, conformance::Spelling::STANDARD).await;
        conformance::check_describe_table(&mut connection, names, conformance::Spelling::STANDARD)
            .await;
        conformance::check_validate(&mut connection, conformance::Spelling::STANDARD).await;
        conformance::check_cancel(&mut connection, LONG_RUNNING).await;

        Box::new(connection).close().await.unwrap();
    }
}

/// One object of every kind the explorer shows. Run one statement at a time: a procedure's
/// body holds semicolons.
const FIXTURE: &[&str] = &[
    "DROP EVENT IF EXISTS dbrover_cleanup",
    "DROP VIEW IF EXISTS dbrover_big_orders",
    "DROP FUNCTION IF EXISTS dbrover_add",
    "DROP PROCEDURE IF EXISTS dbrover_touch",
    "DROP TABLE IF EXISTS dbrover_orders",
    "CREATE TABLE dbrover_orders (
        id INT AUTO_INCREMENT PRIMARY KEY,
        customer VARCHAR(50) NOT NULL,
        total DECIMAL(10, 2),
        CONSTRAINT dbrover_total_positive CHECK (total >= 0),
        INDEX dbrover_orders_customer (customer)
    )",
    "CREATE TRIGGER dbrover_orders_touch BEFORE UPDATE ON dbrover_orders
        FOR EACH ROW SET NEW.total = NEW.total",
    "CREATE VIEW dbrover_big_orders AS SELECT id, total FROM dbrover_orders WHERE total > 100",
    "CREATE FUNCTION dbrover_add(a INT, b INT) RETURNS INT DETERMINISTIC RETURN a + b",
    "CREATE PROCEDURE dbrover_touch() BEGIN SELECT 1; SELECT 2; END",
    "CREATE EVENT dbrover_cleanup ON SCHEDULE EVERY 1 DAY
        DO DELETE FROM dbrover_orders WHERE total < 0",
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
    for server in test_servers() {
        let mut connection = server.connect().await;
        for statement in FIXTURE {
            connection
                .conn
                .lock()
                .await
                .query_drop(*statement)
                .await
                .unwrap_or_else(|error| panic!("{}: {statement}: {error}", server.variable));
        }

        let nodes = conformance::walk_catalog(&mut connection, 5).await;
        conformance::check_invalid_path(&mut connection).await;

        let schema = CatalogPath::root().child(NodeKind::Schema, &server.config.server.database);
        let root = labels_under(&nodes, &CatalogPath::root());
        assert!(
            root.contains(&server.config.server.database.as_str()),
            "{root:?}"
        );
        let information_schema = nodes
            .iter()
            .find(|node| {
                node.path == CatalogPath::root().child(NodeKind::Schema, "information_schema")
            })
            .expect("information_schema is listed");
        assert!(information_schema.system);

        let orders = folder(&schema, FolderKind::Tables).child(NodeKind::Table, "dbrover_orders");
        let contains: [(CatalogPath, &str); 6] = [
            (folder(&schema, FolderKind::Tables), "dbrover_orders"),
            (folder(&schema, FolderKind::Views), "dbrover_big_orders"),
            (folder(&schema, FolderKind::Functions), "dbrover_add"),
            (folder(&schema, FolderKind::Procedures), "dbrover_touch"),
            (folder(&schema, FolderKind::Events), "dbrover_cleanup"),
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
        let exactly: [(CatalogPath, &[&str]); 3] = [
            (
                folder(&orders, FolderKind::Columns),
                &["id", "customer", "total"],
            ),
            (
                folder(&orders, FolderKind::Indexes),
                &["dbrover_orders_customer", "PRIMARY"],
            ),
            (
                folder(&orders, FolderKind::Constraints),
                &["dbrover_total_positive", "PRIMARY"],
            ),
        ];
        for (parent, labels) in exactly {
            assert_eq!(labels_under(&nodes, &parent), labels, "under {parent}");
        }

        let definitions = [
            (orders.clone(), "CREATE TABLE `dbrover_orders`"),
            (
                folder(&schema, FolderKind::Views).child(NodeKind::View, "dbrover_big_orders"),
                "`dbrover_big_orders`",
            ),
            (
                folder(&schema, FolderKind::Functions).child(NodeKind::Function, "dbrover_add"),
                "a + b",
            ),
            (
                folder(&schema, FolderKind::Procedures).child(NodeKind::Procedure, "dbrover_touch"),
                "SELECT 2",
            ),
            (
                folder(&schema, FolderKind::Events).child(NodeKind::Event, "dbrover_cleanup"),
                "DELETE FROM dbrover_orders",
            ),
            (
                folder(&orders, FolderKind::Triggers)
                    .child(NodeKind::Trigger, "dbrover_orders_touch"),
                "SET NEW.total",
            ),
        ];
        for (path, expected) in definitions {
            let definition = connection.definition(&path).await.unwrap();
            assert!(definition.contains(expected), "{path}: {definition}");
        }

        Box::new(connection).close().await.unwrap();
    }
}

/// Both test servers make a self-signed certificate on first start, as MySQL 8 and MariaDB
/// 11.4 do by default. Connecting with TLS also proves `mysql_async`'s rustls connector finds
/// a crypto provider: it takes rustls's process-wide one, which `connect` installs.
#[tokio::test]
async fn encrypts_as_the_tls_mode_says() {
    let _shared = SHARED_DATABASE.lock().await;
    for server in test_servers() {
        for (tls, encrypted) in [
            (TlsMode::Disable, false),
            (TlsMode::Prefer, true),
            (TlsMode::Require, true),
        ] {
            let connection = server
                .connect_with(tls.clone())
                .await
                .unwrap_or_else(|error| panic!("{} {tls:?}: {error:?}", server.variable));

            let cipher = first_value(&connection, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await;
            assert_eq!(
                cipher.is_some_and(|cipher| !cipher.is_empty()),
                encrypted,
                "{} {tls:?}",
                server.variable
            );
            Box::new(connection).close().await.unwrap();
        }

        // The certificate isn't signed by a public authority.
        let unverified = server
            .connect_with(TlsMode::VerifyFull { ca_file: None })
            .await;
        conformance::check_connect_failure(unverified);
    }
}

#[test]
fn tells_mysql_and_mariadb_apart_by_version() {
    let cases = [
        ("8.4.3", "MySQL", "8.4.3"),
        ("11.4.4-MariaDB-ubu2404", "MariaDB", "11.4.4"),
    ];
    for (version, product, number) in cases {
        let info = server_info(version.to_owned());
        assert_eq!(
            (info.product.as_str(), info.version.as_str()),
            (product, number)
        );
    }
}

/// IAM users sign in with `mysql_clear_password`, so a TLS mode that may fall back to plain
/// text would send the token in the clear: connecting stops before anything is sent.
#[tokio::test]
async fn a_cloud_sign_in_needs_tls_that_cant_fall_back() {
    for tls in [TlsMode::Disable, TlsMode::Prefer] {
        let config = MysqlConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_owned(),
                port: 9.into(),
                database: "app".to_owned(),
                user: "iam_user".to_owned(),
                tls: tls.clone(),
                ssh: None,
                password_command: None,
                cloud_sign_in: Some(dbrover_core::CloudSignIn::Aws {
                    profile: String::new(),
                    region: "eu-west-1".to_owned(),
                }),
            },
        };
        let token = Password::new("db.example:3306/?Action=connect");
        let refused = connect(&config, Some(&token), Route::Direct).await;
        let Err(error) = refused else {
            panic!("{tls:?} connected");
        };
        let message = dbrover_core::describe_error(&error);
        assert!(message.contains("needs TLS"), "{tls:?}: {message}");
    }
}
