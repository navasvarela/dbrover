use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

use crate::{ChosenFolderFile, CustomFileKind, CustomFiles, InstallFolders, TrustedCommandsFile};

/// The application name used for DbRover's folders and the OS keychain. Also the eframe app
/// id.
pub const APP_NAME: &str = "dbrover";

/// The name the app had before [`APP_NAME`], whose folders the first start under the new name
/// copies and whose keychain entries it moves on first use (see "Renaming to DbRover" in
/// `docs/releasing.md`). While it equals [`APP_NAME`] there is nothing to bring over.
pub const PREVIOUS_APP_NAME: &str = "dbdesk";

/// The query history database's file name, in the settings folder.
pub(crate) const HISTORY_FILE_NAME: &str = "history.sqlite";

const DATA_SOURCES_FILE_NAME: &str = "data_sources.toml";
const PROJECTS_FILE_NAME: &str = "projects.toml";
const DIAGRAMS_FOLDER_NAME: &str = "diagrams";
const CONSOLES_FOLDER_NAME: &str = "consoles";
const KEYMAP_FILE_NAME: &str = "keymap.toml";
const SETTINGS_FILE_NAME: &str = "settings.toml";

/// Everything DbRover may keep in a settings folder. A folder with any of them holds someone's
/// settings, and choosing it uses them rather than copying over them.
pub(crate) const SETTINGS_FOLDER_ENTRIES: [&str; 10] = [
    DATA_SOURCES_FILE_NAME,
    PROJECTS_FILE_NAME,
    SETTINGS_FILE_NAME,
    KEYMAP_FILE_NAME,
    CustomFileKind::Actions.file_name(),
    CustomFileKind::Snippets.file_name(),
    CustomFileKind::Theme.file_name(),
    CONSOLES_FOLDER_NAME,
    DIAGRAMS_FOLDER_NAME,
    HISTORY_FILE_NAME,
];

/// eframe's saved state, in the data folder.
pub(crate) const WINDOW_STATE_FILE_NAME: &str = "app.ron";

/// The commands the user trusted, in the data folder.
pub(crate) const TRUSTED_COMMANDS_FILE_NAME: &str = "trusted_commands.toml";

/// The folder chosen in Settings, in the data folder.
const CHOSEN_FOLDER_FILE_NAME: &str = "settings_folder.toml";

/// DbRover's folders on this machine: `~/.config/dbrover` for the settings and
/// `~/.local/share/dbrover` for the machine's own files. See `docs/settings-folders.md`.
#[derive(Debug, Clone)]
pub struct AppPaths {
    folders: InstallFolders,
    home: PathBuf,
}

impl AppPaths {
    /// DbRover's default folders, or `None` if the OS reports no home directory.
    pub fn discover() -> Option<Self> {
        let base = BaseDirs::new()?;
        Some(Self {
            folders: default_folders(&base),
            home: base.home_dir().to_owned(),
        })
    }

