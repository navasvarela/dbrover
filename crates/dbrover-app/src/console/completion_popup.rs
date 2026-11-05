//! Draws the completion list under the cursor and takes the keys that drive it.

use std::time::Instant;

use egui::text_edit::TextEditOutput;
use egui::{Area, Frame, Id, Key, Label, Modifiers, Order, RichText, ScrollArea, Ui};

use dbrover_core::NodeKind;

use super::QueryConsole;
use super::completion::CandidateKind;
use crate::icons::{self, Icon};
use crate::theme::Palette;

const MAX_HEIGHT: f32 = 220.0;
const WIDTH: f32 = 320.0;

/// Takes the keys the open list uses (↑, ↓, Enter, Tab, Escape) before the editor sees them.
/// An empty list (still loading, or nothing found) lets them through, so Enter still breaks
/// the line.
pub fn handle_keys(ui: &mut Ui, console: &mut QueryConsole) {
    let Some(open) = &console.completion else {
        return;
    };
    let has_candidates = !open.candidates.is_empty();
    let pressed = |key| ui.input_mut(|input| input.consume_key(Modifiers::NONE, key));
    if pressed(Key::Escape) {
        console.close_completion();
    } else if !has_candidates {
        // Nothing to pick: the keys belong to the editor.
    } else if pressed(Key::ArrowDown) {
        console.move_completion(true);
    } else if pressed(Key::ArrowUp) {
        console.move_completion(false);
    } else if pressed(Key::Enter) || pressed(Key::Tab) {
        console.accept_completion(Instant::now());
    }
}

/// Opens or closes the list after the editor ran: typing a name or a `.` opens it, typing
/// anything else or clicking elsewhere closes it.
pub fn follow_editor(ui: &Ui, console: &mut QueryConsole, output: &TextEditOutput) {
    let pointer_on_list = console
        .completion
        .as_ref()
        .and_then(|open| open.rect)
        .zip(ui.ctx().pointer_latest_pos())
        .is_some_and(|(rect, pointer)| rect.contains(pointer));
    if output.response.clicked() || (!output.response.has_focus() && !pointer_on_list) {
        console.close_completion();
        return;
    }
    if output.response.changed() {
        let typed = console.editor.text
            [..console.editor.selection.end.min(console.editor.text.len())]
            .chars()
            .next_back();
        match typed {
            Some(c) if c.is_alphanumeric() || c == '_' || c == '.' => {
                console.open_completion(false);
            }
            _ => console.close_completion(),
        }
    }
}

/// Draws the open list just below the cursor.
pub fn show(ui: &Ui, console: &mut QueryConsole, output: &TextEditOutput) {
    let Some(cursor) = output.cursor_range else {
        return;
    };
    let Some(open) = &console.completion else {
        return;
    };
    let cursor_rect = output.galley.pos_from_cursor(cursor.primary);
    let anchor = output.galley_pos + cursor_rect.left_bottom().to_vec2();
    let mut chosen = None;
    let response = Area::new(Id::new(("completion", console.id.0)))
        .order(Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(WIDTH);
                if open.candidates.is_empty() {
                    ui.weak(if open.loading {
                        "Loading the catalog…"
                    } else {
                        "No suggestions"
                    });
                    return;
                }
                let palette = &Palette::for_visuals(ui.visuals());
                // An area sizes itself from the frame before, so without a minimum the list
                // would never grow past the height of its first, shorter content.
                ScrollArea::vertical()
                    .max_height(MAX_HEIGHT)
                    .min_scrolled_height(MAX_HEIGHT)
                    .show(ui, |ui| {
                        for (index, candidate) in open.candidates.iter().enumerate() {
                            let selected = index == open.selected;
                            let row = ui.horizontal(|ui| {
                                ui.label(kind_icon(candidate.kind).rich_text(palette));
                                let label = ui.selectable_label(
                                    selected,
                                    RichText::new(&candidate.label).monospace(),
                                );
                                if let Some(detail) = &candidate.detail {
                                    ui.add(Label::new(RichText::new(detail).weak()).truncate());
                                }
                                label
                            });
                            if selected {
                                row.inner.scroll_to_me(None);
                            }
                            if row.inner.clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
            });
        });
    if let Some(open) = &mut console.completion {
        open.rect = Some(response.response.rect);
        if let Some(index) = chosen {
            open.selected = index;
        }
    }
    if chosen.is_some() {
        console.accept_completion(Instant::now());
    }
}

/// The icon a candidate's kind is shown with: the explorer's icon for catalog objects.
fn kind_icon(kind: CandidateKind) -> Icon {
    match kind {
        CandidateKind::Column => icons::node(NodeKind::Column),
        CandidateKind::Table => icons::node(NodeKind::Table),
        CandidateKind::View => icons::node(NodeKind::View),
        CandidateKind::Schema => icons::node(NodeKind::Schema),
        CandidateKind::Function => icons::node(NodeKind::Function),
        CandidateKind::Derived => icons::derived_table(),
        CandidateKind::Alias => icons::alias(),
        CandidateKind::Keyword => icons::keyword(),
        CandidateKind::Snippet => icons::snippet(),
    }
}
