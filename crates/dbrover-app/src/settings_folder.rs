//! Which settings folder this session uses, decided before anything opens a file in it: the
//! `--config-dir` flag, `DBROVER_CONFIG_DIR`, the folder chosen in Settings, or the default.
//! See `docs/settings-folders.md`.

use std::path::{Path, PathBuf};

use dbrover_core::describe_error;
use dbrover_store::{
    AppPaths, CONFIG_DIR_VARIABLE, ChosenFolder, ChosenFolderError, ChosenFolderFile,
    FolderOverrides, InstallFolders, RelativeFolder, SettingsFolder, settings_folder,
};

use crate::folder_notice::FolderNotice;
use crate::folder_switch::FolderInUse;
use crate::previous_install::{self, Adopted};

/// The folder the start-up settled on, before anything is copied into it.
pub struct Decision {
    default: AppPaths,
    folder: SettingsFolder,
    /// Why the folder chosen in Settings can't be used this time. `folder` is then the default.
    unavailable: Option<Unavailable>,
}

#[derive(Debug, thiserror::Error)]
pub enum Unavailable {
    #[error(
        "the settings folder chosen in Settings, {}, isn't there; connect the drive it is on, or \
         choose another in DbRover's Settings › Files",
        .0.display()
    )]
    Missing(PathBuf),
    #[error("could not tell which settings folder was chosen in Settings")]
    Unreadable(#[source] ChosenFolderError),
}

/// Decides the settings folder from `flag` and `DBROVER_CONFIG_DIR`, then the choice saved in
/// `default`'s data folder.
///
/// # Errors
///
/// When the flag or the variable is a relative path.
pub fn decide(default: AppPaths, flag: Option<&Path>) -> Result<Decision, RelativeFolder> {
    let variable = std::env::var_os(CONFIG_DIR_VARIABLE);
    let overrides = FolderOverrides {
        flag: flag.map(Path::as_os_str),
        environment: variable.as_deref(),
    };
    decide_with(default, overrides)
}

fn decide_with(
    default: AppPaths,
    overrides: FolderOverrides<'_>,
) -> Result<Decision, RelativeFolder> {
    let (chosen, unreadable) = match default.chosen_folder_file().load() {
        Ok(chosen) => (chosen, None),
        Err(error) => (None, Some(Unavailable::Unreadable(error))),
    };
    let folder = settings_folder(overrides, chosen, default.home())?;
    let (folder, unavailable) = match folder {
        SettingsFolder::Chosen(chosen) if !chosen.is_available() => (
            SettingsFolder::Default,
            Some(Unavailable::Missing(chosen.folder)),
        ),
        // An unreadable choice only matters when nothing overrides it.
        SettingsFolder::Default => (SettingsFolder::Default, unreadable),
        folder => (folder, None),
    };
    Ok(Decision {
        default,
        folder,
        unavailable,
    })
}

impl Decision {
    /// Why the chosen folder can't be used. The app then uses the default folder for the
    /// session; `dbrover mcp` stops instead, rather than serve projects the client would take
    /// for the chosen folder's.
    pub fn unavailable(&self) -> Option<&Unavailable> {
        self.unavailable.as_ref()
    }

    /// Copies whatever needs copying into the folder, before anything opens it: the newest of
    /// `previous_installs` into the default folder, or the settings the user switched from
    /// into the one they chose.
    pub fn prepare(self, previous_installs: Vec<InstallFolders>) -> Session {
        let Self {
            default,
            folder,
            unavailable,
        } = self;
        let adopted = match &folder {
            SettingsFolder::Default => previous_install::adopt(default.clone(), previous_installs),
            SettingsFolder::Flag(path) | SettingsFolder::Environment(path) => {
                Adopted::unchanged(default.with_settings_folder(path))
            }
            SettingsFolder::Chosen(chosen) => switch_to(&default, chosen),
        };
        Session {
            adopted,
            folder,
            default_settings: default.folders().settings.clone(),
            chosen_file: default.chosen_folder_file(),
            unavailable,
        }
    }
}

/// The chosen folder, after the copy a switch asked for. Once it is done, or found to be
/// unneeded, the choice no longer asks for it.
fn switch_to(default: &AppPaths, chosen: &ChosenFolder) -> Adopted {
    let current = default.with_settings_folder(&chosen.folder);
    let Some(copy_from) = &chosen.copy_from else {
        return Adopted::unchanged(current);
    };
    let fallback = default.with_settings_folder(copy_from);
    let adopted = previous_install::settle(
        chosen.copy_pending(&default.folders().data),
        current,
        fallback,
    );
    if !adopted.failed() {
        forget_copy(&default.chosen_folder_file(), chosen, default);
    }
    adopted
}

/// Saves `chosen` without its pending copy, or forgets it when it is the default folder. If
/// that fails the next start finds the folder copied already, so it is only logged.
fn forget_copy(file: &ChosenFolderFile, chosen: &ChosenFolder, default: &AppPaths) {
    let saved = if chosen.folder == default.folders().settings {
        file.forget()
    } else {
        file.save(&chosen.copied())
    };
    if let Err(error) = saved {
        tracing::warn!("{}", describe_error(&error));
    }
}

/// The folders this session uses, and how it came to them.
pub struct Session {
    pub adopted: Adopted,
    pub folder: SettingsFolder,
    default_settings: PathBuf,
    /// In the default data folder, even when a failed copy leaves this session on a previous
    /// install's folders.
    chosen_file: ChosenFolderFile,
    unavailable: Option<Unavailable>,
}

impl Session {
    pub fn paths(&self) -> &AppPaths {
        &self.adopted.paths
    }

    pub fn log(&self) {
        self.adopted.log();
        match &self.folder {
            SettingsFolder::Default => {}
            SettingsFolder::Flag(path) => {
                tracing::info!("settings folder {} (--config-dir)", path.display());
            }
            SettingsFolder::Environment(path) => {
                tracing::info!("settings folder {} ({CONFIG_DIR_VARIABLE})", path.display());
            }
            SettingsFolder::Chosen(chosen) => {
                tracing::info!("settings folder {} (chosen)", chosen.folder.display());
            }
        }
        if let Some(unavailable) = &self.unavailable {
            tracing::warn!(
                "using the default settings folder: {}",
                describe_error(unavailable)
            );
        }
    }

    /// What to tell the user once: that the chosen folder isn't available, before what a copy
    /// did, since only the first needs them to act.
    pub fn notice(&self) -> Option<FolderNotice> {
        match &self.unavailable {
            Some(unavailable) => Some(FolderNotice::ChoiceUnavailable {
                problem: describe_error(unavailable),
                default: self.default_settings.clone(),
            }),
            None => self.adopted.notice(),
        }
    }

    /// The folder in use, for Settings › Files to show and change.
    pub fn folder_in_use(&self) -> FolderInUse {
        FolderInUse {
            folders: self.paths().folders().clone(),
            source: self.folder.clone(),
            default_settings: self.default_settings.clone(),
            chosen_file: self.chosen_file.clone(),
        }
    }

    /// The folder an MCP client must be told about with `--config-dir`, since it gets neither
    /// this session's flag nor its environment. A folder chosen in Settings it finds itself.
    pub fn folder_for_mcp_clients(&self) -> Option<PathBuf> {
        self.folder
            .is_override()
            .then(|| self.paths().folders().settings.clone())
    }
}

#[cfg(test)]
mod tests;