    /// The default folders in `home`, as on macOS and Windows, or on Linux with the XDG
    /// variables unset. For tests and tools that must not touch the real folders.
    pub fn under_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            folders: InstallFolders::under_home(&home),
            home,
        }
    }

    /// Where earlier versions kept their files, newest first, whether or not they exist: this
    /// name's platform folders (`~/Library/Application Support/dbrover` on macOS), then
    /// [`PREVIOUS_APP_NAME`]'s.
    pub fn previous_installs() -> Vec<InstallFolders> {
        [APP_NAME, PREVIOUS_APP_NAME]
            .into_iter()
            .filter_map(|name| ProjectDirs::from("", "", name))
            .map(|dirs| InstallFolders::of(&dirs))
            .collect()
    }

    /// These paths, but in `folders`: a previous install's, for a session whose copy failed.
    pub fn in_folders(&self, folders: InstallFolders) -> Self {
        Self {
            folders,
            home: self.home.clone(),
        }
    }

    /// The folders this name keeps its files in.
    /// These paths, but with their settings in `folder`. The data folder stays: it is this
    /// machine's, whichever settings are in use.
    pub fn with_settings_folder(&self, folder: impl Into<PathBuf>) -> Self {
        self.in_folders(InstallFolders {
            settings: folder.into(),
            data: self.folders.data.clone(),
        })
    }

    /// The user's home folder, which `~` in a folder given to DbRover stands for.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The folder chosen in Settings, if one was. In the data folder, so the choice belongs to
    /// this machine and never travels with the settings it points at.
    pub fn chosen_folder_file(&self) -> ChosenFolderFile {
        ChosenFolderFile::new(self.folders.data.join(CHOSEN_FOLDER_FILE_NAME))
    }

    pub fn folders(&self) -> &InstallFolders {
        &self.folders
    }

    /// OpenSSH's file of trusted host keys, which SSH tunnels share with `ssh`. Not
    /// guaranteed to exist.
    pub fn ssh_known_hosts_file(&self) -> PathBuf {
        self.home.join(".ssh").join("known_hosts")
    }

    /// eframe's saved state: the window, the layout and open tabs. Not guaranteed to exist
    /// yet.
    pub fn window_state_file(&self) -> PathBuf {
        self.folders.data.join(WINDOW_STATE_FILE_NAME)
    }

    /// Rolling log files. Not guaranteed to exist yet.
    pub fn logs_dir(&self) -> PathBuf {
        self.folders.data.join("logs")
    }

    /// Saved data sources. Not guaranteed to exist yet.
    pub fn data_sources_file(&self) -> PathBuf {
        self.folders.settings.join(DATA_SOURCES_FILE_NAME)
    }

    /// Projects and their MCP permissions. Not guaranteed to exist yet.
    pub fn projects_file(&self) -> PathBuf {
        self.folders.settings.join(PROJECTS_FILE_NAME)
    }

    /// ER diagram files, one folder per project. Not guaranteed to exist yet.
    pub fn diagrams_dir(&self) -> PathBuf {
        self.folders.settings.join(DIAGRAMS_FOLDER_NAME)
    }

    /// Query console files, one folder per data source. Not guaranteed to exist yet.
    pub fn consoles_dir(&self) -> PathBuf {
        self.folders.settings.join(CONSOLES_FOLDER_NAME)
    }

    /// Key bindings, if the user wrote any. Not guaranteed to exist.
    pub fn keymap_file(&self) -> PathBuf {
        self.folders.settings.join(KEYMAP_FILE_NAME)
    }

    /// Preferences, such as how SQL is formatted. Not guaranteed to exist.
    pub fn settings_file(&self) -> PathBuf {
        self.folders.settings.join(SETTINGS_FILE_NAME)
    }

    /// `actions.toml`, `snippets.toml` and `theme.toml`, if the user wrote them, beside the
    /// keymap.
    pub fn custom_files(&self) -> CustomFiles {
        CustomFiles::new(&self.folders.settings)
    }

    /// The command actions the user chose to run. In the data folder rather than beside
    /// `actions.toml`, so a config folder copied from elsewhere brings no trust with it.
    pub fn trusted_commands_file(&self) -> TrustedCommandsFile {
        TrustedCommandsFile::new(self.folders.data.join(TRUSTED_COMMANDS_FILE_NAME))
    }

    /// The query history database. Not guaranteed to exist yet.
    pub fn history_file(&self) -> PathBuf {
        self.folders.settings.join(HISTORY_FILE_NAME)
    }
}

/// `~/.config/dbrover` and `~/.local/share/dbrover`. On Linux and the BSDs the XDG variables
/// move them, as they always have; on macOS and Windows they don't count, because an app
/// started from the Dock or the Start menu doesn't see a shell's variables, and would use
/// another folder than the same app started from a terminal.
fn default_folders(base: &BaseDirs) -> InstallFolders {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        InstallFolders::under_home(base.home_dir())
    } else {
        InstallFolders {
            settings: base.config_dir().join(APP_NAME),
            data: base.data_dir().join(APP_NAME),
        }
    }
}

impl InstallFolders {
    /// The default folders in `home`, ignoring the XDG variables.
    fn under_home(home: &Path) -> Self {
        Self {
            settings: home.join(".config").join(APP_NAME),
            data: home.join(".local").join("share").join(APP_NAME),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_settings_in_dot_config_and_the_rest_in_dot_local_share() {
        let folders = InstallFolders::under_home(Path::new("/home/ada"));

        assert_eq!(folders.settings, Path::new("/home/ada/.config/dbrover"));
        assert_eq!(folders.data, Path::new("/home/ada/.local/share/dbrover"));
    }

    #[test]
    fn the_default_folders_are_under_the_home_folder() {
        let Some(paths) = AppPaths::discover() else {
            return;
        };
        let folders = paths.folders();
        assert!(folders.settings.ends_with("dbrover"));
        assert_ne!(folders.settings, folders.data);
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert_eq!(folders, &InstallFolders::under_home(&paths.home));
        }
    }

    #[test]
    fn previous_installs_are_this_name_then_the_previous_one() {
        let previous = AppPaths::previous_installs();

        assert_eq!(previous.len(), 2);
        assert!(previous[0].settings.to_string_lossy().contains(APP_NAME));
        assert!(
            previous[1]
                .settings
                .to_string_lossy()
                .contains(PREVIOUS_APP_NAME)
        );
    }
}
