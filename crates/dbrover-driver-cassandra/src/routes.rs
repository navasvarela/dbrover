//! Where the client reaches each node of the cluster.
//!
//! The cluster tells the client its nodes' addresses as the nodes know them, which a client
//! outside their network may not reach: a node in Docker gives its container's address, one
//! behind NAT its private one, and every node behind an SSH tunnel is out of reach but through
//! the tunnel. The data source names one node the client can reach, so a node that can't be
//! reached at its own address is reached through that one, which forwards each request to the
//! node that holds its rows.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use scylla::errors::TranslationError;
use scylla::policies::address_translator::{AddressTranslator, UntranslatedPeer};
use tokio::net::TcpStream;

/// How long a node's own address has to answer before it counts as out of reach. A node on
/// the same network answers in milliseconds; an address that leads nowhere would otherwise
/// keep connecting for TCP's own timeout, minutes.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Sends every node's traffic to an SSH tunnel's local end.
pub(crate) struct Tunnel(pub(crate) SocketAddr);

#[async_trait]
impl AddressTranslator for Tunnel {
    async fn translate_address(
        &self,
        _untranslated_peer: &UntranslatedPeer,
    ) -> Result<SocketAddr, TranslationError> {
        Ok(self.0)
    }
}

/// Reaches each node at its own address when that answers, and at the node the data source
/// names otherwise. What each address came to is kept, since the client asks again whenever it
/// refreshes the cluster's topology.
pub(crate) struct NearestRoute {
    named: SocketAddr,
    decided: Mutex<HashMap<SocketAddr, SocketAddr>>,
}

impl NearestRoute {
    pub(crate) fn new(named: SocketAddr) -> Self {
        Self {
            named,
            decided: Mutex::new(HashMap::new()),
        }
    }

    fn decided(&self, address: SocketAddr) -> Option<SocketAddr> {
        self.decided
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&address)
            .copied()
    }

    fn decide(&self, address: SocketAddr, route: SocketAddr) {
        self.decided
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(address, route);
    }
}

#[async_trait]
impl AddressTranslator for NearestRoute {
    async fn translate_address(
        &self,
        untranslated_peer: &UntranslatedPeer,
    ) -> Result<SocketAddr, TranslationError> {
        let address = untranslated_peer.untranslated_address();
        if let Some(route) = self.decided(address) {
            return Ok(route);
        }
        let reachable = tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(address))
            .await
            .is_ok_and(|connected| connected.is_ok());
        let route = if reachable { address } else { self.named };
        if route != address {
            tracing::debug!("node {address} is out of reach; reaching it through {route}");
        }
        self.decide(address, route);
        Ok(route)
    }
}
