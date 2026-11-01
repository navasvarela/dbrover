//! The window that says once what happened to the settings folder at start-up: copied, not
//! copied, or not where the user chose.

use std::path::PathBuf;

use egui::{Align, Id, Layout, RichText};

use crate::theme::Palette;
use crate::widgets;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderNotice {
    /// The settings were copied from `from` to `to`, which this session uses.
    Copied { from: PathBuf, to: PathBuf },
    /// They couldn't be, and this session uses `from`.
    Failed { from: PathBuf, reason: String },
    /// The folder chosen in Settings can't be used, and this session uses `default`.
    ChoiceUnavailable { problem: String, default: PathBuf },
}

impl FolderNotice {
    /// Shows the notice. Returns `true` once the user closes it.
    pub fn show(&self, ctx: &egui::Context) -> bool {
        let modal = widgets::dialog(ctx, Id::new("folder_notice")).show(ctx, |ui| {
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
                    ui.label("They were copied from:");
                    ui.label(RichText::new(from.display().to_string()).monospace());
                    ui.label(
                        "That folder is left as it was, and isn't read any more. Saved \
                         passwords stay in your keychain.",
                    );
                }
                Self::Failed { from, reason } => {
                    ui.heading("Your data couldn't be copied");
                    ui.label(
                        "Your data sources, consoles, history and settings couldn't be copied, \
                         so this time they're used where they are:",
                    );
                    ui.label(RichText::new(from.display().to_string()).monospace());
                    ui.label(RichText::new(reason).color(palette.text_weak));
                    ui.label("Nothing there was changed. The copy is tried again next time.");
                }
                Self::ChoiceUnavailable { problem, default } => {
                    ui.heading("Your settings folder isn't available");
                    ui.label(RichText::new(problem).color(palette.text_weak));
                    ui.label("This time DbRover uses the default folder:");
                    ui.label(RichText::new(default.display().to_string()).monospace());
                    ui.label(
                        "Your choice is kept, so the folder is used again once it's back. \
                         Settings › Files changes it.",
                    );
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
                FolderNotice::Copied {
                    from: PathBuf::from("/home/ada/.config/old-name"),
                    to: PathBuf::from("/home/ada/.config/dbrover"),
                },
                "Your data was copied",
            ),
            (
                FolderNotice::Failed {
                    from: PathBuf::from("/home/ada/.config/old-name"),
                    reason: "the disk is full".to_owned(),
                },
                "Your data couldn't be copied",
            ),
            (
                FolderNotice::ChoiceUnavailable {
                    problem: "/home/ada/.config/old-name isn't there".to_owned(),
                    default: PathBuf::from("/home/ada/.config/dbrover"),
                },
                "Your settings folder isn't available",
            ),
        ];
        for (notice, heading) in notices {
            let mut closed = false;
            let mut harness = Harness::new_ui(|ui| closed |= notice.show(ui.ctx()));
            harness.run();

            harness.get_by_label(heading);
            harness.get_by_label_contains("/home/ada/.config/old-name");
            harness.get_by_label("OK").click();
            harness.run();
            drop(harness);
            assert!(closed, "{heading}");
        }
    }
}
