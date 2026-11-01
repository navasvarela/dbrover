//! The first start on new folders: copying what an earlier version saved, under the previous
//! name or in the platform's folders, before anything reads or creates the current folders.
//! See "Migrating existing installs" in `docs/releasing.md` and `docs/settings-folders.md`.

use std::path::PathBuf;

use dbrover_core::describe_error;
use dbrover_store::{
    Adoption, AdoptionError, AppPaths, CopiedInstall, InstallFolders, adopt_previous_install,
    find_previous_install,
};

use crate::folder_notice::FolderNotice;

/// The folders to use this session, and what bringing over a previous install, or a settings
/// folder the user switched from, did.
pub struct Adopted {
    pub paths: AppPaths,
    outcome: Outcome,
}

enum Outcome {
    NotNeeded,
    /// `to` is the settings folder it was copied to.
    Copied {
        copied: CopiedInstall,
        to: PathBuf,
    },
    /// `paths` are the previous folders this session; `from` is their settings folder.
    Failed {
        from: PathBuf,
        error: AdoptionError,
    },
}

/// Copies the newest of `previous_installs` to `current`'s folders, if they haven't been yet.
///
/// Runs before logging starts: the log folder is inside the data folder, and creating it would
/// make the copy leave out the previous window layout and trusted commands. [`Adopted::log`]
/// reports the outcome once logging is up. When the copy fails, this session uses the previous
/// folders, as the previous version did: starting empty would create the current settings
/// folder, and the copy would never be tried again.
pub fn adopt(current: AppPaths, previous_installs: Vec<InstallFolders>) -> Adopted {
    let Some(previous) = find_previous_install(previous_installs, current.folders()) else {
        return Adopted::unchanged(current);
    };
    let copy = adopt_previous_install(&previous, current.folders());
    let fallback = current.in_folders(previous);
    settle(copy, current, fallback)
}

/// The folders this session uses after a copy into `current`: `fallback`, whose settings folder
/// is the one copied from, when the copy failed.
pub fn settle(
    copy: Result<Adoption, AdoptionError>,
    current: AppPaths,
    fallback: AppPaths,
) -> Adopted {
    match copy {
        Ok(Adoption::NotNeeded) => Adopted::unchanged(current),
        Ok(Adoption::Copied(copied)) => Adopted {
            outcome: Outcome::Copied {
                to: current.folders().settings.clone(),
                copied,
            },
            paths: current,
        },
        Err(error) => Adopted {
            outcome: Outcome::Failed {
                from: fallback.folders().settings.clone(),
                error,
            },
            paths: fallback,
        },
    }
}

impl Adopted {
    /// `paths`, with nothing copied.
    pub fn unchanged(paths: AppPaths) -> Self {
        Self {
            paths,
            outcome: Outcome::NotNeeded,
        }
    }

    /// Whether the copy failed, and this session uses the folder it was copied from.
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed { .. })
    }

    pub fn log(&self) {
        match &self.outcome {
            Outcome::NotNeeded => {}
            Outcome::Copied { copied, to } => {
                tracing::info!(
                    "copied the settings from {} to {}",
                    copied.from.display(),
                    to.display()
                );
                if let Some(error) = &copied.history_read_as_files {
                    tracing::warn!(
                        "the history couldn't be read, so its files were copied as they are: {}",
                        describe_error(error)
                    );
                }
                if let Some(error) = &copied.note_not_written {
                    tracing::warn!(
                        "could not leave a note in the previous folder: {}",
                        describe_error(error)
                    );
                }
            }
            Outcome::Failed { from, error } => tracing::error!(
                "using {} this time, because it couldn't be copied: {}",
                from.display(),
                describe_error(error)
            ),
        }
    }

    /// What to tell the user once, if anything happened.
    pub fn notice(&self) -> Option<FolderNotice> {
        match &self.outcome {
            Outcome::NotNeeded => None,
            Outcome::Copied { copied, to } => Some(FolderNotice::Copied {
                from: copied.from.clone(),
                to: to.clone(),
            }),
            Outcome::Failed { from, error } => Some(FolderNotice::Failed {
                from: from.clone(),
                reason: describe_error(error),
            }),
        }
    }
}
