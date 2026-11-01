use dbrover_engine::{InstallSupport, Restart};
use egui::{Align, Id, Layout, OpenUrl, ProgressBar, RichText};

use crate::form;
use crate::theme::Palette;
use crate::updates::{InstallState, UpdateStatus, Updates};
use crate::version;
use crate::widgets;

/// What the user asked of the updates dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    Close,
    Install,
    Restart,
}

/// Help › Check for updates, and the status bar's notice: whether a newer release is out,
/// installing it when this copy can replace itself, and its release page.
pub fn show(ctx: &egui::Context, updates: &Updates) -> Option<UpdateAction> {
    let modal = widgets::dialog(ctx, Id::new("check_for_updates")).show(ctx, |ui| {
        let palette = &Palette::for_visuals(ui.visuals());
        ui.set_min_width(360.0);
        ui.heading("Updates");
        ui.add_space(ui.spacing().item_spacing.y);
        let mut page = None;
        match updates.status() {
            UpdateStatus::NotChecked | UpdateStatus::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking for a newer release…");
                });
            }
            UpdateStatus::UpToDate { latest } => {
                ui.label(format!("You're up to date ({}).", version::VERSION));
                ui.label(
                    RichText::new(format!("The newest release is {latest}."))
                        .color(palette.text_weak),
                );
            }
            UpdateStatus::Available { version, page: url } => {
                page = Some(url);
                describe_install(ui, palette, version, updates);
            }
            UpdateStatus::Failed(error) => {
                ui.label("Couldn't check for updates.");
                ui.colored_label(palette.error, error);
            }
        }
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut action = None;
            if ui.button("Close").clicked() {
                action = Some(UpdateAction::Close);
            }
            if let Some(url) = page
                && ui.button("Open release page").clicked()
            {
                ui.ctx().open_url(OpenUrl::new_tab(url));
            }
            if let Some(button) = primary_button(updates)
                && form::primary_button(ui, button.label(), true).clicked()
            {
                action = Some(button.action());
            }
            action
        })
        .inner
    });
    if modal.should_close() {
        Some(UpdateAction::Close)
    } else {
        modal.inner
    }
}

/// What the dialog says about an available update, depending on how installing it is going.
fn describe_install(
    ui: &mut egui::Ui,
    palette: &Palette,
    available: &semver::Version,
    updates: &Updates,
) {
    match updates.install_state() {
        InstallState::Idle => {
            ui.label(format!(
                "DbRover {available} is available (you have {}).",
                version::VERSION
            ));
            if let InstallSupport::Unsupported(reason) = updates.support() {
                ui.label(RichText::new(reason).color(palette.text_weak));
            }
        }
        state @ InstallState::Downloading { downloaded, .. } => {
            ui.label(format!("Downloading DbRover {available}…"));
            let bar = match state.fraction() {
                Some(fraction) => ProgressBar::new(fraction).show_percentage(),
                None => ProgressBar::new(0.0)
                    .animate(true)
                    .text(format!("{:.1} MB", *downloaded as f64 / 1_000_000.0)),
            };
            ui.add(bar);
        }
        InstallState::Installed(update) => {
            let (done, next) = match update.restart {
                Restart::Relaunch => ("is installed", "Restart DbRover to start using it."),
                Restart::RunInstaller(_) => (
                    "is downloaded",
                    "Restart DbRover to install it: its installer runs once DbRover has quit, \
                     then starts the new version.",
                ),
            };
            ui.label(format!("DbRover {} {done}.", update.version));
            ui.label(RichText::new(next).color(palette.text_weak));
        }
        InstallState::Failed(error) => {
            ui.label("Couldn't install the update.");
            ui.colored_label(palette.error, error);
        }
    }
}

/// The dialog's main button, when there is a next step to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimaryButton {
    Install,
    TryAgain,
    Restart,
}

