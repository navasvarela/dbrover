//! Switching the settings folder from Settings › Files: checking the folder picked, asking
//! whether to copy the settings there, and saving the choice for the next start, which copies
//! them (see `settings_folder`). See `docs/settings-folders.md`.

use std::path::{Path, PathBuf};

use dbrover_store::{
    CONFIG_DIR_FLAG, CONFIG_DIR_VARIABLE, ChosenFolder, ChosenFolderError, ChosenFolderFile,
    FolderCheck, InstallFolders, SettingsFolder, check_folder,
};
use egui::{Id, RichText};

use crate::form::{self, Segment};
use crate::theme::Palette;
use crate::widgets;

/// The settings folder this session uses, and what Settings › Files needs to change it.
#[derive(Debug, Clone)]
pub struct FolderInUse {
    pub folders: InstallFolders,
    pub source: SettingsFolder,
    pub default_settings: PathBuf,
    pub chosen_file: ChosenFolderFile,
}

impl FolderInUse {
    pub fn is_default(&self) -> bool {
        self.folders.settings == self.default_settings
    }

    /// Where the folder came from, for the line under it.
    pub fn describe_source(&self) -> String {
        match &self.source {
            SettingsFolder::Default => "The default folder.".to_owned(),
            SettingsFolder::Chosen(_) if self.is_default() => "The default folder.".to_owned(),
            SettingsFolder::Chosen(_) => "Chosen here.".to_owned(),
            SettingsFolder::Flag(_) => format!("Set by {CONFIG_DIR_FLAG} when DbRover started."),
            SettingsFolder::Environment(_) => {
                format!("Set by {CONFIG_DIR_VARIABLE} when DbRover started.")
            }
        }
    }

    /// Why the folder can't be changed here, if it can't: a choice would be ignored while the
    /// flag or the variable sets it.
    pub fn locked_because(&self) -> Option<String> {
        self.source.is_override().then(|| {
            format!(
                "Start DbRover without {CONFIG_DIR_FLAG} or {CONFIG_DIR_VARIABLE} to choose \
                 the folder here."
            )
        })
    }

    /// What switching to `folder` would do, checked before anything changes.
    pub fn switch_to(&self, folder: PathBuf) -> FolderSwitch {
        FolderSwitch {
            check: check_folder(&folder, &self.folders),
            folder,
            copy: true,
        }
    }

    /// Saves `switch` for the next start. Going back to the default folder with nothing to
    /// copy forgets the choice instead.
    ///
    /// # Errors
    ///
    /// When `settings_folder.toml` can't be written or removed.
    pub fn save(&self, switch: &PendingSwitch) -> Result<(), ChosenFolderError> {
        if switch.folder == self.default_settings && switch.copy_from.is_none() {
            return self.chosen_file.forget();
        }
        self.chosen_file.save(&ChosenFolder {
            folder: switch.folder.clone(),
            copy_from: switch.copy_from.clone(),
        })
    }
}

/// What Settings › Files asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRequest {
    /// Pick a folder, then confirm switching to it.
    Choose,
    /// Confirm switching back to the default folder.
    UseDefault,
}

/// A switch the user confirmed, saved as DbRover quits so the next start makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSwitch {
    pub folder: PathBuf,
    /// The settings folder to copy into `folder` first.
    pub copy_from: Option<PathBuf>,
}

pub enum SwitchAction {
    Cancel,
    Restart(PendingSwitch),
}

/// The window that confirms a switch, or says why the folder can't be used.
#[derive(Debug)]
pub struct FolderSwitch {
    folder: PathBuf,
    check: FolderCheck,
    /// Into an empty folder: copy the current settings, or start with none.
    copy: bool,
}

