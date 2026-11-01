use egui::{Align, Id, Layout, RichText};

use crate::brand;
use crate::theme::Palette;
use crate::version;
use crate::widgets;

const LOGO_SIZE: f32 = 72.0;
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// Help › About DbRover: the version and commit to quote in a bug report, the licence, and where
/// the project lives. Returns `true` once the user closes it.
pub fn show(ctx: &egui::Context) -> bool {
    let modal = widgets::dialog(ctx, Id::new("about_dbrover")).show(ctx, |ui| {
        let palette = &Palette::for_visuals(ui.visuals());
        ui.vertical_centered(|ui| {
            ui.add(brand::logo().max_size(egui::vec2(LOGO_SIZE, LOGO_SIZE)));
            ui.heading("DbRover");
            ui.label(RichText::new(brand::MOTTO).color(palette.text_weak));
            ui.add_space(ui.spacing().item_spacing.y);
            ui.label(format!("Version {}", version::VERSION));
            ui.label(
                RichText::new(format!("Commit {}", version::COMMIT))
                    .monospace()
                    .small(),
            );
            ui.label(RichText::new(format!("Licensed under {LICENSE}")).small());
            ui.hyperlink_to("Source and issues on GitHub", REPOSITORY);
        });
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let copy = ui.button("Copy version").on_hover_text("For a bug report");
            if copy.clicked() {
                ui.ctx().copy_text(version::describe());
            }
            ui.button("Close").clicked()
        })
        .inner
    });
    modal.should_close() || modal.inner
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn shows_the_version_commit_and_licence_and_closes() {
        let mut closed = false;
        let mut harness = Harness::new_ui(|ui| closed |= show(ui.ctx()));
        harness.run();

        harness.get_by_label(&format!("Version {}", version::VERSION));
        harness.get_by_label(&format!("Commit {}", version::COMMIT));
        harness.get_by_label("Licensed under MIT OR Apache-2.0");
        harness.get_by_label("Close").click();
        harness.run();
        drop(harness);

        assert!(closed);
    }
}
