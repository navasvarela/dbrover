//! MySQL and MariaDB driver for DbRover, built on `mysql_async`.
//!
//! One driver for both servers: they speak the same protocol and SQL, and what differs is
//! keyed off the version the server reports. Must run inside a tokio runtime.

mod catalog;
mod errors;
mod execution;
mod table;
mod tls;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    BoxError, CancelHandle, CatalogNode, CatalogPath, Cell, Connection, DriverError, MysqlConfig,
    Password, Quoting, Rejection, RelationName, ResultCursor, Route, ServerInfo, TableConstraints,
    TableDescription, TableName, TlsMode, TransactionStatus,
};
use mysql_async::consts::StatusFlags;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder};
use tokio::sync::Mutex;

pub use tls::TlsSetupError;

/// Shown in `performance_schema.session_connect_attrs`, so a DBA can tell DbRover's sessions
/// apart.
const APPLICATION_NAME: &str = "dbrover";

/// How the SQL this driver writes quotes names: backticks, which mean an identifier whatever
/// the session's `sql_mode`. The same as `dbrover-sql`'s MySQL dialect, which this crate
/// doesn't depend on.
pub(crate) const MYSQL_QUOTING: Quoting = Quoting {
    identifier_open: '`',
    identifier_close: '`',
    backslash_escapes: true,
    string_prefix: "",
};

