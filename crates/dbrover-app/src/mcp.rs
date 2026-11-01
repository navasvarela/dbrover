//! `dbrover mcp --project <slug>`: the MCP server, run by an MCP client, with no window.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::Context as _;
use dbrover_core::describe_error;
use dbrover_mcp::{McpConfig, serve_stdio};
use dbrover_store::{
    AppPaths, DataSourceFile, HistoryFile, PREVIOUS_APP_NAME, ProjectFile, os_keychain,
};

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
    if let Some(started_as) = std::env::args_os().next()
        && is_previous_name(Path::new(&started_as))
    {
        warn_about_previous_name(Path::new(&started_as));
    }
    let config = McpConfig {
        data_sources: DataSourceFile::new(paths.data_sources_file()),
        projects: ProjectFile::new(paths.projects_file()),
        history: HistoryFile::new(paths.history_file()),
        secrets: os_keychain(),
        known_hosts: paths.ssh_known_hosts_file(),
        trusted_commands: paths.trusted_commands_file(),
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

/// Whether an MCP client started this server through the previous name: the macOS bundle keeps
/// a `dbdesk` link to `dbrover` through 0.15, so that clients configured before the rename
/// still start it after an update (see "Names" in `docs/releasing.md`).
fn is_previous_name(started_as: &Path) -> bool {
    started_as.file_name() == Some(OsStr::new(PREVIOUS_APP_NAME))
}

/// On standard error, which MCP clients show in their logs; standard output is the protocol.
fn warn_about_previous_name(started_as: &Path) {
    let message = format!(
        "{} is the name DbRover had before; it keeps working until DbRover 0.16. Copy the MCP \
         configuration again from the project's settings in DbRover to use the new name.",
        started_as.display()
    );
    tracing::warn!("{message}");
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_previous_name_from_the_current_one() {
        let cases = [
            ("/Applications/DbRover.app/Contents/MacOS/dbdesk", true),
            ("/Applications/dbdesk.app/Contents/MacOS/dbdesk", true),
            ("/Applications/dbdesk.app/Contents/MacOS/dbrover", false),
            ("dbrover", false),
        ];
        for (started_as, expected) in cases {
            assert_eq!(
                is_previous_name(Path::new(started_as)),
                expected,
                "{started_as}"
            );
        }
    }
}
