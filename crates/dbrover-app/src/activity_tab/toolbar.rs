//! The tab's toolbar: when it last read, its interval, Pause and Refresh, and the filters.

use std::time::Duration;

use egui::{Color32, ComboBox, Response, TextEdit, Ui};

use super::ActivityTab;
use super::refresh::CHOICES;
use crate::{icons, theme::Palette, widgets};

const FILTER_WIDTH: f32 = 220.0;

/// Draws the toolbar. Refresh only asks for a read; the timer sends it on the next frame.
pub(super) fn show(tab: &mut ActivityTab, ui: &mut Ui) {
    ui.horizontal(|ui| {
        status(tab, ui);
        interval(tab, ui);
        let (glyph, label, hover) = if tab.refresh.is_paused() {
            (icons::PLAY, "Resume", "Read again on the timer")
        } else {
            (icons::PAUSE, "Pause", "Stop reading until resumed")
        };
        if icon_button(ui, glyph, label).on_hover_text(hover).clicked() {
            tab.refresh.toggle_pause();
        }
        if icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh")
            .on_hover_text("Read once now")
            .clicked()
        {
            tab.refresh.read_now();
        }
        ui.separator();
        ui.checkbox(&mut tab.show_idle, "Idle")
            .on_hover_text("Sessions doing nothing, with no transaction open");
        ui.checkbox(&mut tab.show_background, "Background")
            .on_hover_text("The server's own processes");
        ui.add(
            TextEdit::singleline(&mut tab.filter)
                .hint_text("Filter by user, application, statement")
                .desired_width(FILTER_WIDTH),
        );
    });
}

/// A dot, amber while a read is out, and when the list was read.
fn status(tab: &ActivityTab, ui: &mut Ui) {
    let palette = &Palette::for_visuals(ui.visuals());
    let (color, hover) = if tab.refresh.is_waiting() {
        (palette.warning, "Reading")
    } else if tab.error.is_some() {
        (palette.error, "The last read failed")
    } else {
        (palette.success, "Read")
    };
    ui.colored_label(color, "\u{25cf}").on_hover_text(hover);
    match tab.read_time() {
        Some(time) => ui.weak(format!("Read at {time}")),
        None => ui.weak("Reading\u{2026}"),
    };
}

fn interval(tab: &mut ActivityTab, ui: &mut Ui) {
    let every = tab.refresh.every();
    let label = |seconds: u64| format!("Every {seconds} s");
    let mut chosen = every;
    ComboBox::from_id_salt(("activity_interval", tab.data_source))
        .selected_text(label(every.as_secs()))
        .show_ui(ui, |ui| {
            for seconds in CHOICES {
                ui.selectable_value(&mut chosen, Duration::from_secs(seconds), label(seconds));
            }
        });
    if chosen != every {
        tab.refresh.set_every(chosen);
    }
}

/// A button with an icon before its text, named by the text.
pub(super) fn icon_button(ui: &mut Ui, glyph: &str, label: &str) -> Response {
    let job = widgets::icon_text(ui, glyph, Color32::PLACEHOLDER, label);
    let response = ui.button(job);
    widgets::name(response, egui::WidgetType::Button, label, true)
}