impl FolderSwitch {
    pub fn show(&mut self, ctx: &egui::Context, current: &Path) -> Option<SwitchAction> {
        let modal = widgets::dialog(ctx, Id::new("folder_switch")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            let palette = Palette::for_visuals(ui.visuals());
            ui.heading("Use another settings folder");
            ui.label(RichText::new(self.folder.display().to_string()).monospace());
            ui.add_space(4.0);
            let can_switch = self.explain(ui, current, &palette);
            form::footer(ui, |ui| {
                if !can_switch {
                    return ui.button("OK").clicked().then_some(SwitchAction::Cancel);
                }
                if form::primary_button(ui, "Restart DbRover", true).clicked() {
                    return Some(SwitchAction::Restart(self.pending(current)));
                }
                ui.button("Cancel")
                    .clicked()
                    .then_some(SwitchAction::Cancel)
            })
        });
        if modal.should_close() {
            return Some(SwitchAction::Cancel);
        }
        modal.inner
    }

    /// Says what the switch would do. Returns whether it can be made.
    fn explain(&mut self, ui: &mut egui::Ui, current: &Path, palette: &Palette) -> bool {
        match &self.check {
            FolderCheck::HasSettings => {
                ui.label(
                    "It already has DbRover's settings, which DbRover will use as they are. \
                     Nothing is copied, and the settings you use now stay where they are:",
                );
                ui.label(RichText::new(current.display().to_string()).monospace());
                true
            }
            FolderCheck::Empty => {
                ui.label("It's empty. When DbRover starts again, it can:");
                form::segmented(
                    ui,
                    &mut self.copy,
                    &[
                        Segment::new(true, "Copy my settings there"),
                        Segment::new(false, "Start with no settings"),
                    ],
                );
                form::description_label(
                    ui,
                    "Either way, the folder you use now is left as it is. Passwords stay in \
                     your keychain, and trusted commands on this machine.",
                );
                true
            }
            FolderCheck::InUse => {
                ui.label("DbRover already keeps its settings there.");
                false
            }
            FolderCheck::Refused(refusal) => {
                ui.label(
                    RichText::new(format!("This folder can't be used: {refusal}."))
                        .color(palette.error),
                );
                false
            }
        }
    }

    fn pending(&self, current: &Path) -> PendingSwitch {
        let copies = matches!(self.check, FolderCheck::Empty) && self.copy;
        PendingSwitch {
            folder: self.folder.clone(),
            copy_from: copies.then(|| current.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use tempfile::TempDir;

    use super::*;

    fn in_use(root: &Path) -> FolderInUse {
        let folders = InstallFolders {
            settings: root.join(".config/dbrover"),
            data: root.join(".local/share/dbrover"),
        };
        std::fs::create_dir_all(&folders.settings).unwrap();
        std::fs::write(folders.settings.join("data_sources.toml"), "").unwrap();
        FolderInUse {
            chosen_file: ChosenFolderFile::new(folders.data.join("settings_folder.toml")),
            default_settings: folders.settings.clone(),
            folders,
            source: SettingsFolder::Default,
        }
    }

    /// Shows the window for `folder` and clicks `button` in it, after `before`.
    fn answer(
        in_use: &FolderInUse,
        folder: PathBuf,
        before: Option<&str>,
        button: &str,
    ) -> Option<SwitchAction> {
        let mut switch = in_use.switch_to(folder);
        let current = in_use.folders.settings.clone();
        let mut answer = None;
        let mut harness = Harness::new_ui(|ui| {
            if let Some(action) = switch.show(ui.ctx(), &current) {
                answer = Some(action);
            }
        });
        harness.run();
        if let Some(label) = before {
            harness.get_by_label(label).click();
            harness.run();
        }
        harness.get_by_label(button).click();
        harness.run();
        drop(harness);
        answer
    }

    #[test]
    fn copies_into_an_empty_folder_unless_told_to_start_with_nothing() {
        let dir = TempDir::new().unwrap();
        let in_use = in_use(dir.path());
        let empty = dir.path().join("Dropbox");
        std::fs::create_dir(&empty).unwrap();

        let copied = answer(&in_use, empty.clone(), None, "Restart DbRover");
        let Some(SwitchAction::Restart(copied)) = copied else {
            panic!("expected a restart");
        };
        assert_eq!(copied.copy_from.as_ref(), Some(&in_use.folders.settings));

        let fresh = answer(
            &in_use,
            empty.clone(),
            Some("Start with no settings"),
            "Restart DbRover",
        );
        let Some(SwitchAction::Restart(fresh)) = fresh else {
            panic!("expected a restart");
        };
        assert_eq!(
            fresh,
            PendingSwitch {
                folder: empty,
                copy_from: None
            }
        );
    }

    #[test]
    fn uses_a_folder_with_settings_as_it_is_and_refuses_others() {
        let dir = TempDir::new().unwrap();
        let in_use = in_use(dir.path());
        let other_install = dir.path().join("other");
        std::fs::create_dir(&other_install).unwrap();
        std::fs::write(other_install.join("keymap.toml"), "").unwrap();
        let documents = dir.path().join("Documents");
        std::fs::create_dir(&documents).unwrap();
        std::fs::write(documents.join("letter.txt"), "").unwrap();

        let used = answer(&in_use, other_install.clone(), None, "Restart DbRover");
        assert!(matches!(
            used,
            Some(SwitchAction::Restart(PendingSwitch {
                copy_from: None,
                ..
            }))
        ));
        let refused = answer(&in_use, documents, None, "OK");
        assert!(matches!(refused, Some(SwitchAction::Cancel)));
    }

    #[test]
    fn saves_the_choice_and_forgets_it_for_the_default_folder() {
        let dir = TempDir::new().unwrap();
        let in_use = in_use(dir.path());
        let elsewhere = dir.path().join("elsewhere");
        let switch = PendingSwitch {
            folder: elsewhere.clone(),
            copy_from: Some(in_use.folders.settings.clone()),
        };

        in_use.save(&switch).unwrap();
        let saved = in_use.chosen_file.load().unwrap().unwrap();
        assert_eq!(saved.folder, elsewhere);

        in_use
            .save(&PendingSwitch {
                folder: in_use.default_settings.clone(),
                copy_from: None,
            })
            .unwrap();
        assert_eq!(in_use.chosen_file.load().unwrap(), None);
    }

    #[test]
    fn says_where_the_folder_came_from_and_whether_it_can_change() {
        let dir = TempDir::new().unwrap();
        let mut in_use = in_use(dir.path());
        assert_eq!(in_use.describe_source(), "The default folder.");
        assert_eq!(in_use.locked_because(), None);

        in_use.source = SettingsFolder::Flag(in_use.folders.settings.clone());
        assert!(in_use.describe_source().contains("--config-dir"));
        assert!(
            in_use
                .locked_because()
                .is_some_and(|reason| reason.contains("DBROVER_CONFIG_DIR"))
        );
    }
}
