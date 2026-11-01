//! TLS for the client's connections, with `dbrover-tls`'s configuration and ring.

use std::sync::Arc;

use dbrover_core::{BoxError, ServerConfig, TlsMode};
use rustls::pki_types::ServerName;
use scylla::client::session::TlsContext;

/// The TLS context for `server`'s mode, or `None` to connect unencrypted. CQL over TLS is a
/// port of its own on most servers (9142), so `prefer` can't try TLS and fall back on the same
/// port: it connects unencrypted, as `disable` does.
///
/// The client names each node to rustls by its IP address, so `verify-full` checks the
/// certificate against the data source's host instead (`dbrover_tls::client_config_for`).
pub(crate) fn context(server: &ServerConfig) -> Result<Option<TlsContext>, BoxError> {
    match &server.tls {
        TlsMode::Disable | TlsMode::Prefer => Ok(None),
        tls @ (TlsMode::Require | TlsMode::VerifyFull { .. }) => {
            use_ring();
            let server_name = ServerName::try_from(server.host.clone())?;
            let config = dbrover_tls::client_config_for(tls, server_name)?;
            Ok(Some(TlsContext::Rustls023(Arc::new(config))))
        }
    }
}

/// The `scylla` crate's rustls has aws-lc-rs built in beside ring, as tiberius's has, so rustls
/// can't pick a process-wide provider on its own; ring is installed as that provider, as the
/// SQL Server driver does, though the configuration above names ring itself. A provider
/// already installed stays.
fn use_ring() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
