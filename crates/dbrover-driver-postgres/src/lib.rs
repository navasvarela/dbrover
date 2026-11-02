//! PostgreSQL driver for DbRover, built on `tokio-postgres`.
//!
//! Must run inside a tokio runtime: each connection spawns a task that drives the socket.

mod catalog;
mod execution;
mod roles;
mod table;
mod tls;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::future::poll_fn;
use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, Connection, DriverError,
    NodeKind, Password, PostgresConfig, Rejection, RelationName, ResultCursor, RoleDescription,
    Route, ServerInfo, TableConstraints, TableDescription, TableName, TransactionStatus,
};
use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, Client, Socket};
use tokio_postgres_rustls::MakeRustlsConnect;

pub use tls::TlsSetupError;

/// Shown in `pg_stat_activity`, so a DBA can tell DbRover's sessions apart.
const APPLICATION_NAME: &str = "dbrover";
const PRODUCT: &str = "PostgreSQL";

/// Opens a connection, encrypted as `config.server.tls` says.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used (a missing CA file, say), the
/// server is unreachable, the TLS handshake or certificate check fails, the server rejects
/// the credentials, or the database doesn't exist.
///
/// `route` says whether to connect to the server itself or through an SSH tunnel.
pub async fn connect(
    config: &PostgresConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<PostgresConnection, DriverError> {
    let tls = tls::connector(&config.server.tls)
        .map_err(|error| DriverError::Connect(Box::new(error)))?;
    let server = Server {
        config: config.clone(),
        password: password.cloned(),
        tls,
        route,
    };
    let primary = server.open(&config.server.database).await?;
    let version = server_version(&primary.client)
        .await
        .map_err(connect_error)?;

    Ok(PostgresConnection {
        server,
        primary,
        others: HashMap::new(),
        server_info: ServerInfo {
            product: PRODUCT.to_owned(),
            version,
        },
    })
}

async fn server_version(client: &Client) -> Result<String, tokio_postgres::Error> {
    client
        .query_one("SHOW server_version", &[])
        .await?
        .try_get(0)
}

fn connect_error(error: tokio_postgres::Error) -> DriverError {
    DriverError::Connect(Box::new(error))
}

/// A client error, as [`DriverError::Lost`] when it means the session is over.
fn connection_error(error: tokio_postgres::Error) -> DriverError {
    if is_lost(&error) {
        DriverError::Lost(Box::new(error))
    } else {
        DriverError::Connection(Box::new(error))
    }
}

/// Whether `error` means the session is over: the socket closed or failed, the server ended
/// the session (an administrator's `pg_terminate_backend`, a shutdown or a crash), or it
/// reported a connection exception (SQLSTATE class 08).
fn is_lost(error: &tokio_postgres::Error) -> bool {
    use tokio_postgres::error::SqlState;

    if error.is_closed() {
        return true;
    }
    match error.code() {
        Some(code) => {
            code.code().starts_with("08")
                || [
                    SqlState::ADMIN_SHUTDOWN,
                    SqlState::CRASH_SHUTDOWN,
                    SqlState::CANNOT_CONNECT_NOW,
                ]
                .contains(code)
        }
        None => {
            std::error::Error::source(error).is_some_and(|source| source.is::<std::io::Error>())
        }
    }
}

/// A client bound to one database, and the task that drives its socket.
struct DatabaseClient {
    client: Client,
    /// Finishes once `client` is dropped and the server has been told the session is over.
    connection_task: JoinHandle<()>,
}

/// What opening a client needs, kept for the connection's lifetime to open one for each
/// other database the explorer browses.
struct Server {
    config: PostgresConfig,
    /// Kept in memory, for the session's lifetime only.
    password: Option<Password>,
    tls: MakeRustlsConnect,
    route: Route,
}

impl Server {
    async fn open(&self, database: &str) -> Result<DatabaseClient, DriverError> {
        let server = &self.config.server;
        let mut pg_config = tokio_postgres::Config::new();
        // Through a tunnel, `hostaddr` is where the socket goes while `host` stays the name
        // TLS checks the certificate against, as libpq does with both set.
        pg_config.host(&server.host);
        match self.route {
            Route::Direct => pg_config.port(server.port_number()?),
            Route::Tunnel(local) => pg_config.hostaddr(local.ip()).port(local.port()),
        };
        pg_config
            .dbname(database)
            .user(&server.user)
            .application_name(APPLICATION_NAME)
            .ssl_mode(tls::ssl_mode(&server.tls));
        if let Some(password) = &self.password {
            pg_config.password(password.expose());
        }

        let (client, connection) = pg_config
            .connect(self.tls.clone())
            .await
            .map_err(connect_error)?;
        let connection_task = tokio::spawn(drive_connection(connection, database.to_owned()));
        Ok(DatabaseClient {
            client,
            connection_task,
        })
    }
}

impl DatabaseClient {
    async fn close(self) -> Result<(), JoinError> {
        drop(self.client);
        self.connection_task.await
    }
}

/// Drives a connection's socket until the client is dropped, logging server notices (from
/// `RAISE NOTICE`, `VACUUM VERBOSE`, "relation already exists, skipping", …) on the way.
///
/// The connection future must be polled through `poll_message` for this: awaiting it
/// directly, as the tokio-postgres examples do, silently drops every notice.
async fn drive_connection(
    mut connection: tokio_postgres::Connection<Socket, TlsStream>,
    database: String,
) {
    while let Some(message) = poll_fn(|cx| connection.poll_message(cx)).await {
        match message {
            Ok(AsyncMessage::Notice(notice)) => {
                tracing::info!(database, "{}: {}", notice.severity(), notice.message());
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(database, error = %error, "PostgreSQL connection closed with an error");
                return;
            }
        }
    }
}

/// What a connection's socket carries: TLS or, with `disable` or a `prefer` the server
/// declined, the bare socket.
type TlsStream = <MakeRustlsConnect as MakeTlsConnect<Socket>>::Stream;

/// A session with a PostgreSQL server.
///
/// PostgreSQL binds each connection to one database, but the explorer lists every database
/// on the server. Browsing a database other than the configured one opens an extra
/// connection to it on first use, kept until the session closes.
pub struct PostgresConnection {
    server: Server,
    primary: DatabaseClient,
    others: HashMap<String, DatabaseClient>,
    server_info: ServerInfo,
}

impl PostgresConnection {
    /// The client for the database a catalog path belongs to: the one named by the path's
    /// first segment, or the configured database for the root and for malformed paths (which
    /// the catalog then rejects without ever opening a connection for them).
    async fn client_for(&mut self, path: &CatalogPath) -> Result<&Client, DriverError> {
        let database = match path.segments().first() {
            Some(segment)
                if segment.kind == NodeKind::Database
                    && segment.name != self.server.config.server.database =>
            {
                segment.name.as_str()
            }
            _ => return Ok(&self.primary.client),
        };
        match self.others.entry(database.to_owned()) {
            Entry::Occupied(entry) => Ok(&entry.into_mut().client),
            Entry::Vacant(entry) => {
                let client = self.server.open(database).await?;
                tracing::info!(
                    database,
                    "opened a connection for browsing another database"
                );
                Ok(&entry.insert(client).client)
            }
        }
    }
}

#[async_trait]
impl Connection for PostgresConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.primary
            .client
            .simple_query("SELECT 1")
            .await
            .map(drop)
            .map_err(connection_error)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let client = self.client_for(path).await?;
        catalog::children(client, path).await
    }

    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let client = self.client_for(path).await?;
        catalog::children_page(client, path, query).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        let client = self.client_for(path).await?;
        catalog::definition(client, path).await
    }

    /// Always runs on the configured database. The console's connection is its own session,
    /// so there is no other database to pick.
    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.primary.client, sql).await
    }

    /// Roles belong to the server, but what the role owns is counted in the configured
    /// database, where the helpers' statements run.
    async fn describe_role(&mut self, path: &CatalogPath) -> Result<RoleDescription, DriverError> {
        roles::describe(&self.primary.client, path).await
    }

    /// Always on the configured database, the one consoles run on.
    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        table::describe_table(&self.primary.client, table).await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        table::describe_constraints(&self.primary.client, table).await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        execution::validate(&self.primary.client, sql).await
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        execution::transaction_status(&self.primary.client).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(execution::PostgresCancel {
            token: self.primary.client.cancel_token(),
            tls: self.server.tls.clone(),
        })
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        let Self {
            primary, others, ..
        } = *self;
        let mut first_error = None;
        for client in std::iter::once(primary).chain(others.into_values()) {
            if let Err(error) = client.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), |error| {
            Err(DriverError::Connection(Box::new(error)))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    use dbrover_core::{FolderKind, ServerConfig, TlsMode, conformance};
    use tokio_postgres::config::Host;

    use super::*;

    /// Points the tests at a server, e.g. `postgres://dbrover:dbrover@localhost:55432/dbrover`
    /// from the repository's `docker-compose.yml`. Tests that need a server are skipped when
    /// it is unset, so `cargo test` works offline.
    const TEST_URL_VAR: &str = "DBROVER_TEST_POSTGRES_URL";
    /// Points at a server that refuses unencrypted connections, the `postgres-tls` service in
    /// `docker-compose.yml`, e.g. `postgres://dbrover:dbrover@localhost:55443/dbrover`.
    const TLS_TEST_URL_VAR: &str = "DBROVER_TEST_POSTGRES_TLS_URL";

    fn test_server() -> Option<(PostgresConfig, Option<Password>)> {
        server_from(TEST_URL_VAR)
    }

    /// The TLS-only server, connected to with `tls`.
    fn tls_test_server(tls: TlsMode) -> Option<(PostgresConfig, Option<Password>)> {
        server_from(TLS_TEST_URL_VAR).map(|(config, password)| (with_tls(config, tls), password))
    }

    fn with_tls(config: PostgresConfig, tls: TlsMode) -> PostgresConfig {
        PostgresConfig {
            server: ServerConfig {
                tls,
                ..config.server
            },
        }
    }

    /// The authority that signed the TLS server's certificate.
    fn test_ca_file() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
    }

    fn server_from(variable: &str) -> Option<(PostgresConfig, Option<Password>)> {
        let Ok(url) = std::env::var(variable) else {
            eprintln!("skipping: {variable} is not set");
            return None;
        };
        let parsed = tokio_postgres::Config::from_str(&url).expect("valid test URL");
        let host = match parsed.get_hosts().first() {
            Some(Host::Tcp(host)) => host.clone(),
            other => panic!("test URL needs a TCP host, got {other:?}"),
        };
        let config = PostgresConfig {
            server: ServerConfig {
                host,
                port: parsed
                    .get_ports()
                    .first()
                    .copied()
                    .unwrap_or(PostgresConfig::DEFAULT_PORT)
                    .into(),
                database: parsed
                    .get_dbname()
                    .expect("database in test URL")
                    .to_owned(),
                user: parsed.get_user().expect("user in test URL").to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        };
        let password = parsed
            .get_password()
            .map(|bytes| Password::new(String::from_utf8_lossy(bytes)));
        Some((config, password))
    }

    /// What an administrator's `pg_terminate_backend`, or a server restart, looks like to
    /// the client.
    #[tokio::test]
    async fn reports_a_session_the_server_ended_as_lost() {
        let Some((config, password)) = test_server() else {
            return;
        };
        let connection = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();
        let killer = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();
        let pid: i32 = connection
            .primary
            .client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);

        killer
            .primary
            .client
            .execute("SELECT pg_terminate_backend($1)", &[&pid])
            .await
            .unwrap();
        conformance::wait_until("the session ending", || async {
            let sessions: i64 = killer
                .primary
                .client
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity WHERE pid = $1",
                    &[&pid],
                )
                .await
                .unwrap()
                .get(0);
            sessions == 0
        })
        .await;

        conformance::check_lost(Box::new(connection)).await;
    }

    #[tokio::test]
    async fn conforms_to_the_connection_lifecycle() {
        let Some((config, password)) = test_server() else {
            return;
        };

        let connection = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();

        assert_eq!(connection.server_info().product, PRODUCT);
        conformance::check_lifecycle(Box::new(connection)).await;
    }

    #[tokio::test]
    async fn wrong_password_is_a_connect_error() {
        let Some((config, _)) = test_server() else {
            return;
        };

        let result = connect(
            &config,
            Some(&Password::new("definitely wrong")),
            Route::Direct,
        )
        .await;

        conformance::check_connect_failure(result);
    }

    #[tokio::test]
    async fn unreachable_server_is_a_connect_error() {
        let config = PostgresConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_owned(),
                // Port 1 (tcpmux) is reserved and has no listener on any normal machine, so the
                // connection is refused immediately instead of timing out.
                port: 1.into(),
                database: "postgres".to_owned(),
                user: "postgres".to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        };

        conformance::check_connect_failure(connect(&config, None, Route::Direct).await);
    }

    /// Whether the session's own connection is encrypted, as the server sees it.
    async fn is_encrypted(connection: &PostgresConnection) -> bool {
        connection
            .primary
            .client
            .query_one(
                "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
                &[],
            )
            .await
            .unwrap()
            .get(0)
    }

    /// Through a tunnel the socket goes to a local port while TLS still checks the name the
    /// server was given: the test certificate names `localhost` and `127.0.0.1`, so the same
    /// server under another name is refused even though the tunnel's end is `127.0.0.1`.
    #[tokio::test]
    async fn connects_through_a_tunnel_checking_tls_against_the_servers_name() {
        let verified = TlsMode::VerifyFull {
            ca_file: Some(test_ca_file()),
        };
        let Some((config, password)) = tls_test_server(verified) else {
            return;
        };
        let server = format!(
            "{}:{}",
            config.server.host,
            config.server.port_number().unwrap()
        );
        let route = Route::Tunnel(conformance::forwarder(&server).await);

        let mut connection = connect(&config, password.as_ref(), route).await.unwrap();
        assert!(is_encrypted(&connection).await);
        // Browsing another database opens a connection of its own, through the tunnel too.
        let databases = connection.children(&CatalogPath::root()).await.unwrap();
        let other = databases
            .iter()
            .find(|node| node.label == "postgres")
            .expect("the postgres database");
        connection.children(&other.path).await.unwrap();
        Box::new(connection).close().await.unwrap();

        let mut renamed = config.clone();
        renamed.server.host = "db.invalid".to_owned();
        conformance::check_connect_failure(connect(&renamed, password.as_ref(), route).await);
    }

    #[tokio::test]
    async fn encrypts_when_the_server_requires_it() {
        let modes = [
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull {
                ca_file: Some(test_ca_file()),
            },
        ];
        for tls in modes {
            let Some((config, password)) = tls_test_server(tls.clone()) else {
                return;
            };

            let connection = connect(&config, password.as_ref(), Route::Direct)
                .await
                .unwrap_or_else(|error| panic!("{tls:?}: {error:?}"));

            assert!(is_encrypted(&connection).await, "{tls:?}");
            Box::new(connection).close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn runs_browses_and_cancels_over_tls() {
        let Some((config, password)) = tls_test_server(TlsMode::Require) else {
            return;
        };
        let mut connection = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();

        conformance::check_execution(&mut connection, conformance::Spelling::STANDARD).await;
        // Another database's connection and the cancel request are connections of their own,
        // which the server refuses unless they are encrypted too.
        let other = CatalogPath::root().child(NodeKind::Database, "postgres");
        assert!(!connection.children(&other).await.unwrap().is_empty());
        conformance::check_cancel(&mut connection, "SELECT pg_sleep(60)").await;

        conformance::check_lifecycle(Box::new(connection)).await;
    }

    #[tokio::test]
    async fn refuses_unencrypted_or_unverified_connections_where_they_are_not_allowed() {
        let cases = [
            // The TLS-only server rejects a plain connection.
            (TLS_TEST_URL_VAR, TlsMode::Disable),
            // Its certificate isn't signed by a public authority.
            (TLS_TEST_URL_VAR, TlsMode::VerifyFull { ca_file: None }),
            // The plain server offers no TLS to require.
            (TEST_URL_VAR, TlsMode::Require),
        ];
        for (variable, tls) in cases {
            let Some((config, password)) = server_from(variable) else {
                continue;
            };
            let config = with_tls(config, tls);

            conformance::check_connect_failure(
                connect(&config, password.as_ref(), Route::Direct).await,
            );
        }
    }

    #[tokio::test]
    async fn prefer_falls_back_to_plain_text_without_server_tls() {
        let Some((config, password)) = test_server() else {
            return;
        };
        assert_eq!(config.server.tls, TlsMode::Prefer);

        let connection = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();

        assert!(!is_encrypted(&connection).await);
        Box::new(connection).close().await.unwrap();
    }

    const FIXTURE_SCHEMA: &str = "dbrover_catalog_test";
    /// One object of every kind the explorer shows, in a schema of its own.
    const FIXTURE: &str = "
        DROP SCHEMA IF EXISTS dbrover_catalog_test CASCADE;
        CREATE SCHEMA dbrover_catalog_test;
        SET search_path TO dbrover_catalog_test;
        CREATE TABLE orders (
            id serial PRIMARY KEY,
            customer text NOT NULL,
            total numeric(10, 2) CHECK (total >= 0)
        );
        CREATE INDEX orders_customer ON orders (customer);
        CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
        CREATE TRIGGER orders_touch BEFORE UPDATE ON orders
            FOR EACH ROW EXECUTE FUNCTION touch();
        CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100;
        CREATE MATERIALIZED VIEW order_totals AS
            SELECT customer, sum(total) AS total FROM orders GROUP BY customer;
        CREATE FUNCTION add(a integer, b integer) RETURNS integer
            LANGUAGE sql AS 'SELECT a + b';
        CREATE SEQUENCE invoice_numbers;
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
    async fn walks_the_catalog_across_databases_and_shows_definitions() {
        let Some((config, password)) = test_server() else {
            return;
        };
        let mut connection = connect(&config, password.as_ref(), Route::Direct)
            .await
            .unwrap();
        connection
            .primary
            .client
            .batch_execute(FIXTURE)
            .await
            .unwrap();

        let nodes = conformance::walk_catalog(&mut connection, 6).await;
        conformance::check_invalid_path(&mut connection).await;

        let root_labels = labels_under(&nodes, &CatalogPath::root());
        assert!(
            root_labels.contains(&config.server.database.as_str()),
            "{root_labels:?}"
        );
        assert!(root_labels.contains(&"postgres"), "{root_labels:?}");
        assert_eq!(root_labels.first(), Some(&"roles"), "{root_labels:?}");
        let roles = folder(&CatalogPath::root(), FolderKind::Roles);
        let login = nodes
            .iter()
            .find(|node| node.path == roles.child(NodeKind::Role, config.server.user.clone()))
            .expect("the test server's own role");
        assert_eq!(login.detail.as_deref(), Some("login, superuser"));
        assert!(!login.system);
        assert!(
            nodes
                .iter()
                .any(|node| node.path.parent().as_ref() == Some(&roles) && node.system),
            "PostgreSQL's predefined pg_ roles are system objects"
        );
        let database =
            CatalogPath::root().child(NodeKind::Database, config.server.database.clone());
        let pg_catalog = nodes
            .iter()
            .find(|node| node.path == database.child(NodeKind::Schema, "pg_catalog"))
            .unwrap();
        assert!(pg_catalog.system);

        let schema = database.child(NodeKind::Schema, FIXTURE_SCHEMA);
        let orders = folder(&schema, FolderKind::Tables).child(NodeKind::Table, "orders");
        let expected: [(CatalogPath, &[&str]); 9] = [
            (folder(&schema, FolderKind::Tables), &["orders"]),
            (folder(&schema, FolderKind::Views), &["big_orders"]),
            (
                folder(&schema, FolderKind::MaterializedViews),
                &["order_totals"],
            ),
            (
                folder(&schema, FolderKind::Functions),
                &["add(a integer, b integer)", "touch()"],
            ),
            (
                folder(&schema, FolderKind::Sequences),
                &["invoice_numbers", "orders_id_seq"],
            ),
            (
                folder(&orders, FolderKind::Columns),
                &["id", "customer", "total"],
            ),
            (
                folder(&orders, FolderKind::Indexes),
                &["orders_customer", "orders_pkey"],
            ),
            (
                folder(&orders, FolderKind::Constraints),
                &["orders_pkey", "orders_total_check"],
            ),
            (folder(&orders, FolderKind::Triggers), &["orders_touch"]),
        ];
        for (parent, labels) in expected {
            assert_eq!(labels_under(&nodes, &parent), labels, "under {parent}");
        }

        let view = folder(&schema, FolderKind::Views).child(NodeKind::View, "big_orders");
        let view_definition = connection.definition(&view).await.unwrap();
        assert!(
            view_definition.starts_with("CREATE VIEW dbrover_catalog_test.big_orders AS"),
            "{view_definition}"
        );
        let add = folder(&schema, FolderKind::Functions)
            .child(NodeKind::Function, "add(a integer, b integer)");
        assert!(connection.definition(&add).await.unwrap().contains("a + b"));

        Box::new(connection).close().await.unwrap();
    }

    #[tokio::test]
    async fn runs_pages_cancels_and_reports_error_positions() {
        let Some((config, password)) = test_server() else {
            return;
        };
        let mut connection = connect(&config, password.as_ref(), Route::Direct)
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
        conformance::check_cancel(&mut connection, "SELECT pg_sleep(60)").await;

        Box::new(connection).close().await.unwrap();
    }
}
