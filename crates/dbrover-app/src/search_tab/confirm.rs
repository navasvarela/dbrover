//! The questions a search asks: before a careful data source's tables are read, and before a
//! timed-out table is searched again with no limit.

use egui::Id;

use super::{Phase, Question, SearchAction, SearchTab, SearchView};
use crate::dialog::{self, DialogSize};
use crate::{form, icons};

pub(super) fn show(
    tab: &mut SearchTab,
    ctx: &egui::Context,
    view: &SearchView<'_>,
) -> Option<SearchAction> {
    if let Phase::Asking(search) = &tab.phase {
        let question = search.plan.question?;
        let heading =
            super::question_heading(question, view.name, search.request.search.value.text());
        let limit = search.request.limits.per_table;
        let answer = ask(
            ctx,
            Id::new(("search_careful", tab.data_source)),
            &heading,
            |ui| {
                ui.label(super::question_body(question, limit));
            },
        )?;
        if answer {
            return tab.confirmed();
        }
        tab.phase = Phase::Idle;
        return None;
    }
    let Some(Question::Retry(index)) = tab.question else {
        return None;
    };
    let Phase::Ended(search) = &tab.phase else {
        tab.question = None;
        return None;
    };
    let table = search
        .plan
        .tables
        .get(index)
        .map(|table| super::table_label(&table.table))?;
    let heading = format!("Search {table} with no time limit?");
    let answer = ask(
        ctx,
        Id::new(("search_retry", tab.data_source)),
        &heading,
        |ui| {
            ui.label("It reads until it finishes or you press Cancel. Nothing is changed.");
        },
    )?;
    tab.question = None;
    if answer { tab.retry(index) } else { None }
}

/// A question answered **Search** or **Not now**; `None` until one is chosen.
fn ask(
    ctx: &egui::Context,
    id: Id,
    heading: &str,
    body: impl FnOnce(&mut egui::Ui),
) -> Option<bool> {
    let modal = dialog::modal(ctx, id, DialogSize::Confirm).show(ctx, |ui| {
        form::Header::new(icons::MAGNIFYING_GLASS, heading).show(ui);
        body(ui);
        form::footer(ui, |ui| {
            if form::primary_button(ui, "Search", true).clicked() {
                Some(true)
            } else if ui.button("Not now").clicked() {
                Some(false)
            } else {
                None
            }
        })
    });
    if modal.should_close() {
        Some(false)
    } else {
        modal.inner
    }
}
