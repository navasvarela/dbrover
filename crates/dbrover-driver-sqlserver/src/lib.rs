//! SQL Server and Azure SQL driver for DbRover, built on `tiberius`.
//!
//! Signs in with a SQL Server login. Every statement is sent as a plain batch (a TDS
//! `SQLBatch`), never through `sp_executesql`, so the temporary tables, variables and `SET`
//! options it creates belong to the session, as in SQL Server's own tools. Must run inside a
//! tokio runtime.

mod catalog;
mod constraint;
mod errors;
mod execution;
mod table;
mod tls;
mod values;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    BoxError, CancelHandle, CatalogNode, CatalogPath, Cell, Connection, DriverError, Password,
    Quoting, Rejection, RelationName, ResultCursor, Route, ServerInfo, SqlServerConfig,
    TableConstraints, TableDescription, TableName, TransactionStatus,
};
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub use tls::TlsSetupError;

/// Shown in `sys.dm_exec_sessions.program_name`, so a DBA can tell DbRover's sessions apart.
const APPLICATION_NAME: &str = "dbrover";

/// What the SQL this driver writes quotes names with: brackets, which mean an identifier
/// whatever the session's `QUOTED_IDENTIFIER`. The same as `dbrover-sql`'s SQL Server dialect.
pub(crate) const SQL_SERVER_QUOTING: Quoting = Quoting {
    identifier_open: '[',
    identifier_close: ']',
    backslash_escapes: false,
    string_prefix: "N",
};

/// The session options SQL Server's own tools and its ODBC and OLE DB drivers set on every
/// connection; a bare TDS login gets the server's older defaults instead. Without them,
/// `"name"` is a string rather than an identifier, `= NULL` is true for nulls, and indexes on
/// computed columns and indexed views refuse changes.
const SESSION_OPTIONS: &str = "SET QUOTED_IDENTIFIER ON; SET ANSI_NULLS ON; \
     SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; SET ANSI_NULL_DFLT_ON ON; \
     SET CONCAT_NULL_YIELDS_NULL ON; SET ARITHABORT ON";

pub(crate) type SqlClient = Client<Compat<TcpStream>>;

