//! A pane over another, with a line between them dragged to share the height: a console's
//! editor over its results, a report's file over its run.
//!
//! Laid out by hand rather than with a resizable `egui::Panel`: a panel lays its contents out
//! at full size even when the space is too short for it, and the pane above then covers (and
//! takes the clicks of) the top of the one below.

use egui::{Id, Ui, vec2};

const SPLITTER_HEIGHT: f32 = 6.0;
/// Each pane keeps at least this share of the height, so neither can be dragged (or squeezed
/// by a small window) out of reach.
const MIN_SHARE: f32 = 0.2;

/// Where the bottom pane's height is kept, and what it starts at.
pub struct VerticalSplit {
    pub id: Id,
    pub bottom_height: f32,
}

impl VerticalSplit {
    /// The top pane's and the bottom pane's heights, from the bottom height last dragged to.
    pub fn heights(&self, ui: &Ui) -> (f32, f32) {
        let total = (ui.available_height() - SPLITTER_HEIGHT).max(0.0);
        let wanted = ui.data(|data| data.get_temp(self.id).unwrap_or(self.bottom_height));
        let bottom = wanted.clamp(total * MIN_SHARE, total * (1.0 - MIN_SHARE));
        (total - bottom, bottom)
    }

    /// The line between the panes, dragged to resize them.
    pub fn splitter(&self, ui: &mut Ui) {
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), SPLITTER_HEIGHT),
            egui::Sense::drag(),
        );
        let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.dragged() {
            let height = ui.data(|data| data.get_temp(self.id).unwrap_or(self.bottom_height));
            ui.data_mut(|data| data.insert_temp(self.id, height - response.drag_delta().y));
        }
        let stroke = if response.hovered() || response.dragged() {
            ui.visuals().widgets.hovered.fg_stroke
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        ui.painter().hline(rect.x_range(), rect.center().y, stroke);
    }
}
