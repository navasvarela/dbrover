use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};

use crate::{CustomFiles, InstallFolders, TrustedCommandsFile};

/// The application name used for platform directories and the OS keychain. Also the eframe app
/// id, so the window/layout state eframe persists lands in the same folder.
pub const APP_NAME: &str = "dbrover";

/// The name the app had before [`APP_NAME`], whose folders the first start under the new name
/// copies and whose keychain entries it moves on first use (see "Renaming to DbRover" in
/// `docs/releasing.md`). While it equals [`APP_NAME`] there is nothing to bring over.
pub const PREVIOUS_APP_NAME: &str = "dbdesk";

/// The query history database's file name, in the settings folder.
pub(crate) const HISTORY_FILE_NAME: &str = "history.sqlite";

/// DbRover's directories on this machine (`~/Library/Application Support/dbrover` on macOS).
#[derive(Debug, Clone)]
pub struct AppPaths {
    dirs: ProjectDirs,
    home: PathBuf,
}

impl AppPaths {
    /// Resolves the platform directories, or `None` if the OS reports no home directory.
    pub fn discover() -> Option<Self> {
        Self::named(APP_NAME)
    }

    /// The directories of [`PREVIOUS_APP_NAME`], which may not exist.
    pub fn discover_previous() -> Option<Self> {
        Self::named(PREVIOUS_APP_NAME)
    }

    fn named(name: &str) -> Option<Self> {
        Some(Self {
            dirs: ProjectDirs::from("", "", name)?,
            home: BaseDirs::new()?.home_dir().to_owned(),
        })
    }

    /// The folders this name keeps its files in.
    pub fn folders(&self) -> InstallFolders {
        InstallFolders::of(&self.dirs)
    }

    /// OpenSSH's file of trusted host keys, which SSH tunnels share with `ssh`. Not
    /// guaranteed to exist.
    pub fn ssh_known_hosts_file(&self) -> PathBuf {
        self.home.join(".ssh").join("known_hosts")
    }

    /// eframe's saved state: the window, the layout and open tabs. Where eframe would put it
    /// for [`APP_NAME`], but following these paths when they are another name's. Not guaranteed
    /// to exist yet.
    pub fn window_state_file(&self) -> PathBuf {
        self.dirs.data_dir().join("app.ron")
    }

    /// Rolling log files. Not guaranteed to exist yet.
    pub fn logs_dir(&self) -> PathBuf {
        self.dirs.data_dir().join("logs")
    }

    /// Saved data sources. Not guaranteed to exist yet.
    pub fn data_sources_file(&self) -> PathBuf {
        self.dirs.config_dir().join("data_sources.toml")
    }

    /// Projects and their MCP permissions. Not guaranteed to exist yet.
    pub fn projects_file(&self) -> PathBuf {
        self.dirs.config_dir().join("projects.toml")
    }

    /// ER diagram files, one folder per project. Not guaranteed to exist yet.
    pub fn diagrams_dir(&self) -> PathBuf {
        self.dirs.config_dir().join("diagrams")
    }

    /// Query console files, one folder per data source. Not guaranteed to exist yet.
    pub fn consoles_dir(&self) -> PathBuf {
        self.dirs.config_dir().join("consoles")
    }

    /// Key bindings, if the user wrote any. Not guaranteed to exist.
    pub fn keymap_file(&self) -> PathBuf {
        self.dirs.config_dir().join("keymap.toml")
    }

    /// Preferences, such as how SQL is formatted. Not guaranteed to exist.
    pub fn settings_file(&self) -> PathBuf {
        self.dirs.config_dir().join("settings.toml")
    }

    /// `actions.toml`, `snippets.toml` and `theme.toml`, if the user wrote them, beside the
    /// keymap.
    pub fn custom_files(&self) -> CustomFiles {
        CustomFiles::new(self.dirs.config_dir())
    }

    /// The command actions the user chose to run. In the data folder rather than beside
    /// `actions.toml`, so a config folder copied from elsewhere brings no trust with it.
    pub fn trusted_commands_file(&self) -> TrustedCommandsFile {
        TrustedCommandsFile::new(self.dirs.data_dir().join("trusted_commands.toml"))
    }

    /// The query history database. Not guaranteed to exist yet.
    pub fn history_file(&self) -> PathBuf {
        self.dirs.config_dir().join(HISTORY_FILE_NAME)
    }
}
