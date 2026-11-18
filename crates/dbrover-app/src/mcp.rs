//! `dbrover mcp --project <slug>`: the MCP server, run by an MCP client, with no window.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use dbrover_core::describe_error;
use dbrover_mcp::{McpConfig, serve_stdio};
use dbrover_store::{AppPaths, DataSourceFile, HistoryFile, ProjectFile, os_keychain};

use crate::mcp_reports::McpReports;
use crate::{settings_folder, telemetry};

/// Serves the project until the client closes standard input. Needs no display: nothing here
/// touches the windowing stack, so it runs on a headless machine or over SSH.
pub fn run(project: &str, config_dir: Option<&Path>) -> anyhow::Result<()> {
    let default = AppPaths::discover()
        .context("the OS reports no home directory, so DbRover can't find its files")?;
    let decision = settings_folder::decide(default, config_dir)?;
    if let Some(unavailable) = decision.unavailable() {
        return Err(anyhow::anyhow!("{}", describe_error(unavailable)));
    }
    // Whichever starts first, the app or an MCP client's server, brings over a previous
    // install, or the settings a switch in Settings asked to copy.
    let session = decision.prepare(AppPaths::previous_installs());
    let paths = session.paths();
    let _file_guard = telemetry::init_headless(&paths.logs_dir())?;
    session.log();
    let config = McpConfig {
        data_sources: DataSourceFile::new(paths.data_sources_file()),
        projects: ProjectFile::new(paths.projects_file()),
        history: HistoryFile::new(paths.history_file()),
        secrets: os_keychain(),
        known_hosts: paths.ssh_known_hosts_file(),
        trusted_commands: paths.trusted_commands_file(),
        reports: Some(Arc::new(McpReports {
            paths: paths.clone(),
            folder: crate::default_report_folder(paths),
        })),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("dbrover-mcp")
        .enable_all()
        .build()
        .context("could not start the server's runtime")?;
    let served = runtime.block_on(serve_stdio(config, project));
    if let Err(error) = &served {
        tracing::error!(project, "the MCP server stopped: {error:#}");
    }
    served.with_context(|| format!("could not serve the project {project:?}"))
}
