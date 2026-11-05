//! The driver against Trino's container (`server`), which runs nightly, and against a
//! coordinator on this machine that answers from a list (`protocol`), on every pull request.

mod protocol;
mod server;
mod stand_in;

use dbrover_core::{Port, ServerConfig, TlsMode, TrinoConfig, TrinoProtocol};

/// A data source on the coordinator at `host:port`, as `user`, starting in `catalog.schema`.
fn config(host: &str, port: u16, user: &str, catalog: &str, schema: &str) -> TrinoConfig {
    TrinoConfig {
        server: ServerConfig {
            host: host.to_owned(),
            port: Port::Number(port),
            database: catalog.to_owned(),
            user: user.to_owned(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        },
        schema: schema.to_owned(),
        protocol: TrinoProtocol::Trino,
    }
}