/// Opens a connection, encrypted as `config.server.tls` says.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used (a missing CA file, say), the
/// server is unreachable, the TLS handshake or certificate check fails, the server rejects
/// the login, or the database doesn't exist or the login may not open it.
pub async fn connect(
    config: &SqlServerConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<SqlServerConnection, DriverError> {
    tls::install_crypto_provider();
    let mut tiberius_config = base_config(config, config.server.port_number()?, password);
    tls::configure(&mut tiberius_config, &config.server.tls).map_err(connect_error)?;
    let mut client = open_client(tiberius_config, route).await?;
    client
        .simple_query(SESSION_OPTIONS)
        .await
        .map_err(connect_error)?
        .into_results()
        .await
        .map_err(connect_error)?;
    let server_info = server_info(&mut client).await.map_err(connect_error)?;
    Ok(SqlServerConnection {
        client: Arc::new(Mutex::new(client)),
        cancel: Arc::new(execution::CancelSlot::default()),
        server_info,
        statistics_xml: false,
    })
}

fn base_config(config: &SqlServerConfig, port: u16, password: Option<&Password>) -> Config {
    let server = &config.server;
    let mut tiberius_config = Config::new();
    tiberius_config.host(&server.host);
    tiberius_config.port(port);
    tiberius_config.database(&server.database);
    tiberius_config.application_name(APPLICATION_NAME);
    // `tiberius` gives up on a statement after 30 seconds without a reply by default, and
    // leaves the connection unusable. A console's statements may rightly run for hours; ⌘.
    // stops one.
    tiberius_config.command_timeout(None);
    tiberius_config.authentication(AuthMethod::sql_server(
        &server.user,
        password.map_or("", Password::expose),
    ));
    tiberius_config
}

/// Connects and signs in, following one redirect: Azure SQL's gateway may answer the login
/// by naming the server that holds the database, which the client must then connect to.
///
/// The socket goes where `route` says, while `config` keeps the server's name for TLS.
async fn open_client(mut config: Config, route: Route) -> Result<SqlClient, DriverError> {
    let address = match route {
        Route::Direct => config.get_addr(),
        Route::Tunnel(local) => local.to_string(),
    };
    let tcp = tcp_connect(&address).await?;
    match Client::connect(config.clone(), tcp.compat_write()).await {
        Ok(client) => Ok(client),
        Err(tiberius::error::Error::Routing { host, port }) if route == Route::Direct => {
            config.host(&host);
            config.port(port);
            let tcp = tcp_connect(&config.get_addr()).await?;
            Client::connect(config, tcp.compat_write())
                .await
                .map_err(connect_error)
        }
        // The tunnel only reaches the server it was opened to.
        Err(tiberius::error::Error::Routing { host, port }) => Err(DriverError::Connect(
            format!(
                "the server redirected the login to {host}:{port}, which the SSH tunnel \
                 doesn't reach"
            )
            .into(),
        )),
        Err(error) => Err(connect_error(error)),
    }
}

async fn tcp_connect(address: &str) -> Result<TcpStream, DriverError> {
    let tcp = TcpStream::connect(address).await.map_err(connect_error)?;
    // Statements and their replies are small messages; waiting to batch them only adds
    // latency.
    tcp.set_nodelay(true).map_err(connect_error)?;
    Ok(tcp)
}

/// `SERVERPROPERTY('ProductVersion')` is `16.0.4305.5` on SQL Server 2022; Azure SQL reports
/// itself as edition `SQL Azure`.
async fn server_info(client: &mut SqlClient) -> tiberius::Result<ServerInfo> {
    let row = client
        .simple_query(
            "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), \
             CAST(SERVERPROPERTY('Edition') AS nvarchar(128))",
        )
        .await?
        .into_row()
        .await?;
    let text = |index: usize| {
        row.as_ref()
            .and_then(|row| column::<&str>(row, index))
            .unwrap_or_default()
            .to_owned()
    };
    let product = if text(1) == "SQL Azure" {
        "Azure SQL"
    } else {
        "SQL Server"
    };
    Ok(ServerInfo {
        product: product.to_owned(),
        version: text(0),
    })
}

/// Column `index` of `row`, or `None` if it is `NULL` or not a `T`. Never
/// `tiberius::Row::get`, which panics on a type it can't convert.
pub(crate) fn column<'a, T: tiberius::FromSql<'a>>(
    row: &'a tiberius::Row,
    index: usize,
) -> Option<T> {
    row.try_get(index).ok().flatten()
}

fn connect_error(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

/// A failure of the connection, as [`DriverError::Lost`] when it means the session is over.
fn connection_error(error: impl Into<BoxError>) -> DriverError {
    let error = error.into();
    if error
        .downcast_ref::<tiberius::error::Error>()
        .is_some_and(errors::is_lost)
    {
        DriverError::Lost(error)
    } else {
        DriverError::Connection(error)
    }
}

/// A session with a SQL Server or Azure SQL database.
pub struct SqlServerConnection {
    /// Behind an async mutex because a running statement's task owns the lock until its
    /// cursor finishes: `tiberius` borrows the client for as long as a result is open.
    client: Arc<Mutex<SqlClient>>,
    cancel: Arc<execution::CancelSlot>,
    server_info: ServerInfo,
    /// Whether the session's `STATISTICS XML` is on, as the last statement that switched it
    /// left it: each statement's reply then ends with its actual plan, which is what the
    /// statement answers with (Explain analyse, W10).
    statistics_xml: bool,
}

impl SqlServerConnection {
    /// The client, once no statement holds it, unless a lost cancel acknowledgement has left
    /// it out of step with the server.
    async fn client(&self) -> Result<tokio::sync::MutexGuard<'_, SqlClient>, DriverError> {
        self.cancel.ensure_in_step()?;
        Ok(self.client.lock().await)
    }

    /// Runs a user's statement, answering with its actual plan while `STATISTICS XML` is on,
    /// and noting a statement that switches it.
    async fn run(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.cancel.ensure_in_step()?;
        let answer = if self.statistics_xml {
            execution::Answer::ActualPlan
        } else {
            execution::Answer::Rows
        };
        let cursor = execution::execute(&self.client, &self.cancel, sql, values, answer).await?;
        if let Some(on) = execution::statistics_xml_switch(sql) {
            self.statistics_xml = on;
        }
        Ok(cursor)
    }
}

#[async_trait]
impl Connection for SqlServerConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        execution::run_quietly(&mut *self.client().await?, "SELECT 1").await
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&mut *self.client().await?, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        catalog::definition(&mut *self.client().await?, path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.run(sql, &[]).await
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.run(sql, values).await
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        table::describe_table(&mut *self.client().await?, table).await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        constraint::describe_constraints(&mut *self.client().await?, table).await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        execution::validate(&mut *self.client().await?, sql).await
    }

    /// `@@TRANCOUNT` counts the open transactions (nested `BEGIN TRANSACTION`s included), and
    /// `XACT_STATE()` is `-1` once an error has doomed the transaction, which then only
    /// accepts `ROLLBACK`.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        let mut client = self.client().await?;
        let row = client
            .simple_query("SELECT @@TRANCOUNT, XACT_STATE()")
            .await
            .map_err(connection_error)?
            .into_row()
            .await
            .map_err(connection_error)?;
        let count: i32 = row.as_ref().and_then(|row| column(row, 0)).unwrap_or(0);
        let state: i16 = row.as_ref().and_then(|row| column(row, 1)).unwrap_or(0);
        Ok(match (count, state) {
            (0, _) => TransactionStatus::Idle,
            (_, -1) => TransactionStatus::Failed,
            _ => TransactionStatus::InTransaction,
        })
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.cancel) as Arc<dyn CancelHandle>
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        match Arc::try_unwrap(self.client) {
            Ok(client) => client.into_inner().close().await.map_err(connection_error),
            // A statement's task still holds the client, which is closed when that task drops
            // it; the session ends either way.
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
