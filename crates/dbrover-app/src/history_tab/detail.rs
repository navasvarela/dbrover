//! The entry selected in the History window: the whole statement, where it came from, and for
//! a grid edit, the values it replaced and the statement that would put them back.

use dbrover_core::Cell;
use dbrover_store::{HistoryEntry, HistorySource, NamedCell, RowChange, RowChangeKind};
use egui::{Grid, Label, RichText, ScrollArea, Ui};

/// How tall the statement may get before it scrolls, so the change below it stays in view.
const STATEMENT_HEIGHT: f32 = 96.0;

/// What the user asked of the selected entry.
pub(super) enum DetailAction {
    /// Put this statement into the console: the entry's own, or the one that reverts it.
    Insert(String),
}

/// Draws the entry's details.
pub(super) fn show(ui: &mut Ui, entry: &HistoryEntry) -> Option<DetailAction> {
    ui.label(provenance(&entry.source));
    ScrollArea::vertical()
        .id_salt("history_statement")
        .max_height(STATEMENT_HEIGHT)
        .show(ui, |ui| {
            ui.add(Label::new(RichText::new(&entry.sql).monospace()).selectable(true));
        });
    let HistorySource::GridEdit(Some(change)) = &entry.source else {
        return None;
    };
    ui.separator();
    row_change(ui, change);
    let revert = change.revert.as_ref()?;
    ui.add_space(4.0);
    ui.label("To put it back:");
    ui.add(Label::new(RichText::new(revert).monospace()).selectable(true));
    ui.horizontal(|ui| {
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(revert.clone());
        }
        ui.button("Insert into console")
            .clicked()
            .then(|| DetailAction::Insert(revert.clone()))
    })
    .inner
}

/// Where the statement came from, in a sentence.
fn provenance(source: &HistorySource) -> String {
    match source {
        HistorySource::Console => "Run in a query console.".to_owned(),
        HistorySource::GridEdit(Some(change)) => {
            format!("Submitted from the grid, on {}.", change.table)
        }
        HistorySource::GridEdit(None) => "Submitted from the grid.".to_owned(),
        HistorySource::ObjectHelper => "Run by an object helper in the explorer.".to_owned(),
        HistorySource::Import { file } => format!("Imported from {}.", file.display()),
        HistorySource::Export => "Read by Export \u{203a} All rows.".to_owned(),
        HistorySource::Mcp {
            client: Some(client),
        } => format!("Run by the MCP client {client}, through dbrover mcp."),
        HistorySource::Mcp { client: None } => {
            "Run by an MCP client, through dbrover mcp.".to_owned()
        }
        HistorySource::Action { id } => format!("Run by the action {id} from actions.toml."),
        HistorySource::DbRover => {
            "Run by a DbRover from before the history recorded where.".to_owned()
        }
    }
}

/// The row's key, and its values before and after.
fn row_change(ui: &mut Ui, change: &RowChange) {
    match &change.kind {
        RowChangeKind::Updated { key, cells } => {
            ui.label(format!("Changed the row where {}:", describe_key(key)));
            Grid::new("history_change").striped(true).show(ui, |ui| {
                ui.strong("Column");
                ui.strong("Before");
                ui.strong("After");
                ui.end_row();
                for cell in cells {
                    ui.label(&cell.column);
                    value(ui, &cell.old);
                    value(ui, &cell.new);
                    ui.end_row();
                }
            });
        }
        RowChangeKind::Deleted { key, row } => {
            ui.label(format!("Deleted the row where {}:", describe_key(key)));
            values(ui, row);
        }
        RowChangeKind::Inserted { values: typed } => {
            ui.label("Inserted a row with:");
            values(ui, typed);
        }
    }
}

fn values(ui: &mut Ui, cells: &[NamedCell]) {
    Grid::new("history_change").striped(true).show(ui, |ui| {
        for cell in cells {
            ui.label(&cell.column);
            value(ui, &cell.value);
            ui.end_row();
        }
    });
}

fn value(ui: &mut Ui, cell: &Cell) {
    match cell {
        Some(text) => ui.add(Label::new(RichText::new(text).monospace()).truncate()),
        None => ui.weak("NULL"),
    };
}

/// `id = 1, part = 2`.
fn describe_key(key: &[NamedCell]) -> String {
    key.iter()
        .map(|cell| match &cell.value {
            Some(value) => format!("{} = {value}", cell.column),
            None => format!("{} is NULL", cell.column),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
