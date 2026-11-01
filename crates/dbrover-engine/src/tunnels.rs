//! SSH tunnels for the data sources that name an SSH server: one per data source, shared by
//! its connections, and closed with the last of them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, Cell, ChildrenPage, ChildrenQuery, Connection,
    DataSourceId, DriverError, FullScan, Rejection, RelationName, ResultCursor, RoleDescription,
    ServerInfo, SshConfig, TableConstraints, TableDescription, TableName, TransactionStatus,
};
use dbrover_ssh::{SshError, Tunnel, TunnelRequest};

/// The open tunnels, and the `known_hosts` file their host keys are checked against. Cloning
/// shares them, so every session of the engine (or of `dbrover mcp`) finds the same tunnels.
#[derive(Clone)]
pub struct Tunnels(Arc<Shared>);

struct Shared {
    known_hosts: PathBuf,
    /// A slot per data source that has had a tunnel. Each has its own lock, held while its
    /// tunnel opens, so two sessions connecting at once share one tunnel while another data
    /// source's slow SSH server holds nobody else up.
    slots: Mutex<HashMap<DataSourceId, Arc<tokio::sync::Mutex<Slot>>>>,
}

/// The data source's tunnel while any connection holds it. Weak, so the tunnel closes when
/// the last connection through it does.
#[derive(Default)]
struct Slot {
    open: Option<(TunnelKey, Weak<Tunnel>)>,
}

/// What a tunnel goes through and to: a data source whose settings change gets a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TunnelKey {
    ssh: SshConfig,
    target_host: String,
    target_port: u16,
}

impl Tunnels {
    /// Tunnels that trust the host keys in `known_hosts`, normally `~/.ssh/known_hosts`.
    pub fn new(known_hosts: PathBuf) -> Self {
        Self(Arc::new(Shared {
            known_hosts,
            slots: Mutex::default(),
        }))
    }

    pub fn known_hosts(&self) -> &Path {
        &self.0.known_hosts
    }

    /// The data source's open tunnel to the same place, or a new one.
    pub(crate) async fn lease(
        &self,
        id: DataSourceId,
        request: TunnelRequest<'_>,
    ) -> Result<Arc<Tunnel>, SshError> {
        let slot = Arc::clone(
            self.0
                .slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(id)
                .or_default(),
        );
        let mut slot = slot.lock().await;
        let key = TunnelKey {
            ssh: request.ssh.clone(),
            target_host: request.target_host.to_owned(),
            target_port: request.target_port,
        };
        let shared = slot
            .open
            .as_ref()
            .filter(|(open_key, _)| *open_key == key)
            .and_then(|(_, tunnel)| tunnel.upgrade())
            .filter(|tunnel| !tunnel.is_closed());
        if let Some(tunnel) = shared {
            return Ok(tunnel);
        }
        let tunnel = Arc::new(Tunnel::open(request).await?);
        slot.open = Some((key, Arc::downgrade(&tunnel)));
        Ok(tunnel)
    }
}

/// A connection made through a tunnel, which it keeps open for as long as it lives.
pub(crate) struct Tunnelled {
    pub(crate) connection: Box<dyn Connection>,
    pub(crate) tunnel: Arc<Tunnel>,
}

#[async_trait]
impl Connection for Tunnelled {
    fn server_info(&self) -> &ServerInfo {
        self.connection.server_info()
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.connection.ping().await
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        self.connection.children(path).await
    }

    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        self.connection.children_page(path, query).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        self.connection.definition(path).await
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.connection.execute(sql).await
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.connection.execute_bound(sql, values).await
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        self.connection.describe_table(table).await
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        self.connection.describe_constraints(table).await
    }

    async fn describe_role(&mut self, path: &CatalogPath) -> Result<RoleDescription, DriverError> {
        self.connection.describe_role(path).await
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        self.connection.validate(sql).await
    }

    async fn full_scan(&mut self, statement: &str) -> Result<Option<FullScan>, DriverError> {
        self.connection.full_scan(statement).await
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        self.connection.transaction_status().await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.connection.cancel_handle()
    }

    /// Closes the connection first, so it can say goodbye to the server through the tunnel.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        let Self { connection, tunnel } = *self;
        let closed = connection.close().await;
        drop(tunnel);
        closed
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use dbrover_core::{SshConfig, SshLogin};

    use super::*;

    /// The `ssh` compose service, signing in with the test key, and a `known_hosts` that
    /// trusts it; `None` (skipping the test) unless `DBROVER_TEST_SSH_URL` is set.
    fn test_server(dir: &Path) -> Option<(SshConfig, PathBuf)> {
        let url = std::env::var("DBROVER_TEST_SSH_URL").ok()?;
        let (_, address) = url.rsplit_once('@')?;
        let (host, port) = address.split_once(':')?;
        let docker_ssh = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/ssh");
        let host_key = std::fs::read_to_string(docker_ssh.join("ssh_host_ed25519_key.pub")).ok()?;
        let known_hosts = dir.join("known_hosts");
        let key: Vec<&str> = host_key.split_whitespace().take(2).collect();
        std::fs::write(&known_hosts, format!("[{host}]:{port} {}\n", key.join(" "))).ok()?;
        let ssh = SshConfig {
            host: host.to_owned(),
            port: port.parse::<u16>().ok()?.into(),
            user: "dbrover".to_owned(),
            login: SshLogin::KeyFile {
                key_file: docker_ssh.join("client_ed25519"),
            },
        };
        Some((ssh, known_hosts))
    }

    #[tokio::test]
    async fn shares_one_tunnel_per_data_source_until_the_last_holder_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let Some((ssh, known_hosts)) = test_server(dir.path()) else {
            eprintln!("skipped: set DBROVER_TEST_SSH_URL (see test-servers.env)");
            return;
        };
        let tunnels = Tunnels::new(known_hosts);
        let (shop, warehouse) = (DataSourceId::generate(), DataSourceId::generate());
        let request = TunnelRequest {
            ssh: &ssh,
            secret: None,
            target_host: "postgres",
            target_port: 5432,
            known_hosts: tunnels.known_hosts(),
        };

        let first = tunnels.lease(shop, request).await.unwrap();
        let second = tunnels.lease(shop, request).await.unwrap();
        let other = tunnels.lease(warehouse, request).await.unwrap();

        assert!(Arc::ptr_eq(&first, &second), "one data source, one tunnel");
        assert!(!Arc::ptr_eq(&first, &other), "each data source its own");
        let first_port = first.local_addr();
        drop((first, second));
        let reopened = tunnels.lease(shop, request).await.unwrap();
        assert_ne!(reopened.local_addr(), first_port, "the old tunnel closed");
    }
}
