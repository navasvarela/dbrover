//! A result of documents (ADR 0006): flattened into the grid's columns, and shown whole as a
//! tree or as JSON, beside the grid.

use dbrover_core::{ColumnInfo, Row};
use dbrover_document::{
    ColumnsChange, Document, FieldCell, Flattened, Path, Syntax, Value, ValueType,
};
use egui::{Id, Label, Response, RichText, ScrollArea, Sense, TextEdit, Ui};

use crate::json_tree::JsonTree;
use crate::widgets;

/// The documents of one result, and how they are shown.
#[derive(Debug, Default)]
pub struct DocumentResult {
    flattened: Flattened,
    pub shown_as: ShownAs,
}

/// The switch above a result of documents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShownAs {
    #[default]
    Grid,
    Tree,
    Json,
}

/// A change to the grid's columns asked for from a column's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnFold {
    Expand(usize),
    Collapse(Path),
}

/// Whether the grid's columns moved, so its sort and selection no longer point where they did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ColumnsMoved {
    No,
    Yes,
}

impl DocumentResult {
    /// Reads a page of documents, one a row as the cursor gave them, into the grid's `columns`
    /// and `rows`. Rows already loaded get an empty cell for each column the page added.
    pub fn add_page(
        &mut self,
        page: &[Row],
        columns: &mut Vec<ColumnInfo>,
        rows: &mut Vec<Row>,
    ) -> ColumnsMoved {
        let loaded = rows.len();
        let change = self.flattened.add_page(page);
        *columns = column_infos(&self.flattened);
        if change == ColumnsChange::Rearranged {
            *rows = self.flattened.rows(0);
            return ColumnsMoved::Yes;
        }
        for row in rows.iter_mut() {
            row.resize(columns.len(), None);
        }
        rows.extend(self.flattened.rows(loaded));
        ColumnsMoved::No
    }

    /// The documents at `indices` among those loaded, each a row of one cell as a cursor
    /// gives them: what JSON and JSON Lines write whole.
    pub fn document_rows(&self, indices: &[usize]) -> Vec<Row> {
        let documents = self.flattened.documents();
        indices
            .iter()
            .filter_map(|&index| documents.get(index))
            .map(|document| match document {
                Document::Read(value) => vec![Some(value.to_json())],
                Document::Unreadable { text, .. } => vec![Some(text.clone())],
            })
            .collect()
    }

    /// The document at `row` among those loaded, when it could be read.
    pub fn document(&self, row: usize) -> Option<&Value> {
        match self.flattened.documents().get(row)? {
            Document::Read(value) => Some(value),
            Document::Unreadable { .. } => None,
        }
    }

    /// The field `column` shows, from the document's top.
    pub fn field_of(&self, column: usize) -> Option<&Path> {
        self.flattened.field_of(column)
    }

    /// What the document at `row` holds in `column`; `None` for a missing field.
    pub fn value(&self, row: usize, column: usize) -> Option<&Value> {
        match self.flattened.cell(row, column) {
            FieldCell::Value(value) => Some(value),
            FieldCell::Missing | FieldCell::Unreadable(_) => None,
        }
    }

    /// Expands or collapses a column, and lays out the grid's `columns` and `rows` again.
    pub fn fold(&mut self, fold: &ColumnFold, columns: &mut Vec<ColumnInfo>, rows: &mut Vec<Row>) {
        match fold {
            ColumnFold::Expand(column) => self.flattened.expand(*column),
            ColumnFold::Collapse(field) => self.flattened.collapse(field),
        }
        *columns = column_infos(&self.flattened);
        *rows = self.flattened.rows(0);
    }
}

fn column_infos(flattened: &Flattened) -> Vec<ColumnInfo> {
    flattened
        .column_names()
        .into_iter()
        .map(|name| ColumnInfo {
            name,
            type_name: None,
        })
        .collect()
}