/// Opens a connection, encrypted as `config.server.tls` says.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used (a missing CA file, say), the
/// server is unreachable, the TLS handshake or certificate check fails, the server rejects
/// the credentials, or the database doesn't exist.
pub async fn connect(
    config: &MysqlConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<MysqlConnection, DriverError> {
    install_crypto_provider();
    let always_encrypted = matches!(
        config.server.tls,
        TlsMode::Require | TlsMode::VerifyFull { .. }
    );
    if config.server.cloud_sign_in.is_some() && !always_encrypted {
        return Err(connect_error(
            "a cloud sign-in sends its token to MySQL as clear text, so it needs TLS: set TLS \
             to require or verify-full",
        ));
    }
    let address = match route {
        Route::Direct => (config.server.host.clone(), config.server.port_number()?),
        Route::Tunnel(local) => (local.ip().to_string(), local.port()),
    };
    let mut ssl_opts = tls::ssl_opts(&config.server.tls).map_err(connect_error)?;
    if let (Route::Tunnel(_), Some(opts)) = (route, ssl_opts.as_mut()) {
        // The socket goes to the tunnel's local end; the certificate must still name the
        // server.
        *opts = opts
            .clone()
            .with_danger_tls_hostname_override(Some(config.server.host.clone()));
    }
    let encrypted = base_opts(config, &address, password).ssl_opts(ssl_opts);
    let (mut conn, opts) = match Conn::new(encrypted.clone()).await {
        Ok(conn) => (conn, Opts::from(encrypted)),
        Err(error) if tls::falls_back_to_plain_text(&config.server.tls, &error) => {
            let plain = base_opts(config, &address, password);
            let conn = Conn::new(plain.clone()).await.map_err(connect_error)?;
            (conn, Opts::from(plain))
        }
        Err(error) => return Err(connect_error(error)),
    };
    let version: Option<String> = conn
        .query_first("SELECT VERSION()")
        .await
        .map_err(connect_error)?;
    let server_info = server_info(version.unwrap_or_default());
    let connection_id = conn.id();
    Ok(MysqlConnection {
        conn: Arc::new(Mutex::new(conn)),
        cancel: Arc::new(MysqlCancel {
            opts,
            connection_id,
        }),
        server_info,
    })
}

/// Makes ring rustls's process-wide crypto provider, unless one is installed already.
///
/// `mysql_async`'s rustls connector takes the process-wide provider, which rustls only picks
/// by itself while a single provider is compiled in. The SQL Server driver's `tiberius` also
/// compiles in aws-lc-rs, so whichever driver connects first installs ring, which every
/// driver uses.
fn install_crypto_provider() {
    // An error only means a provider is installed already, which is what was wanted.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The options for connecting to `address`: the server's host and port, or a tunnel's.
fn base_opts(
    config: &MysqlConfig,
    (host, port): &(String, u16),
    password: Option<&Password>,
) -> OptsBuilder {
    let server = &config.server;
    OptsBuilder::default()
        .ip_or_hostname(host.clone())
        .tcp_port(*port)
        .user(Some(server.user.clone()))
        .pass(password.map(|password| password.expose().to_owned()))
        .db_name(Some(server.database.clone()))
        // The host is a network address. Left on, mysql_async switches a connection to
        // `localhost` over to the server's Unix socket, which on a container's host is a path
        // that doesn't exist.
        .prefer_socket(Some(false))
        // Validation prepares statements only to close them again; a cache would keep them
        // open on the server for nothing.
        .stmt_cache_size(Some(0))
        .connect_attribute("program_name", APPLICATION_NAME)
        // RDS's, Cloud SQL's and Azure Database's IAM users sign in with
        // `mysql_clear_password`, which only ever goes over TLS: `connect` refuses a cloud
        // sign-in whose TLS mode could fall back to plain text.
        .enable_cleartext_plugin(server.cloud_sign_in.is_some())
}

const MARIADB: &str = "MariaDB";

/// Which of the two servers the driver talks to, where they answer the same question
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavour {
    Mysql,
    MariaDb,
}

impl Flavour {
    fn of(server: &ServerInfo) -> Self {
        if server.product == MARIADB {
            Self::MariaDb
        } else {
            Self::Mysql
        }
    }
}

/// `VERSION()` is `8.4.3` on MySQL and `11.4.4-MariaDB-…` on MariaDB.
fn server_info(version: String) -> ServerInfo {
    match version.split_once("-MariaDB") {
        Some((number, _)) => ServerInfo::new(MARIADB, number.to_owned()),
        None => ServerInfo::new("MySQL", version),
    }
}

fn connect_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

/// A failure of the connection, as [`DriverError::Lost`] when it means the session is over.
fn connection_error(error: impl Into<BoxError>) -> DriverError {
    let error = error.into();
    if error
        .downcast_ref::<mysql_async::Error>()
        .is_some_and(errors::is_lost)
    {
        DriverError::Lost(error)
    } else {
        DriverError::Connection(error)
    }
}

/// A session with a MySQL or MariaDB server.
pub struct MysqlConnection {
    /// Behind an async mutex because a running statement's task owns the lock until its
    /// cursor finishes: `mysql_async` borrows the connection for as long as a result is open.
    conn: Arc<Mutex<Conn>>,
    cancel: Arc<MysqlCancel>,
    server_info: ServerInfo,
}

#[async_trait]
impl Connection for MysqlConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.conn
            .lock()
            .await
            .query_drop("SELECT 1")
            .await
            .map_err(connection_error)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&mut *self.conn.lock().await, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        catalog::definition(&mut *self.conn.lock().await, path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.conn, sql, &[]).await
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.conn, sql, values).await
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        let flavour = Flavour::of(&self.server_info);
        table::describe_table(&mut *self.conn.lock().await, table, flavour).await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        let flavour = Flavour::of(&self.server_info);
        table::describe_constraints(&mut *self.conn.lock().await, table, flavour).await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        execution::validate(&mut *self.conn.lock().await, sql).await
    }

    /// From the status flags of the server's reply to a statement that does nothing: MySQL
    /// has no variable that says whether a transaction is open (MariaDB's `@@in_transaction`
    /// is its own).
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        let mut conn = self.conn.lock().await;
        conn.query_drop("DO 0").await.map_err(connection_error)?;
        let open = conn.last_ok_packet().is_some_and(|ok| {
            ok.status_flags()
                .contains(StatusFlags::SERVER_STATUS_IN_TRANS)
        });
        Ok(if open {
            TransactionStatus::InTransaction
        } else {
            TransactionStatus::Idle
        })
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.cancel) as Arc<dyn CancelHandle>
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        match Arc::try_unwrap(self.conn) {
            Ok(conn) => conn
                .into_inner()
                .disconnect()
                .await
                .map_err(connection_error),
            // A statement's task still holds the connection, which is closed when that task
            // drops it; the session ends either way.
            Err(_) => Ok(()),
        }
    }
}

/// MySQL has no cancel message in its protocol: a cancel is `KILL QUERY` sent from a second,
/// short-lived connection, which stops the statement the first one is running and leaves
/// that connection open.
struct MysqlCancel {
    /// What the session connected with, TLS included, so the second connection is allowed
    /// wherever the first one was.
    opts: Opts,
    connection_id: u32,
}

#[async_trait]
impl CancelHandle for MysqlCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let mut conn = Conn::new(self.opts.clone())
            .await
            .map_err(connection_error)?;
        let killed = conn
            .query_drop(format!("KILL QUERY {}", self.connection_id))
            .await;
        // Close the second connection whether or not the kill worked, then report the kill.
        let closed = conn.disconnect().await;
        killed.map_err(connection_error)?;
        closed.map_err(connection_error)
    }
}

#[cfg(test)]
mod tests;
