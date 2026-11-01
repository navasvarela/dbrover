//! DbRover's MCP server: lets an AI assistant browse, query and, where the project allows it,
//! change the data sources of one project, through the Model Context Protocol. See
//! `docs/mcp-server.md`.
//!
//! The app's binary runs it as `dbrover mcp --project <slug>`, over standard input and output;
//! this crate holds everything but the command line, so it can be tested without the app.
//!
//! Reading is enforced twice: [`dbrover_sql::analyze`] refuses any statement that isn't a
//! read (or can't be parsed) before it is sent, and every read runs inside the database's
//! own read-only transaction, which catches what the parser can't see, such as a function
//! that writes. Changes go through the `execute` tool only, each statement allowed by the
//! project's permission for its kind; beyond reading, only the grants of a data source's MCP
//! login are airtight.

mod audit;
mod catalog;
mod connections;
mod execute;
mod project;
mod scope;
mod server;
mod transaction;

use std::path::PathBuf;
use std::sync::Arc;

use dbrover_engine::{PasswordCommands, ProcessRunner, Tunnels};
use dbrover_store::{DataSourceFile, HistoryFile, ProjectFile, SecretStore, TrustedCommandsFile};
use rmcp::ServiceExt as _;
use rmcp::service::ServerInitializeError;
use tokio::io::{AsyncRead, AsyncWrite};

pub use project::{Changes, Limits, Permission, ServedProject, StartError};

use crate::audit::Audit;
use crate::connections::Connections;
use crate::server::DbRoverMcp;

/// Where the server finds what the app saved.
pub struct McpConfig {
    pub data_sources: DataSourceFile,
    pub projects: ProjectFile,
    pub history: HistoryFile,
    pub secrets: Arc<dyn SecretStore>,
    /// OpenSSH's `known_hosts`, which SSH tunnels check host keys against.
    pub known_hosts: PathBuf,
    /// The password commands the user trusted in the app, the only ones the server runs.
    pub trusted_commands: TrustedCommandsFile,
}

/// [`StartError`] is boxed: it carries paths, I/O and TOML errors, which made this type large
/// enough on Windows for clippy's `result_large_err`.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error(transparent)]
    Start(Box<StartError>),
    #[error("the MCP client's handshake failed")]
    Initialize(#[source] Box<ServerInitializeError>),
    #[error("the MCP session ended with an error")]
    Session(#[source] tokio::task::JoinError),
}

/// Beyond reading, DbRover's own checks can be fooled (a function can do anything), so only a
/// login whose grants the database enforces makes the permissions airtight.
fn warn_about_shared_logins(project: &ServedProject) {
    for data_source in &project.data_sources {
        if data_source.mcp_login.is_none() && data_source.config.driver().has_logins() {
            tracing::warn!(
                project = project.slug(),
                data_source = %data_source.name,
                "this project allows changes, and the data source has no MCP login of its own: \
                 only a dedicated login's grants are airtight beyond reading"
            );
        }
    }
}

/// Serves the project with `slug` over standard input and output until the client closes
/// them. Nothing else may write to standard output: it carries the protocol.
///
/// # Errors
///
/// [`ServeError`] if the project can't be served or the session fails.
pub async fn serve_stdio(config: McpConfig, slug: &str) -> Result<(), ServeError> {
    serve(config, slug, (tokio::io::stdin(), tokio::io::stdout())).await
}

/// [`serve_stdio`] over any pair of streams, for tests.
///
/// # Errors
///
/// [`ServeError`] if the project can't be served or the session fails.
pub async fn serve<R, W>(config: McpConfig, slug: &str, transport: (R, W)) -> Result<(), ServeError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let project = ServedProject::load(&config.projects, &config.data_sources, slug)
        .map_err(|error| ServeError::Start(Box::new(error)))?;
    let allowed = project.changes.names();
    tracing::info!(
        project = project.slug(),
        data_sources = project.data_sources.len(),
        max_rows = project.limits.max_rows,
        "serving the project to an MCP client: {}",
        if allowed.is_empty() {
            "read-only".to_owned()
        } else {
            format!("reading, and {}", allowed.join(", "))
        }
    );
    if !allowed.is_empty() {
        warn_about_shared_logins(&project);
    }
    let audit = Audit::open(project.slug(), &config.history);
    let commands = PasswordCommands::load(&config.trusted_commands, Arc::new(ProcessRunner)).await;
    let connections = Connections::new(
        config.secrets,
        Tunnels::new(config.known_hosts),
        commands,
        &project.data_sources,
    );
    connections.sweep_idle();
    let server = DbRoverMcp::new(project, Arc::clone(&connections), audit);
    let running = server
        .serve(transport)
        .await
        .map_err(|error| ServeError::Initialize(Box::new(error)))?;
    let quit = running.waiting().await.map_err(ServeError::Session)?;
    tracing::info!("the MCP client disconnected ({quit:?})");
    connections.close_all().await;
    Ok(())
}
