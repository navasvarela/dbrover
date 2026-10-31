//! ClickHouse driver for DbRover, over ClickHouse's HTTP interface with its official client.
//!
//! HTTP is stateless, so a connection is a client and a `session_id`: the server keeps a
//! session's `USE`, `SET` and temporary tables between requests, as a console expects. A
//! session ends after [`SESSION_TIMEOUT_SECONDS`] without a request, and a request after that
//! starts a new one, in the user's default database. Cancels go out on a
//! client without the session, since the session is busy with the query being cancelled.
//! ClickHouse has no transactions in practice (they are experimental), so a connection is
//! never inside one.

mod catalog;
mod errors;
mod execution;
mod table;

use std::sync::Arc;

use async_trait::async_trait;
use clickhouse::Client;
use dbrover_core::{
    BoxError, CancelHandle, CatalogNode, CatalogPath, ClickHouseConfig, Connection, DriverError,
    Password, Quoting, Rejection, RelationName, ResultCursor, Route, ServerInfo, TableConstraints,
    TableDescription, TableName, TlsMode, TransactionStatus,
};
pub use dbrover_tls::TlsSetupError;
use hyper_rustls::FixedServerNameResolver;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::ServerName;
use uuid::Uuid;

/// How long the server keeps an idle session, in seconds: an hour, the most a server allows by
/// default (`max_session_timeout`). After that, the session's temporary tables and settings
/// are gone.
const SESSION_TIMEOUT_SECONDS: &str = "3600";

pub(crate) const CLICKHOUSE_QUOTING: Quoting = Quoting {
    identifier_open: '`',
    identifier_close: '`',
    backslash_escapes: true,
    string_prefix: "",
};

/// Settings every request carries:
/// - an error partway through a result is written as plain text, which the client recognises,
///   rather than as one more row of the result, which it would take for data;
/// - decimals keep their trailing zeros, as their type says (`12.50`);
/// - a read stops when its client goes away, so an abandoned result stops being computed.
const REQUEST_SETTINGS: &[(&str, &str)] = &[
    ("http_write_exception_in_output_format", "0"),
    ("output_format_decimal_trailing_zeros", "1"),
    ("cancel_http_readonly_queries_on_client_close", "1"),
];

/// Sent with every request of a read-only connection, so the server refuses writes, DDL and
/// lifting the setting itself. `2` rather than `1`, which would also refuse a query's own
/// `SETTINGS` (`SELECT … SETTINGS max_threads = 1`), and can't be sent at all as a user whose
/// profile already has `readonly = 2`. The server checks a request's settings against the
/// user's own `readonly` and only then applies them, so [`REQUEST_SETTINGS`] still go through.
const READ_ONLY_SETTING: (&str, &str) = ("readonly", "2");

/// Opens a session, over HTTPS unless `config.server.tls` is `disable` or `prefer`, and
/// read-only if `config.read_only` says so.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used, the server is unreachable, the
/// TLS handshake or certificate check fails, the server rejects the credentials, or the
/// database doesn't exist.
pub async fn connect(
    config: &ClickHouseConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<ClickHouseConnection, DriverError> {
    let address = match route {
        Route::Direct => format!("{}:{}", config.server.host, config.server.port_number()?),
        Route::Tunnel(local) => local.to_string(),
    };
    let base = client(config, &address, password).map_err(connect_error)?;
    // The session's database is set once with `USE`, not sent with every request: a
    // request's `database` would undo the user's own `USE`.
    let session = base
        .clone()
        .with_setting("session_id", Uuid::new_v4().to_string())
        .with_setting("session_timeout", SESSION_TIMEOUT_SECONDS);
    let database = CLICKHOUSE_QUOTING.identifier(&config.server.database);
    execution::single_value(&session, &format!("USE {database}"))
        .await
        .map_err(connect_error)?;
    let version = execution::single_value(&session, "SELECT version()")
        .await
        .map_err(connect_error)?;
    let base = base.with_database(&config.server.database);
    Ok(ClickHouseConnection {
        session,
        cancel: Arc::new(execution::ClickHouseCancel::new(base)),
        server_info: ServerInfo {
            product: "ClickHouse".to_owned(),
            version,
        },
    })
}

/// A client for the server in `config`, reached at `address` (`host:port`, the server's own
/// or an SSH tunnel's), with no session and no database.
///
/// HTTPS is a port of its own (8443), not an upgrade of the HTTP one, so `prefer` can't try
/// TLS and fall back on the same port: it means plain HTTP, as `disable` does.
fn client(
    config: &ClickHouseConfig,
    address: &str,
    password: Option<&Password>,
) -> Result<Client, BoxError> {
    let server = &config.server;
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    let (scheme, client) = match &server.tls {
        TlsMode::Disable | TlsMode::Prefer => (
            "http",
            Client::with_http_client(HyperClient::builder(TokioExecutor::new()).build(http)),
        ),
        tls @ (TlsMode::Require | TlsMode::VerifyFull { .. }) => {
            // The URL may name a tunnel's local end; the certificate must name the server.
            let server_name = ServerName::try_from(server.host.clone())?;
            let connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(dbrover_tls::client_config(tls)?)
                .https_only()
                .with_server_name_resolver(FixedServerNameResolver::new(server_name))
                .enable_http1()
                .wrap_connector(http);
            (
                "https",
                Client::with_http_client(
                    HyperClient::builder(TokioExecutor::new()).build(connector),
                ),
            )
        }
    };
    let client = client
        .with_url(format!("{scheme}://{address}"))
        .with_user(&server.user)
        .with_password(password.map_or("", Password::expose));
    let read_only = config.read_only.then_some(READ_ONLY_SETTING);
    Ok(REQUEST_SETTINGS
        .iter()
        .copied()
        .chain(read_only)
        .fold(client, |client, (name, value)| {
            client.with_setting(name, value)
        }))
}

fn connect_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

/// A failed request, as [`DriverError::Lost`] when it means the session is over.
fn connection_error(error: impl Into<BoxError>) -> DriverError {
    let error = error.into();
    if error
        .downcast_ref::<execution::Failure>()
        .is_some_and(execution::Failure::is_lost)
    {
        DriverError::Lost(error)
    } else {
        DriverError::Connection(error)
    }
}

/// A session with a ClickHouse server.
pub struct ClickHouseConnection {
    session: Client,
    cancel: Arc<execution::ClickHouseCancel>,
    server_info: ServerInfo,
}

#[async_trait]
impl Connection for ClickHouseConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        execution::single_value(&self.session, "SELECT 1")
            .await
            .map(drop)
            .map_err(connection_error)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&self.session, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        catalog::definition(&self.session, path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.session, &self.cancel, sql).await
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        table::describe_table(&self.session, table).await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        table::describe_constraints(&self.session, table).await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        execution::validate(&self.session, sql).await
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.cancel) as Arc<dyn CancelHandle>
    }

    /// Nothing to close: the server ends the session once it has been idle for its timeout.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