impl PrimaryButton {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install and restart",
            Self::TryAgain => "Try again",
            Self::Restart => "Restart now",
        }
    }

    fn action(self) -> UpdateAction {
        match self {
            Self::Install | Self::TryAgain => UpdateAction::Install,
            Self::Restart => UpdateAction::Restart,
        }
    }
}

fn primary_button(updates: &Updates) -> Option<PrimaryButton> {
    let installable = matches!(updates.status(), UpdateStatus::Available { .. })
        && *updates.support() == InstallSupport::Supported;
    match updates.install_state() {
        InstallState::Idle if installable => Some(PrimaryButton::Install),
        InstallState::Failed(_) if installable => Some(PrimaryButton::TryAgain),
        InstallState::Installed(_) => Some(PrimaryButton::Restart),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dbrover_engine::{Event, InstalledUpdate, Release, StagedInstaller};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn with_update(support: InstallSupport) -> Updates {
        let mut updates = Updates::new(support);
        updates.apply(&Event::LatestRelease(Ok(Release {
            tag: "v9.0.0".to_owned(),
            page: "https://example.com/v9.0.0".to_owned(),
        })));
        updates
    }

    #[test]
    fn offers_to_install_where_it_can_and_the_release_page_everywhere() {
        let updates = with_update(InstallSupport::Supported);
        let mut action = None;
        let mut harness = Harness::new_ui(|ui| action = action.or(show(ui.ctx(), &updates)));
        harness.run();
        harness.get_by_label(&format!(
            "DbRover 9.0.0 is available (you have {}).",
            version::VERSION
        ));
        harness.get_by_label("Open release page");
        harness.get_by_label("Install and restart").click();
        harness.run();
        drop(harness);
        assert_eq!(action, Some(UpdateAction::Install));

        let updates = with_update(InstallSupport::Unsupported("Only the AppImage".to_owned()));
        let mut harness = Harness::new_ui(|ui| {
            show(ui.ctx(), &updates);
        });
        harness.run();
        harness.get_by_label("Only the AppImage");
        assert!(harness.query_by_label("Install and restart").is_none());
        harness.get_by_label("Open release page");
    }

    #[test]
    fn shows_progress_then_offers_a_restart() {
        let mut updates = with_update(InstallSupport::Supported);
        let _ = updates.install();
        updates.apply(&Event::UpdateProgress {
            downloaded: 50,
            total: Some(100),
        });
        let mut harness = Harness::new_ui(|ui| {
            show(ui.ctx(), &updates);
        });
        harness.run_steps(2);
        harness.get_by_label("Downloading DbRover 9.0.0…");
        assert!(harness.query_by_label("Install and restart").is_none());
        drop(harness);

        updates.apply(&Event::UpdateInstalled(Ok(InstalledUpdate {
            version: "9.0.0".to_owned(),
            restart: Restart::Relaunch,
        })));
        let mut action = None;
        let mut harness = Harness::new_ui(|ui| action = action.or(show(ui.ctx(), &updates)));
        harness.run();
        harness.get_by_label("DbRover 9.0.0 is installed.");
        harness.get_by_label("Restart now").click();
        harness.run();
        drop(harness);
        assert_eq!(action, Some(UpdateAction::Restart));
    }

    #[test]
    fn says_a_windows_update_installs_on_restart() {
        let mut updates = with_update(InstallSupport::Supported);
        let _ = updates.install();
        updates.apply(&Event::UpdateInstalled(Ok(InstalledUpdate {
            version: "9.0.0".to_owned(),
            restart: Restart::RunInstaller(StagedInstaller::from(PathBuf::from(
                "dbrover-setup.exe",
            ))),
        })));
        let mut harness = Harness::new_ui(|ui| {
            show(ui.ctx(), &updates);
        });
        harness.run();
        harness.get_by_label("DbRover 9.0.0 is downloaded.");
        harness.get_by_label_contains("its installer runs once DbRover has quit");
        harness.get_by_label("Restart now");
    }
}
