//! Cassandra and ScyllaDB driver for DbRover, with ScyllaDB's own client, which speaks CQL's
//! native protocol to both.
//!
//! A connection is a client session: a pool of connections to the cluster's nodes, routed to
//! the data source's local datacenter first. Statements run unprepared, as typed, so nothing is
//! left cached on the server, and their rows page with the protocol's own paging. The
//! protocol has no cancel: ⌘. stops waiting (see [`execution`]). CQL has no transactions, so a
//! connection is never inside one.

mod catalog;
mod errors;
mod execution;
mod routes;
mod tls;
mod values;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CassandraConfig, CatalogNode, CatalogPath, Connection, DriverError, Password,
    Rejection, RelationName, ResultCursor, Route, ServerConfig, ServerInfo, TableConstraints,
    TableDescription, TableName, TransactionStatus,
};
pub use dbrover_tls::TlsSetupError;
use scylla::client::PoolSize;
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;

use crate::execution::{Running, Timeouts};

/// How long the client waits for a node to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens a session with the cluster `config` names, as `config.server.user` when it has one.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used, no node can be reached, the TLS
/// handshake or certificate check fails, the cluster refuses the credentials, or the keyspace
/// doesn't exist.
pub async fn connect(
    config: &CassandraConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<CassandraConnection, DriverError> {
    let session = session(config, password, route)
        .await
        .map_err(errors::connect)?;
    let session = Arc::new(session);
    let server_info = server_info(&session).await?;
    let timeouts = Timeouts::new(config.statement_timeout, &server_info);
    let running = Arc::new(Running::default());
    Ok(CassandraConnection {
        session,
        server_info,
        timeouts,
        cancel: Arc::new(execution::CassandraCancel(Arc::clone(&running))),
        running,
    })
}

async fn session(
    config: &CassandraConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<Session, dbrover_core::BoxError> {
    let server = &config.server;
    let mut builder = SessionBuilder::new()
        .connection_timeout(CONNECT_TIMEOUT)
        .pool_size(PoolSize::PerHost(one()))
        .disallow_shard_aware_port(true)
        .tls_context(tls::context(server)?)
        .fetch_schema_metadata(false);
    builder = match route {
        Route::Direct => {
            let named = server_address(server).await?;
            builder
                .known_node_addr(named)
                .address_translator(Arc::new(routes::NearestRoute::new(named)))
        }
        // Every node is reached through the tunnel, which leads to the one the data source
        // names: the cluster's own addresses for its nodes can't be reached from here.
        Route::Tunnel(local) => builder
            .known_node_addr(local)
            .address_translator(Arc::new(routes::Tunnel(local))),
    };
    if !server.user.is_empty() {
        builder = builder.user(server.user.as_str(), password.map_or("", Password::expose));
    }
    if !config.local_datacenter.is_empty() {
        builder = builder.prefer_datacenter(config.local_datacenter.clone());
    }
    if !server.database.is_empty() {
        builder = builder.use_keyspace(server.database.as_str(), true);
    }
    Ok(builder.build().await?)
}

/// One connection per node: a console runs one statement at a time.
fn one() -> std::num::NonZeroUsize {
    std::num::NonZeroUsize::MIN
}

/// The first address the data source's host resolves to.
async fn server_address(server: &ServerConfig) -> Result<SocketAddr, dbrover_core::BoxError> {
    let port = server.port_number()?;
    tokio::net::lookup_host((server.host.as_str(), port))
        .await?
        .next()
        .ok_or_else(|| format!("{} resolves to no address", server.host).into())
}

/// ScyllaDB says which it is in `system.versions`, which Apache Cassandra doesn't have; both
/// have `system.local`'s `release_version`, which on ScyllaDB is the Cassandra version it
/// speaks for.
async fn server_info(session: &Session) -> Result<ServerInfo, DriverError> {
    let scylla = execution::single_text(
        session,
        "SELECT version FROM system.versions WHERE key = 'local'",
    )
    .await;
    if let Ok(Some(version)) = scylla {
        return Ok(ServerInfo {
            product: execution::SCYLLADB.to_owned(),
            version: version
                .split_once('-')
                .map_or(version.as_str(), |(release, _build)| release)
                .to_owned(),
        });
    }
    let version = execution::single_text(session, "SELECT release_version FROM system.local")
        .await
        .map_err(errors::connect)?
        .unwrap_or_default();
    Ok(ServerInfo {
        product: "Apache Cassandra".to_owned(),
        version,
    })
}

/// A session with a Cassandra or ScyllaDB cluster.
pub struct CassandraConnection {
    session: Arc<Session>,
    server_info: ServerInfo,
    timeouts: Timeouts,
    running: Arc<Running>,
    cancel: Arc<execution::CassandraCancel>,
}

#[async_trait]
impl Connection for CassandraConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        execution::single_text(&self.session, "SELECT release_version FROM system.local")
            .await
            .map(drop)
            .map_err(errors::connection)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&self.session, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        catalog::definition(&self.session, path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        execution::execute(&self.session, &self.running, &self.timeouts, sql).await
    }

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

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        execution::validate(&self.session, sql).await
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.cancel) as Arc<dyn CancelHandle>
    }

    /// Nothing to close: dropping the session closes its connections.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
