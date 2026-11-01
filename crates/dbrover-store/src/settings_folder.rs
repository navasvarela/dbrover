//! Which folder holds the settings: `--config-dir`, then `DBROVER_CONFIG_DIR`, then the folder
//! chosen in Settings, then the default. See `docs/settings-folders.md`.

mod check;

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use self::check::{FolderCheck, FolderRefusal, check_folder};
use crate::atomic::write_atomically;
use crate::config_text::read_if_exists;
use crate::previous_install::{Adoption, AdoptionError, InstallFolders, adopt_previous_install};

/// The environment variable that sets the settings folder for every DbRover it starts.
pub const CONFIG_DIR_VARIABLE: &str = "DBROVER_CONFIG_DIR";

/// The command-line flag that sets the settings folder, for the app and `dbrover mcp`.
pub const CONFIG_DIR_FLAG: &str = "--config-dir";

/// The folder chosen in Settings, as `settings_folder.toml` in the data folder keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChosenFolder {
    pub folder: PathBuf,
    /// The settings folder to copy into `folder` at the next start, before anything opens
    /// either; forgotten once it is copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_from: Option<PathBuf>,
}

impl ChosenFolder {
    /// Whether this session can use the folder: it is there, or a copy will create it in a
    /// folder that is. A folder on a drive that isn't mounted is neither, and isn't created, so
    /// it can't hide the real one once the drive is back.
    pub fn is_available(&self) -> bool {
        if self.folder.is_dir() {
            return true;
        }
        self.copy_from.is_some() && self.folder.parent().is_some_and(Path::is_dir)
    }

    /// Copies `copy_from` into the folder if a switch asked for it, the way a previous install
    /// is copied: under a lock against `dbrover mcp` starting at the same moment, through a
    /// staging folder renamed into place. `data` is the data folder, which stays where it is.
    ///
    /// # Errors
    ///
    /// When the copy fails. The chosen folder is then left as it was (empty, or missing), so
    /// the next start tries again.
    pub fn copy_pending(&self, data: &Path) -> Result<Adoption, AdoptionError> {
        let Some(copy_from) = &self.copy_from else {
            return Ok(Adoption::NotNeeded);
        };
        // The copy is renamed into place, which needs the empty folder the user picked gone
        // (on Windows; elsewhere an empty folder may be replaced). A folder with files in it
        // stays, and the copy then finds there is nothing to do.
        check::remove_if_empty(&self.folder)
            .map_err(|source| AdoptionError::new(copy_from.clone(), self.folder.clone(), source))?;
        let previous = InstallFolders {
            settings: copy_from.clone(),
            data: data.to_owned(),
        };
        let current = InstallFolders {
            settings: self.folder.clone(),
            data: data.to_owned(),
        };
        adopt_previous_install(&previous, &current)
    }

    /// The choice once the pending copy is done or no longer needed.
    pub fn copied(&self) -> Self {
        Self {
            folder: self.folder.clone(),
            copy_from: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChosenFolderError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} doesn't name a settings folder", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write the settings folder as TOML")]
    Serialize(#[source] toml::ser::Error),
}

/// `settings_folder.toml`: the folder chosen in Settings. In the data folder, so it belongs to
/// this machine and never travels with the settings it points at.
#[derive(Debug, Clone)]
pub struct ChosenFolderFile {
    path: PathBuf,
}

impl ChosenFolderFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The chosen folder, or `None` when none was chosen.
    ///
    /// # Errors
    ///
    /// [`ChosenFolderError::Read`] or [`ChosenFolderError::Parse`] if the file exists but
    /// can't be used.
    pub fn load(&self) -> Result<Option<ChosenFolder>, ChosenFolderError> {
        let read = read_if_exists(&self.path).map_err(|source| ChosenFolderError::Read {
            path: self.path.clone(),
            source,
        })?;
        read.map(|text| toml::from_str(&text))
            .transpose()
            .map_err(|source| ChosenFolderError::Parse {
                path: self.path.clone(),
                source,
            })
    }

    /// # Errors
    ///
    /// [`ChosenFolderError::Write`] if the file can't be written.
    pub fn save(&self, chosen: &ChosenFolder) -> Result<(), ChosenFolderError> {
        let text = toml::to_string(chosen).map_err(ChosenFolderError::Serialize)?;
        let text = format!("# Written by DbRover: the settings folder chosen in Settings.\n{text}");
        write_atomically(&self.path, &text).map_err(|source| ChosenFolderError::Write {
            path: self.path.clone(),
            source,
        })
    }

    /// Goes back to the default folder.
    ///
    /// # Errors
    ///
    /// [`ChosenFolderError::Write`] if the file is there and can't be removed.
    pub fn forget(&self) -> Result<(), ChosenFolderError> {
        match std::fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(ChosenFolderError::Write {
                    path: self.path.clone(),
                    source: error,
                })
            }
            _ => Ok(()),
        }
    }
}

