//! What a search found, by table, in the order the tables finished: each table that matched
//! with its rows, the ones that timed out or failed, then the rest folded into one line.

use dbrover_core::{Cell, Row, TableName};
use dbrover_engine::{PlannedTable, TableOutcome};
use egui::{CollapsingHeader, FontId, Id, RichText, ScrollArea, Sense, TextStyle, Ui};

use super::highlight;
use super::{Phase, Question, Search, SearchAction, SearchTab, SearchView};
use crate::console::RowsToOpen;
use crate::theme::Palette;
use crate::{icons, widgets};

/// How many tables with matches show their rows before any is clicked.
pub(super) const EXPANDED_AT_FIRST: usize = 5;

pub(super) fn show(tab: &mut SearchTab, ui: &mut Ui, view: &SearchView<'_>) -> Vec<SearchAction> {
    let data_source = tab.data_source;
    let mut actions = Vec::new();
    let mut retry = None;
    let search = match &mut tab.phase {
        Phase::Running(search) | Phase::Ended(search) => search,
        Phase::Asking(_) | Phase::Planning { .. } | Phase::Idle | Phase::Failed(_) => {
            return actions;
        }
    };
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let order: Vec<usize> = search.searched.iter().map(|table| table.index).collect();
            for index in order {
                let Some(searched) = search.outcome(index).cloned() else {
                    continue;
                };
                let Some(planned) = search.plan.tables.get(index).cloned() else {
                    continue;
                };
                let retries = search.end.is_some() && search.retry.is_none();
                match &searched.outcome {
                    TableOutcome::Matched { rows, more } => {
                        let found = Found {
                            planned: &planned,
                            index,
                            rows,
                            more: *more,
                        };
                        matched(ui, search, &found, data_source, &mut actions);
                    }
                    TableOutcome::TimedOut(limit) => {
                        if stopped(ui, &planned, &format!("timed out after {limit}"), retries) {
                            retry = Some(index);
                        }
                    }
                    TableOutcome::Failed(message) => {
                        stopped(ui, &planned, &format!("failed: {message}"), false);
                    }
                    TableOutcome::NoMatch => {}
                }
            }
            the_rest(ui, search, view);
        });
    if let Some(index) = retry {
        tab.question = Some(Question::Retry(index));
    }
    actions
}

/// A table that matched, and what it found.
struct Found<'a> {
    planned: &'a PlannedTable,
    /// Its place in the plan.
    index: usize,
    rows: &'a [Row],
    more: bool,
}

/// A table that matched: a header with its count and columns, and its rows beneath.
fn matched(
    ui: &mut Ui,
    search: &mut Search,
    found: &Found<'_>,
    data_source: dbrover_core::DataSourceId,
    actions: &mut Vec<SearchAction>,
) {
    let Found {
        planned,
        index,
        rows,
        more,
    } = *found;
    let Some(statement) = &planned.statement else {
        return;
    };
    let count = if more {
        format!("{}+ rows", rows.len())
    } else if rows.len() == 1 {
        "1 row".to_owned()
    } else {
        format!("{} rows", rows.len())
    };
    let columns = matched_columns(rows, &statement.selected, search);
    let open = search.expanded.contains(&index);
    let header = CollapsingHeader::new(RichText::new(super::table_label(&planned.table)).strong())
        .id_salt(("search_table", index))
        .open(Some(open))
        .show(ui, |ui| {
            for row in rows {
                row_line(
                    ui,
                    row,
                    planned,
                    &statement.selected,
                    search,
                    data_source,
                    actions,
                );
            }
        });
    if header.header_response.clicked() {
        if open {
            search.expanded.remove(&index);
        } else {
            search.expanded.insert(index);
        }
    }
    header.header_response.context_menu(|ui| {
        statement_menu(ui, &statement.sql, data_source, actions);
    });
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        let mut text = count;
        if !columns.is_empty() {
            text.push_str(&format!(" in {}", columns.join(", ")));
        }
        ui.weak(text);
        if ui.small_button("Open all").clicked() {
            actions.push(SearchAction::RunInConsole(
                data_source,
                format!("{};\n", statement.every_row),
            ));
        }
    });
}

/// The columns whose values hold the searched text in some row, in the select list's order.
fn matched_columns(rows: &[Row], selected: &[String], search: &Search) -> Vec<String> {
    selected
        .iter()
        .enumerate()
        .filter(|(column, _)| {
            rows.iter().any(|row| {
                row.get(*column)
                    .and_then(Option::as_deref)
                    .and_then(|value| highlight::find(value, &search.request.search))
                    .is_some()
            })
        })
        .map(|(_, name)| name.clone())
        .collect()
}

