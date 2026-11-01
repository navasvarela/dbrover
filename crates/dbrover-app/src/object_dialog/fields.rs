//! Fields the helpers' forms share.

use dbrover_core::{CatalogPath, FolderKind, NodeKind};
use dbrover_language::QueryLanguage;
use dbrover_sql::Dialect;
use egui::{Response, ScrollArea, TextEdit, Ui, WidgetType};

use crate::catalog_tree::CatalogTree;
use crate::console::highlight::{self, Marks};
use crate::widgets;

/// Lines of SQL an editor shows before it scrolls: a query of a few joins.
const EDITOR_ROWS: usize = 10;
/// The editor's height once it scrolls, about `EDITOR_ROWS` lines.
const EDITOR_MAX_HEIGHT: f32 = 200.0;

/// The columns of a key or index, in the order they were ticked, which is its order.
pub(super) fn column_picker(
    ui: &mut Ui,
    chosen: &mut Vec<String>,
    columns: &[String],
    label: &str,
) {
    let text = if chosen.is_empty() {
        "Choose…".to_owned()
    } else {
        chosen.join(", ")
    };
    let menu = widgets::menu_button(ui, text, |ui| {
        if columns.is_empty() {
            ui.weak("No columns yet");
        }
        for column in columns {
            let mut ticked = chosen.contains(column);
            if ui.checkbox(&mut ticked, column).changed() {
                if ticked {
                    chosen.push(column.clone());
                } else {
                    chosen.retain(|other| other != column);
                }
            }
        }
    });
    widgets::name(menu.response, WidgetType::Button, label, true);
}

pub(super) fn columns_folder(table: &CatalogPath) -> CatalogPath {
    table.child(
        NodeKind::Folder(FolderKind::Columns),
        FolderKind::Columns.label(),
    )
}

/// A table's column names, once its columns folder is loaded.
pub(super) fn loaded_columns(catalog: &CatalogTree, table: &CatalogPath) -> Option<Vec<String>> {
    let columns = catalog.loaded_children(&columns_folder(table))?;
    Some(
        columns
            .iter()
            .filter(|column| column.kind() == Some(NodeKind::Column))
            .map(|column| column.label.clone())
            .collect(),
    )
}

/// An editable field of SQL, highlighted as the console highlights it.
pub(super) fn sql_editor(ui: &mut Ui, text: &mut String, dialect: Dialect, id: &str) -> Response {
    let marks = Marks::default();
    let mut layouter = highlight::layouter(QueryLanguage::Sql(dialect), &marks);
    ScrollArea::vertical()
        .id_salt((id, "scroll"))
        .max_height(EDITOR_MAX_HEIGHT)
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(text)
                    .id_salt(id)
                    .code_editor()
                    .hint_text("SELECT …")
                    .desired_rows(EDITOR_ROWS)
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            )
        })
        .inner
}