/// Where this session's settings folder comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsFolder {
    /// `~/.config/dbrover`.
    Default,
    /// `--config-dir`.
    Flag(PathBuf),
    /// `DBROVER_CONFIG_DIR`.
    Environment(PathBuf),
    /// Settings › Files.
    Chosen(ChosenFolder),
}

impl SettingsFolder {
    /// The folder, unless it is the default.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Default => None,
            Self::Flag(folder) | Self::Environment(folder) => Some(folder),
            Self::Chosen(chosen) => Some(&chosen.folder),
        }
    }

    /// Whether the folder was set for this start only, by the flag or the variable, which an
    /// MCP client won't pass on and a choice in Settings can't override.
    pub fn is_override(&self) -> bool {
        matches!(self, Self::Flag(_) | Self::Environment(_))
    }
}

/// `--config-dir` and `DBROVER_CONFIG_DIR` as given, before they are checked.
#[derive(Debug, Clone, Copy, Default)]
pub struct FolderOverrides<'a> {
    pub flag: Option<&'a OsStr>,
    pub environment: Option<&'a OsStr>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{source_name} must be an absolute path or start with ~, not {}", folder.display())]
pub struct RelativeFolder {
    source_name: &'static str,
    folder: PathBuf,
}

/// The settings folder, highest first: the flag, the variable, `chosen`, the default. An empty
/// variable counts as unset, as shells leave `DBROVER_CONFIG_DIR=` behind.
///
/// # Errors
///
/// When the flag or the variable is a relative path: an MCP client starts the server wherever
/// it likes, so a relative path would mean a different folder each time.
pub fn settings_folder(
    overrides: FolderOverrides<'_>,
    chosen: Option<ChosenFolder>,
    home: &Path,
) -> Result<SettingsFolder, RelativeFolder> {
    if let Some(flag) = overrides.flag {
        return expand(flag, home, CONFIG_DIR_FLAG).map(SettingsFolder::Flag);
    }
    if let Some(variable) = overrides.environment.filter(|value| !value.is_empty()) {
        return expand(variable, home, CONFIG_DIR_VARIABLE).map(SettingsFolder::Environment);
    }
    Ok(chosen.map_or(SettingsFolder::Default, SettingsFolder::Chosen))
}

/// `folder` made absolute: `~` is the home folder, which DbRover expands itself because MCP
/// clients don't start the server through a shell.
fn expand(
    folder: &OsStr,
    home: &Path,
    source_name: &'static str,
) -> Result<PathBuf, RelativeFolder> {
    let folder = Path::new(folder);
    let mut components = folder.components();
    if components.next() == Some(Component::Normal(OsStr::new("~"))) {
        let rest = components.as_path();
        return Ok(if rest.as_os_str().is_empty() {
            home.to_owned()
        } else {
            home.join(rest)
        });
    }
    if folder.is_absolute() {
        Ok(folder.to_owned())
    } else {
        Err(RelativeFolder {
            source_name,
            folder: folder.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests;
