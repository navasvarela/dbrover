//! **Change…**: the tables under the node the search was opened on, with tick boxes and a
//! filter, to search some of them only.

use std::collections::HashSet;

use dbrover_core::{CatalogPath, SearchScope, SearchableTable, TablesToSearch};
use egui::{Id, ScrollArea, TextEdit};

use super::{SearchAction, SearchTab, SearchView};
use crate::dialog::{self, DialogSize};
use crate::{form, icons};

const LIST_HEIGHT: f32 = 320.0;

pub(super) struct Picker {
    /// The tables listed, once the engine answers.
    tables: Option<Result<Vec<SearchableTable>, String>>,
    ticked: HashSet<CatalogPath>,
    /// Whether `ticked` is still to be filled from the tab's scope when the tables come.
    from_scope: bool,
    filter: String,
}

impl Picker {
    pub(super) fn listed(&mut self, result: Result<Vec<SearchableTable>, String>) {
        self.tables = Some(result);
    }
}

/// Opens the picker on the node the tab was opened on, and asks for its tables.
pub(super) fn open(tab: &mut SearchTab) -> SearchAction {
    tab.picker = Some(Picker {
        tables: None,
        ticked: match &tab.scope {
            SearchScope::Tables(paths) => paths.iter().cloned().collect(),
            SearchScope::Under(_) => HashSet::new(),
        },
        from_scope: matches!(tab.scope, SearchScope::Under(_)),
        filter: String::new(),
    });
    SearchAction::ListTables {
        data_source: tab.data_source,
        tables: TablesToSearch {
            scope: SearchScope::Under(tab.base.clone()),
            views: tab.views,
        },
    }
}

pub(super) fn show(
    tab: &mut SearchTab,
    ctx: &egui::Context,
    view: &SearchView<'_>,
) -> Option<SearchAction> {
    let picker = tab.picker.as_mut()?;
    if picker.from_scope
        && let Some(Ok(tables)) = &picker.tables
    {
        picker.ticked = tables.iter().map(|table| table.path.clone()).collect();
        picker.from_scope = false;
    }
    let id = Id::new(("search_picker", tab.data_source));
    let modal = dialog::modal(ctx, id, DialogSize::Wide).show(ctx, |ui| {
        form::Header::new(
            icons::TABLE,
            &format!("Search which tables of {}?", view.name),
        )
        .show(ui);
        let tables = match &picker.tables {
            None => {
                ui.spinner();
                return None;
            }
            Some(Err(error)) => {
                ui.label(format!("Could not list the tables: {error}"));
                return form::footer(ui, |ui| ui.button("Close").clicked().then_some(None));
            }
            Some(Ok(tables)) => tables,
        };
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut picker.filter).hint_text("Filter by name"));
            if ui.button("All").clicked() {
                picker.ticked = tables.iter().map(|table| table.path.clone()).collect();
            }
            if ui.button("None").clicked() {
                picker.ticked.clear();
            }
        });
        let filter = picker.filter.to_lowercase();
        ScrollArea::vertical()
            .max_height(LIST_HEIGHT)
            .show(ui, |ui| {
                for table in tables {
                    let label = super::table_label(table);
                    if !label.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let mut on = picker.ticked.contains(&table.path);
                    if ui.checkbox(&mut on, &label).changed() {
                        if on {
                            picker.ticked.insert(table.path.clone());
                        } else {
                            picker.ticked.remove(&table.path);
                        }
                    }
                }
            });
        let chosen: Vec<CatalogPath> = tables
            .iter()
            .filter(|table| picker.ticked.contains(&table.path))
            .map(|table| table.path.clone())
            .collect();
        let every = chosen.len() == tables.len();
        let label = match chosen.len() {
            1 => "Search 1 table".to_owned(),
            count => format!("Search {count} tables"),
        };
        form::footer(ui, |ui| {
            if form::primary_button(ui, &label, !chosen.is_empty()).clicked() {
                Some(Some((every, chosen)))
            } else if ui.button("Cancel").clicked() {
                Some(None)
            } else {
                None
            }
        })
    });
    let answer = if modal.should_close() {
        Some(None)
    } else {
        modal.inner
    };
    if let Some(chosen) = answer {
        if let Some((every, chosen)) = chosen {
            tab.scope = if every {
                SearchScope::Under(tab.base.clone())
            } else {
                SearchScope::Tables(chosen)
            };
        }
        tab.picker = None;
    }
    None
}
