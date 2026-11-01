//! The DbRover desktop application.

mod about_dialog;
mod app;
mod brand;
mod catalog_tree;
mod command_runs;
mod commands;
mod console;
mod consoles;
mod csv_import;
mod custom_actions;
mod custom_files;
mod data_source_dialog;
mod data_source_form;
mod database_brand;
mod definition_window;
mod delete_dialog;
mod diagram;
mod explorer;
mod export_dialog;
mod export_sources_dialog;
mod feedback;
mod file_picker;
mod folder_notice;
mod fonts;
mod form;
mod hazard_dialog;
mod history_window;
mod host_key_prompt;
mod icons;
mod import_dialog;
mod json_tree;
mod keymap;
mod log_console;
#[cfg(target_os = "macos")]
mod macos;
mod mcp;
mod mcp_client_config;
mod mcp_settings;
mod menu_bar;
mod native_menu;
mod object_dialog;
mod parameters_dialog;
mod password_command_prompt;
mod password_prompt;
mod picture;
mod previous_install;
mod project_dialog;
mod relaunch;
mod renderer;
mod repaint;
mod rollback_dialog;
#[cfg(test)]
mod screenshots;
mod settings;
mod settings_folder;
mod settings_tab;
mod sql_files;
mod startup_failure;
mod state;
mod system_look;
mod telemetry;
#[cfg(test)]
mod test_support;
mod theme;
mod title_bar;
mod unsaved_dialog;
mod update_dialog;
mod updates;
mod version;
mod welcome;
mod widgets;
mod workspace;

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use dbrover_engine::{EngineConfig, GithubReleases, PackagerInstaller, ProcessRunner};
use dbrover_store::{
    APP_NAME, AppPaths, ConsoleFiles, DataSourceFile, DiagramFiles, HistoryFile, KeymapFile,
    ProjectFile, SettingsFile, os_keychain,
};

use crate::app::DbRoverApp;
use crate::log_console::LogFeed;
use crate::settings_folder::Session;

/// Also the size UI tests run at, so they see the layout a new user sees.
const DEFAULT_WINDOW_SIZE: [f32; 2] = [1440.0, 900.0];

fn main() -> anyhow::Result<()> {
    // Before anything else: `--version` must work without a display, a home directory or a
    // config folder, because the release workflow smoke-tests packaged builds with it; and
    // `mcp` must not open a window.
    let request = version::CliRequest::parse(std::env::args().skip(1));
    if request.answer() {
        return Ok(());
    }
    let (files, config_dir) = match request {
        version::CliRequest::Mcp {
            project,
            config_dir,
        } => return mcp::run(&project, config_dir.as_deref()),
        version::CliRequest::Invalid(problem) => {
            anyhow::bail!("{problem}\n\n{}", version::CliRequest::usage())
        }
        version::CliRequest::Run { files, config_dir } => (files, config_dir),
        version::CliRequest::Version | version::CliRequest::Help => (Vec::new(), None),
    };
    // Relative to where DbRover was started, which the app no longer knows once it runs.
    let files: Vec<_> = files
        .into_iter()
        .map(|file| std::path::absolute(&file).unwrap_or(file))
        .collect();
    run_window(files, config_dir.as_deref())
}

/// Opens DbRover's window and runs it until it closes. Its failures are shown to the user, not
/// only printed.
fn run_window(files: Vec<PathBuf>, config_dir: Option<&Path>) -> anyhow::Result<()> {
    let paths = AppPaths::discover()
        .context("the OS reports no home directory, so DbRover has nowhere to keep its files")
        .inspect_err(|error| startup_failure::report(error, None))?;
    let session = settings_folder::decide(paths, config_dir)
        .map_err(anyhow::Error::from)
        .inspect_err(|error| startup_failure::report(error, None))?
        .prepare(AppPaths::previous_installs());
    let logs_dir = session.paths().logs_dir();
    let telemetry =
        telemetry::init(&logs_dir).inspect_err(|error| startup_failure::report(error, None))?;
    // Lives until DbRover exits, so the log file also gets the reason for a failed start.
    let _file_guard = telemetry.file_guard;
    session.log();
    // wgpu reports some failures by panicking, and a panic would otherwise end DbRover as
    // silently as an error.
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        open_window(session, telemetry.log_feed, files)
    }))
    .unwrap_or_else(|panic| {
        Err(anyhow!(
            "DbRover stopped unexpectedly: {}",
            telemetry::panic_message(panic.as_ref())
        ))
    })
    .inspect_err(|error| startup_failure::report(error, Some(&logs_dir)))?;
    relaunch::start_if_requested();
    Ok(())
}

fn open_window(session: Session, log_feed: LogFeed, files: Vec<PathBuf>) -> anyhow::Result<()> {
    let paths = session.paths();
    let folder_notice = session.notice();
    let mcp_config_dir = session.folder_for_mcp_clients();
    let engine_config = EngineConfig {
        data_sources: DataSourceFile::new(paths.data_sources_file()),
        projects: ProjectFile::new(paths.projects_file()),
        secrets: os_keychain(),
        consoles: ConsoleFiles::new(paths.consoles_dir()),
        diagrams: DiagramFiles::new(paths.diagrams_dir()),
        history: HistoryFile::new(paths.history_file()),
        keymap: KeymapFile::new(paths.keymap_file()),
        settings: SettingsFile::new(paths.settings_file()),
        custom_files: paths.custom_files(),
        trusted_commands: paths.trusted_commands_file(),
        programs: Arc::new(ProcessRunner),
        releases: Arc::new(GithubReleases::new(
            updates::RELEASES_REPOSITORY,
            &format!("dbrover/{}", version::VERSION),
        )),
        updater: Arc::new(PackagerInstaller::new(
            updates::UPDATE_MANIFEST_URL,
            updates::UPDATE_PUBLIC_KEY,
            version::VERSION,
        )),
        known_hosts: paths.ssh_known_hosts_file(),
        idle_ping_after: dbrover_engine::IDLE_PING_AFTER,
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("DbRover")
        .with_app_id(APP_NAME)
        .with_inner_size(DEFAULT_WINDOW_SIZE);
    if let Some(icon) = brand::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    // The content runs under a transparent title bar, and DbRover draws its own top strip
    // (`title_bar`) around the window buttons. egui spells winit's
    // `with_titlebar_transparent(true)` as `with_titlebar_shown(false)`.
    #[cfg(target_os = "macos")]
    let viewport = viewport
        .with_fullsize_content_view(true)
        .with_title_shown(false)
        .with_titlebar_shown(false);
    let options = eframe::NativeOptions {
        viewport,
        // Named, because eframe's own choice is the platform's folder, which DbRover no longer
        // uses (see `docs/settings-folders.md`), and so that a session on a previous install's
        // folders (a failed copy, see `previous_install::adopt`) keeps its window state there.
        persistence_path: Some(paths.window_state_file()),
        ..Default::default()
    };
    renderer::run_native(APP_NAME, options, renderer::RENDERERS, move |cc| {
        let app = DbRoverApp::new(cc, log_feed, engine_config)?
            .with_files_to_open(files)
            .with_folder_notice(folder_notice)
            .with_mcp_config_dir(mcp_config_dir);
        #[cfg(target_os = "macos")]
        let app = app
            .with_native_menu(native_menu::NativeMenu::install(&cc.egui_ctx))
            .with_integrated_title_bar()
            .with_system_look(system_look::SystemLook::install(&cc.egui_ctx));
        Ok(Box::new(app))
    })
}