/// **Grid**, **Tree** and **JSON**, and how many fields only the tree shows.
/// `active` is false while something else, the chart, is shown in place of the documents.
/// Returns whether one of the documents' views was picked.
pub fn view_switch(ui: &mut Ui, documents: &mut DocumentResult, active: bool) -> bool {
    let mut picked = false;
    for (shown_as, label) in [
        (ShownAs::Grid, "Grid"),
        (ShownAs::Tree, "Tree"),
        (ShownAs::Json, "JSON"),
    ] {
        let selected = active && documents.shown_as == shown_as;
        if ui.selectable_label(selected, label).clicked() {
            documents.shown_as = shown_as;
            picked = true;
        }
    }
    let hidden = documents.flattened.hidden_columns();
    if hidden > 0 {
        ui.weak(format!("{hidden} more fields only in the tree"))
            .on_hover_text(format!(
                "The grid shows the first {} fields; the tree shows every one",
                dbrover_document::MAX_COLUMNS
            ));
    }
    picked
}

/// One document whole, as a tree or as JSON: the one at `index` among those loaded.
pub fn document_view(ui: &mut Ui, documents: &DocumentResult, index: usize, id: Id) {
    let all = documents.flattened.documents();
    let Some(document) = all.get(index) else {
        ui.weak("No documents");
        return;
    };
    ui.weak(format!(
        "Document {} of {} loaded; select a row in the grid to show another",
        index + 1,
        all.len()
    ));
    let value = match document {
        Document::Read(value) => value,
        Document::Unreadable { text, reason } => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Not a document: {reason}"),
            );
            ui.monospace(text);
            return;
        }
    };
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        if documents.shown_as == ShownAs::Json {
            let text = value.to_pretty_json();
            ui.add(
                TextEdit::multiline(&mut text.as_str())
                    .code_editor()
                    .desired_width(f32::INFINITY),
            );
            return;
        }
        JsonTree {
            id: id.with(index),
            root: "document",
            value,
            syntax: Syntax::ExtendedJson,
        }
        .show(ui);
    });
}

/// How a document's cell is drawn in the grid, beside its text.
pub enum CellLook {
    /// The document has no such field: empty and greyed, unlike `null`.
    Missing,
    /// An object or array, by its size: `{3 fields}`, `[12 items]`.
    Container(String),
    /// A scalar, or a row that isn't a document, drawn from the row's text as any cell is.
    Text,
}

pub fn cell_look(documents: &DocumentResult, row: usize, column: usize) -> CellLook {
    match documents.flattened.cell(row, column) {
        FieldCell::Missing => CellLook::Missing,
        FieldCell::Unreadable(_) => CellLook::Text,
        FieldCell::Value(value) => {
            let shown = value.shown(Syntax::ExtendedJson);
            match shown.value_type {
                ValueType::Object | ValueType::Array => CellLook::Container(shown.text),
                _ => CellLook::Text,
            }
        }
    }
}

/// A missing field: an empty cell, greyed unless selected.
pub fn missing_label(ui: &mut Ui, selected: bool) -> Response {
    if !selected {
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, ui.visuals().faint_bg_color);
    }
    ui.add(
        Label::new(RichText::new("").weak())
            .selectable(false)
            .sense(Sense::click_and_drag()),
    )
    .on_hover_text("This document has no such field")
}

/// An object or array, by its size; viewing the cell shows it whole.
pub fn container_label(ui: &mut Ui, summary: &str) -> Response {
    ui.add(
        Label::new(RichText::new(summary).weak())
            .selectable(false)
            .sense(Sense::click_and_drag()),
    )
    .on_hover_text("Space or double-click shows it as a tree")
}

/// The header's menu for a column of documents: **Expand column** for one holding objects,
/// **Collapse column** for one expanded from a field.
pub fn header_menu(
    response: &Response,
    documents: &DocumentResult,
    column: usize,
) -> Option<ColumnFold> {
    let flattened = &documents.flattened;
    let collapse = flattened.expanded_from(column);
    let can_expand = flattened.can_expand(column);
    if collapse.is_none() && !can_expand {
        return None;
    }
    let mut fold = None;
    widgets::context_menu(response, |ui| {
        if can_expand && ui.button("Expand column").clicked() {
            fold = Some(ColumnFold::Expand(column));
        }
        if let Some(field) = &collapse
            && ui.button(format!("Collapse into {field}")).clicked()
        {
            fold = Some(ColumnFold::Collapse(field.clone()));
        }
    });
    fold
}

/// The syntax of a cell's value in a result: Extended JSON for documents, plain JSON for a
/// SQL engine's column.
pub fn cell_syntax(documents: Option<&DocumentResult>) -> Syntax {
    documents.map_or(Syntax::Json, |_| Syntax::ExtendedJson)
}
