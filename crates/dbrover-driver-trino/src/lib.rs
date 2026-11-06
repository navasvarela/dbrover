//! Trino driver for DbRover, over Trino's client protocol (`docs/cloud-data.md`, C3), which
//! Starburst speaks as it is and Presto with its own header names.
//!
//! HTTP is stateless, so a connection is a client and a [`protocol::Session`]: the catalog,
//! schema, session properties, prepared statements and transaction the coordinator hands back
//! in its answers' headers and the client sends with every statement. A statement's result is
//! read as the console pages through it ([`statement`]).

mod catalog;
mod errors;
mod http;
mod protocol;
mod statement;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, Connection, DriverError, Password, ProgressHandle,
    Rejection, RelationName, ResultCursor, Route, ServerInfo, TableConstraints, TableDescription,
    TableName, TransactionStatus, TrinoConfig,
};
use hyper::{Method, StatusCode};
use serde::Deserialize;

use crate::http::Client;
use crate::protocol::Session;
use crate::statement::{Outcome, Runner, Running};

/// Put before a statement to have Trino analyse it, names and types included, without running
/// it.
const VALIDATE: &str = "EXPLAIN (TYPE VALIDATE) ";

/// What `GET /v1/info` answers, in part.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    node_version: Option<NodeVersion>,
}

#[derive(Deserialize)]
struct NodeVersion {
    version: String,
}

/// Connects to the coordinator in `config`, through `route`, asks it what it is, and runs
/// `SELECT 1` to prove the user and password are taken.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used, a password would go over plain
/// HTTP, the coordinator can't be reached, the TLS handshake or certificate check fails, or the
/// coordinator refuses the user or the password.
pub async fn connect(
    config: &TrinoConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<TrinoConnection, DriverError> {
    let address = match route {
        Route::Direct => format!("{}:{}", config.server.host, config.server.port_number()?),
        Route::Tunnel(local) => local.to_string(),
    };
    let client = Client::new(config, password, &address).map_err(errors::connect)?;
    let info = client
        .send(Method::GET, "/v1/info", None, &[])
        .await
        .map_err(errors::connect)?;
    let version = (info.status == StatusCode::OK)
        .then(|| serde_json::from_slice::<Info>(&info.body).ok())
        .flatten()
        .and_then(|info| info.node_version)
        .map_or_else(|| "unknown".to_owned(), |node| node.version);
    let optional = |setting: &str| (!setting.trim().is_empty()).then(|| setting.trim().to_owned());
    let session = Session {
        catalog: optional(&config.server.database),
        schema: optional(&config.schema),
        ..Session::default()
    };
    let runner = Runner {
        running: Arc::new(Running::new(client.clone())),
        client,
        session: Arc::new(Mutex::new(session)),
    };
    let mut connection = TrinoConnection {
        server_info: ServerInfo::new(config.protocol.product(), version),
        runner,
    };
    connection.ping().await.map_err(|error| match error {
        DriverError::Connection(source) | DriverError::Statement { source, .. } => {
            DriverError::Connect(source)
        }
        other => other,
    })?;
    Ok(connection)
}

/// A console's or the explorer's connection to a Trino, Starburst or Presto coordinator.
pub struct TrinoConnection {
    runner: Runner,
    server_info: ServerInfo,
}

#[async_trait]
impl Connection for TrinoConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.runner.rows("SELECT 1").await.map(drop)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&self.runner, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        catalog::definition(&self.runner, path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.runner.execute(sql).await
    }

    /// `None`: a Trino table has no key that tells its rows apart, so its rows are never
    /// edited (`Dialect::read_only_results`).
    async fn describe_table(
        &mut self,
        _table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        Ok(None)
    }

    async fn describe_constraints(
        &mut self,
        _table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        Ok(TableConstraints::default())
    }

    /// `EXPLAIN (TYPE VALIDATE)`, which analyses the statement, names and types included,
    /// without running it.
    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        let explained = format!("{VALIDATE}{sql}");
        match self.runner.start(&explained).await {
            Ok(cursor) => {
                // Read to its end, so the coordinator isn't left holding the plan.
                let mut cursor: Box<dyn ResultCursor> = Box::new(cursor);
                while !cursor.is_exhausted() {
                    cursor.next_page(usize::MAX).await?;
                }
                Ok(None)
            }
            Err(Outcome::Failed(error)) => Ok(Some(Rejection {
                kind: errors::rejection_kind(error.error_name.as_deref()),
                position: error
                    .error_location
                    .and_then(|at| {
                        dbrover_core::position_at(&explained, at.line_number, at.column_number)
                    })
                    .and_then(|position| position.checked_sub(VALIDATE.len())),
                message: error.message,
            })),
            Err(Outcome::Unreachable(error)) => Err(error),
        }
    }

    /// Inside a transaction once `START TRANSACTION` has handed back its id, until `COMMIT`
    /// or `ROLLBACK` clears it.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(match self.runner.session().transaction {
            Some(_) => TransactionStatus::InTransaction,
            None => TransactionStatus::Idle,
        })
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.runner.running) as Arc<dyn CancelHandle>
    }

    fn progress_handle(&self) -> Option<Arc<dyn ProgressHandle>> {
        Some(Arc::clone(&self.runner.running) as Arc<dyn ProgressHandle>)
    }

    /// Nothing to close: the coordinator keeps no connection. An open transaction is left to
    /// the coordinator, which ends it once it has been idle for its timeout.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}