/// One found row: its key's values, then the values that hold the text, highlighted. A click
/// selects it; a double click, or Open row, opens it.
fn row_line(
    ui: &mut Ui,
    row: &Row,
    planned: &PlannedTable,
    selected: &[String],
    search: &Search,
    data_source: dbrover_core::DataSourceId,
    actions: &mut Vec<SearchAction>,
) {
    let palette = Palette::for_visuals(ui.visuals());
    let font = TextStyle::Monospace.resolve(ui.style());
    let key = &planned.table.key;
    let response = ui
        .horizontal(|ui| {
            for (column, name) in selected.iter().enumerate() {
                let value = row.get(column).cloned().flatten();
                let found = value
                    .as_deref()
                    .and_then(|value| highlight::find(value, &search.request.search));
                if !key.contains(name) && found.is_none() {
                    continue;
                }
                ui.weak(name);
                let text = value.as_deref().unwrap_or("NULL");
                ui.label(highlight::layout(
                    text,
                    found,
                    FontId::new(font.size, font.family.clone()),
                    palette.text,
                    palette.selection,
                ));
            }
            if ui.small_button("Open row").clicked() {
                actions.extend(open_row(row, planned, selected, data_source));
            }
        })
        .response
        .interact(Sense::click());
    if response.double_clicked() {
        actions.extend(open_row(row, planned, selected, data_source));
    }
}

/// The console that shows `row` whole: by its key, or, for a table without one, by the values
/// the search read that aren't `NULL`.
fn open_row(
    row: &Row,
    planned: &PlannedTable,
    selected: &[String],
    data_source: dbrover_core::DataSourceId,
) -> Option<SearchAction> {
    let key = &planned.table.key;
    let filter: Vec<(String, String)> = selected
        .iter()
        .zip(row.iter())
        .filter(|(name, _)| key.is_empty() || key.contains(name))
        .filter_map(|(name, value): (&String, &Cell)| {
            value.clone().map(|value| (name.clone(), value))
        })
        .collect();
    if filter.is_empty() {
        return None;
    }
    Some(SearchAction::OpenRows(
        data_source,
        RowsToOpen {
            table: TableName {
                schema: planned.table.name.schema.clone(),
                name: planned.table.name.name.clone(),
            },
            filter,
        },
    ))
}

fn statement_menu(
    ui: &mut Ui,
    sql: &str,
    data_source: dbrover_core::DataSourceId,
    actions: &mut Vec<SearchAction>,
) {
    if widgets::menu_item(ui, icons::COPY, "Copy statement").clicked() {
        actions.push(SearchAction::Copy(sql.to_owned()));
    }
    if widgets::menu_item(ui, icons::TERMINAL_WINDOW, "Open statement").clicked() {
        actions.push(SearchAction::OpenInConsole(
            data_source,
            format!("{sql};\n"),
        ));
    }
}

/// A table that timed out or failed, with Retry for one that timed out. Returns whether Retry
/// was clicked.
fn stopped(ui: &mut Ui, planned: &PlannedTable, why: &str, retries: bool) -> bool {
    let palette = Palette::for_visuals(ui.visuals());
    let color = if retries {
        palette.warning
    } else {
        palette.error
    };
    ui.horizontal(|ui| {
        let glyph = if retries {
            icons::CLOCK
        } else {
            icons::WARNING
        };
        widgets::status(
            ui,
            glyph,
            color,
            &format!("{} {why}", super::table_label(&planned.table)),
        );
        retries
            && ui
                .small_button("Retry")
                .on_hover_text("Search this table again, with no time limit")
                .clicked()
    })
    .inner
}

/// The tables that held nothing, had no column that can hold the value, or weren't read, in
/// one line, with their names on demand.
fn the_rest(ui: &mut Ui, search: &Search, view: &SearchView<'_>) {
    let names = |pick: &dyn Fn(usize, &PlannedTable) -> bool| -> Vec<String> {
        search
            .plan
            .tables
            .iter()
            .enumerate()
            .filter(|(index, table)| pick(*index, table))
            .map(|(_, table)| super::table_label(&table.table))
            .collect()
    };
    let no_match = names(&|index, _| {
        search
            .outcome(index)
            .is_some_and(|table| table.outcome == TableOutcome::NoMatch)
    });
    let no_columns = names(&|_, table| table.statement.is_none());
    let ended = search.end.is_some();
    let not_read = names(&|index, table| {
        ended && table.statement.is_some() && search.outcome(index).is_none()
    });
    if no_match.is_empty() && no_columns.is_empty() && not_read.is_empty() {
        if ended && search.searched.is_empty() && search.plan.searched() == 0 {
            ui.weak(format!(
                "No table in {} has a column that can hold this value.",
                view.name
            ));
        }
        return;
    }
    ui.add_space(ui.spacing().item_spacing.y);
    let mut parts = Vec::new();
    for (list, what) in [
        (&no_match, "with no match"),
        (&no_columns, "with no column that can hold the value"),
        (&not_read, "not searched"),
    ] {
        if !list.is_empty() {
            parts.push(format!("{} {}", plural(list.len()), what));
        }
    }
    CollapsingHeader::new(RichText::new(parts.join(" \u{b7} ")).weak())
        .id_salt(Id::new("search_the_rest"))
        .show(ui, |ui| {
            for (list, what) in [
                (&no_match, "No match"),
                (&no_columns, "No column that can hold the value"),
                (&not_read, "Not searched"),
            ] {
                if !list.is_empty() {
                    ui.label(RichText::new(what).strong());
                    ui.weak(list.join(", "));
                }
            }
        });
}

fn plural(count: usize) -> String {
    if count == 1 {
        "1 table".to_owned()
    } else {
        format!("{count} tables")
    }
}
