//! The first start on new folders: copying what an earlier version saved, under the previous
//! name or in the platform's folders, before anything reads or creates the current folders.
//! See "Migrating existing installs" in `docs/releasing.md` and `docs/settings-folders.md`.

use std::path::PathBuf;

use dbrover_core::describe_error;
use dbrover_store::{
    Adoption, AdoptionError, AppPaths, CopiedInstall, adopt_previous_install, find_previous_install,
};
use egui::{Align, Id, Layout, Modal, RichText};

use crate::theme::Palette;

/// The folders to use this session, and what bringing over the previous install did.
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

/// Copies the newest previous install's folders to `current`'s, if they haven't been yet.
///
/// Runs before logging starts: the log folder is inside the data folder, and creating it would
/// make the copy leave out the previous window layout and trusted commands. [`Adopted::log`]
/// reports the outcome once logging is up. When the copy fails, this session uses the previous
/// folders, as the previous version did: starting empty would create the current settings
/// folder, and the copy would never be tried again.
pub fn adopt(current: AppPaths) -> Adopted {
    let Some(previous) = find_previous_install(AppPaths::previous_installs(), current.folders())
    else {
        return Adopted {
            paths: current,
            outcome: Outcome::NotNeeded,
        };
    };
    let from = previous.settings.clone();
    match adopt_previous_install(&previous, current.folders()) {
        Ok(Adoption::NotNeeded) => Adopted {
            paths: current,
            outcome: Outcome::NotNeeded,
        },
        Ok(Adoption::Copied(copied)) => Adopted {
            outcome: Outcome::Copied {
                to: current.folders().settings.clone(),
                copied,
            },
            paths: current,
        },
        Err(error) => Adopted {
            paths: current.in_folders(previous),
            outcome: Outcome::Failed { from, error },
        },
    }
}

impl Adopted {
    pub fn log(&self) {
        match &self.outcome {
            Outcome::NotNeeded => {}
            Outcome::Copied { copied, to } => {
                tracing::info!(
                    "copied the previous install from {} to {}",
                    copied.from.display(),
                    to.display()
                );
                if let Some(error) = &copied.history_read_as_files {
                    tracing::warn!(
                        "the previous history couldn't be read, so its files were copied as \
                         they are: {}",
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
                "using the previous install's folder {} this time: {}",
                from.display(),
                describe_error(error)
            ),
        }
    }

    /// What to tell the user once, if anything happened.
    pub fn notice(&self) -> Option<AdoptionNotice> {
        match &self.outcome {
            Outcome::NotNeeded => None,
            Outcome::Copied { copied, to } => Some(AdoptionNotice::Copied {
                from: copied.from.clone(),
                to: to.clone(),
            }),
            Outcome::Failed { from, error } => Some(AdoptionNotice::Failed {
                from: from.clone(),
                reason: describe_error(error),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptionNotice {
    Copied { from: PathBuf, to: PathBuf },
    Failed { from: PathBuf, reason: String },
}

impl AdoptionNotice {
    /// Shows the notice. Returns `true` once the user closes it.
    pub fn show(&self, ctx: &egui::Context) -> bool {
        let modal = Modal::new(Id::new("previous_install_notice")).show(ctx, |ui| {
            let palette = &Palette::for_visuals(ui.visuals());
            ui.set_max_width(420.0);
            match self {
                Self::Copied { from, to } => {
                    ui.heading("Your data was copied");
                    ui.label(
                        "DbRover now keeps your data sources, projects, consoles, history and \
                         settings in",
                    );
                    ui.label(RichText::new(to.display().to_string()).monospace());
                    ui.label("They were copied from the folder an earlier version used:");
                    ui.label(RichText::new(from.display().to_string()).monospace());
                    ui.label(
                        "That folder is left as it was, and isn't read any more. Saved \
                         passwords stay in your keychain.",
                    );
                }
                Self::Failed { from, reason } => {
                    ui.heading("Your data couldn't be copied");
                    ui.label(
                        "The previous version's data sources, consoles, history and settings \
                         couldn't be copied, so this time they're used where they are:",
                    );
                    ui.label(RichText::new(from.display().to_string()).monospace());
                    ui.label(RichText::new(reason).color(palette.text_weak));
                    ui.label("Nothing there was changed. The copy is tried again next time.");
                }
            }
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.button("OK").clicked()
            })
            .inner
        });
        modal.should_close() || modal.inner
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn says_where_the_data_came_from_and_closes() {
        let notices = [
            (
                AdoptionNotice::Copied {
                    from: PathBuf::from("/home/ada/.config/old-name"),
                    to: PathBuf::from("/home/ada/.config/dbrover"),
                },
                "Your data was copied",
            ),
            (
                AdoptionNotice::Failed {
                    from: PathBuf::from("/home/ada/.config/old-name"),
                    reason: "the disk is full".to_owned(),
                },
                "Your data couldn't be copied",
            ),
        ];
        for (notice, heading) in notices {
            let mut closed = false;
            let mut harness = Harness::new_ui(|ui| closed |= notice.show(ui.ctx()));
            harness.run();

            harness.get_by_label(heading);
            harness.get_by_label("/home/ada/.config/old-name");
            harness.get_by_label("OK").click();
            harness.run();
            drop(harness);
            assert!(closed, "{heading}");
        }
    }
}
