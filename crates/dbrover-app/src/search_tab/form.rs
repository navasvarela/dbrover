//! The top of the tab: the value, Search and Cancel, where it searches, how it matches, and
//! how far it has got.

use dbrover_core::{CaseMatch, MatchKind, Relations};
use dbrover_engine::{SearchEnd, TableOutcome};
use egui::{Key, TextEdit, Ui};

use super::{Phase, Search, SearchAction, SearchTab, SearchView, picker};
use crate::theme::Palette;
use crate::{form, icons, widgets};

const VALUE_WIDTH: f32 = 320.0;
/// The field's hint, and its name to a screen reader.
const VALUE_LABEL: &str = "A value to find";

pub(super) fn show(tab: &mut SearchTab, ui: &mut Ui, view: &SearchView<'_>) -> Vec<SearchAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        let field = ui
            .add(
                TextEdit::singleline(&mut tab.text)
                    .hint_text(VALUE_LABEL)
                    .desired_width(VALUE_WIDTH),
            )
            .on_hover_text("A name, a reference, an email, a number, a UUID or a date");
        let field = widgets::name(field, egui::WidgetType::TextEdit, VALUE_LABEL, true);
        if std::mem::take(&mut tab.focus_value) {
            field.request_focus();
        }
        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
        let can_search = !tab.text.trim().is_empty() && !tab.is_busy();
        let clicked = form::primary_button(ui, "Search", can_search).clicked();
        if (clicked || (entered && can_search))
            && let Some(action) = tab.search()
        {
            actions.push(action);
        }
        if let Some(run) = tab.cancellable()
            && ui.button("Cancel").clicked()
        {
            actions.push(SearchAction::Cancel {
                data_source: tab.data_source,
                run,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.weak("In:");
        ui.label(super::scope_label(&tab.scope, view.name));
        if ui
            .add_enabled(!tab.is_busy(), egui::Button::new("Change\u{2026}"))
            .clicked()
        {
            actions.push(picker::open(tab));
        }
        ui.separator();
        ui.weak("Match:");
        ui.radio_value(&mut tab.matching, MatchKind::Contains, "Contains")
            .on_hover_text("The value anywhere in a text column");
        ui.radio_value(&mut tab.matching, MatchKind::WholeValue, "Whole value")
            .on_hover_text("A text column holding exactly the value");
        let mut matches_case = tab.case == CaseMatch::Match;
        if ui.checkbox(&mut matches_case, "Case").changed() {
            tab.case = if matches_case {
                CaseMatch::Match
            } else {
                CaseMatch::Ignore
            };
        }
        let mut views = tab.views == Relations::TablesAndViews;
        if ui
            .checkbox(&mut views, "Views")
            .on_hover_text("Views read their tables again, often with joins")
            .changed()
        {
            tab.views = if views {
                Relations::TablesAndViews
            } else {
                Relations::TablesOnly
            };
        }
    });
    progress(tab, ui);
    actions
}

/// How far the search has got, or how it ended.
fn progress(tab: &SearchTab, ui: &mut Ui) {
    let palette = &Palette::for_visuals(ui.visuals());
    match &tab.phase {
        Phase::Idle => {
            ui.weak(
                "Numbers, UUIDs and dates are also matched as whole values in their own columns.",
            );
        }
        Phase::Planning { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Listing the tables\u{2026}");
            });
        }
        Phase::Asking(_) => {}
        Phase::Running(search) => {
            ui.horizontal(|ui| {
                ui.spinner();
                let mut text = format!(
                    "Searched {} of {} tables",
                    search.searched.len(),
                    search.plan.searched()
                );
                if let Some(table) = search
                    .current
                    .and_then(|index| search.plan.tables.get(index))
                {
                    text.push_str(&format!(
                        " \u{b7} reading {}",
                        super::table_label(&table.table)
                    ));
                }
                text.push_str(&format!(" \u{b7} {}", elapsed(search.started.elapsed())));
                ui.label(text);
            });
        }
        Phase::Ended(search) => {
            let (glyph, color) = match search.end.as_ref().map(|(end, _)| end) {
                Some(SearchEnd::Failed(_)) => (icons::WARNING, palette.error),
                Some(SearchEnd::Cancelled) => (icons::PROHIBIT, palette.warning),
                _ => (icons::CHECK_CIRCLE, palette.success),
            };
            widgets::status(ui, glyph, color, &ended(search));
        }
        Phase::Failed(error) => {
            widgets::status(
                ui,
                icons::WARNING,
                palette.error,
                &format!("Could not search: {error}"),
            );
        }
    }
}

/// *Searched 38 tables in 0:41 · 4 with matches · 2 timed out*.
fn ended(search: &Search) -> String {
    let took = search
        .end
        .as_ref()
        .map_or_else(String::new, |(_, took)| format!(" in {}", elapsed(*took)));
    let matched = search
        .searched
        .iter()
        .filter(|table| matches!(table.outcome, TableOutcome::Matched { .. }))
        .count();
    let timed_out = search
        .searched
        .iter()
        .filter(|table| matches!(table.outcome, TableOutcome::TimedOut(_)))
        .count();
    let mut text = match search.end.as_ref().map(|(end, _)| end) {
        Some(SearchEnd::Cancelled) => format!(
            "Cancelled after {} of {} tables{took}",
            search.searched.len(),
            search.plan.searched()
        ),
        Some(SearchEnd::Failed(error)) => format!(
            "Stopped after {} of {} tables: {error}",
            search.searched.len(),
            search.plan.searched()
        ),
        _ => format!("Searched {} tables{took}", search.searched.len()),
    };
    text.push_str(&format!(" \u{b7} {matched} with matches"));
    if timed_out > 0 {
        text.push_str(&format!(" \u{b7} {timed_out} timed out"));
    }
    text
}

/// `0:41`, `12:05`.
fn elapsed(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
