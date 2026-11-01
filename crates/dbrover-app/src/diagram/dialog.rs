//! **Diagram…** on a schema: which of its tables the diagram shows, and what it is called.
//! The explorer selects one node at a time, so tables are chosen here rather than in the tree.

use std::collections::BTreeSet;

use dbrover_core::{CatalogNode, CatalogPath, DataSourceId, FolderKind, NodeKind};
use egui::{Align, Id, Layout, ScrollArea, TextEdit, Ui};

use super::Growth;
use crate::catalog_tree::{CatalogTree, Children, LevelEnd};
use crate::form;
use crate::theme::Palette;
use crate::widgets;

/// Past this many tables the dialog asks to narrow the choice before opening: a diagram that
/// big is hard to read at any zoom that shows it whole, and slow to load from a distant
/// server. Measured and decided in `docs/diagrams.md`.
pub const LARGE_DIAGRAM: usize = 150;
const LIST_HEIGHT: f32 = 280.0;
const FIELD_WIDTH: f32 = 280.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramDialogAction {
    Open {
        tables: Vec<String>,
        growth: Option<Growth>,
        name: String,
    },
    Close,
}

pub struct DiagramDialog {
    pub data_source: DataSourceId,
    schema: String,
    tables_path: CatalogPath,
    /// Tables are ticked unless unticked here, so ones still loading start ticked.
    unticked: BTreeSet<String>,
    filter: String,
    add_referenced: bool,
    name: String,
}

impl DiagramDialog {
    /// The dialog for the schema at `schema_path`, or `None` if that isn't a schema.
    pub fn new(data_source: DataSourceId, schema_path: &CatalogPath) -> Option<Self> {
        let schema = schema_path
            .last()
            .filter(|segment| segment.kind == NodeKind::Schema)?;
        let folder = FolderKind::Tables;
        Some(Self {
            data_source,
            schema: schema.name.clone(),
            tables_path: schema_path.child(NodeKind::Folder(folder), folder.label()),
            unticked: BTreeSet::new(),
            filter: String::new(),
            add_referenced: false,
            name: schema.name.clone(),
        })
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// The schema's tables folder, which the dialog lists once the explorer has loaded it.
    pub fn tables_path(&self) -> &CatalogPath {
        &self.tables_path
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        catalog: Option<&CatalogTree>,
    ) -> Option<DiagramDialogAction> {
        let modal = widgets::dialog(ctx, Id::new("diagram_dialog")).show(ctx, |ui| {
            ui.set_width(FIELD_WIDTH + 80.0);
            self.contents(ui, catalog)
        });
        if modal.should_close() {
            return Some(DiagramDialogAction::Close);
        }
        modal.inner
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        catalog: Option<&CatalogTree>,
    ) -> Option<DiagramDialogAction> {
        let palette = Palette::for_visuals(ui.visuals());
        ui.heading(format!("Diagram of {}", self.schema));
        ui.horizontal(|ui| {
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
        });
        ui.add_space(4.0);
        let tables = self.list(ui, catalog, &palette);
        ui.checkbox(&mut self.add_referenced, "Add the tables these reference");
        let chosen: Vec<String> = tables
            .into_iter()
            .filter(|table| !self.unticked.contains(table))
            .collect();
        let large = chosen.len() > LARGE_DIAGRAM;
        if large {
            ui.colored_label(
                palette.warning,
                format!(
                    "{} tables make a diagram that is hard to read and slow to load. \
                     Untick some, or open it anyway.",
                    chosen.len()
                ),
            );
        }
        ui.separator();
        let name = self.name.trim().to_owned();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let open = if large { "Open anyway" } else { "Open" };
            let can_open = !chosen.is_empty() && !name.is_empty();
            if form::primary_button(ui, open, can_open).clicked() {
                return Some(DiagramDialogAction::Open {
                    tables: chosen,
                    growth: self.add_referenced.then_some(Growth::Referenced),
                    name,
                });
            }
            ui.button("Cancel")
                .clicked()
                .then_some(DiagramDialogAction::Close)
        })
        .inner
    }

    /// The schema's tables, a box for each, under a filter and **All** and **None**; returns
    /// every table loaded, in the explorer's order. The list keeps one height while it loads
    /// and filters, so the dialog doesn't move under the pointer.
    fn list(
        &mut self,
        ui: &mut Ui,
        catalog: Option<&CatalogTree>,
        palette: &Palette,
    ) -> Vec<String> {
        let tables: Vec<String> = match catalog.and_then(|c| c.children(&self.tables_path)) {
            Some(Children::Loaded(nodes)) => nodes.iter().filter_map(table_name).collect(),
            _ => Vec::new(),
        };
        self.list_controls(ui, &tables);
        ScrollArea::vertical()
            .min_scrolled_height(LIST_HEIGHT)
            .max_height(LIST_HEIGHT)
            .auto_shrink(false)
            .show(ui, |ui| {
                match catalog.and_then(|c| c.children(&self.tables_path)) {
                    Some(Children::Loaded(_)) => self.checkboxes(ui, &tables),
                    Some(Children::Failed(error)) => {
                        ui.colored_label(
                            palette.error,
                            format!("Could not list the tables: {error}"),
                        );
                    }
                    Some(Children::Loading) | None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak("Loading the tables…");
                        });
                    }
                }
            });
        if let Some(LevelEnd::More(_)) = catalog.map(|catalog| catalog.level_end(&self.tables_path))
        {
            ui.weak(format!(
                "Only the first {} tables are listed.",
                tables.len()
            ));
        }
        tables
    }

    fn list_controls(&mut self, ui: &mut Ui, tables: &[String]) {
        ui.horizontal(|ui| {
            let label = ui.label("Tables");
            ui.add(
                TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter")
                    .desired_width(160.0),
            )
            .labelled_by(label.id);
            let shown = self.matching(tables);
            if ui.button("All").clicked() {
                for table in &shown {
                    self.unticked.remove(*table);
                }
            }
            if ui.button("None").clicked() {
                self.unticked
                    .extend(shown.iter().map(|table| (*table).clone()));
            }
        });
    }

    fn checkboxes(&mut self, ui: &mut Ui, tables: &[String]) {
        for table in self.matching(tables) {
            let mut ticked = !self.unticked.contains(table);
            if ui.checkbox(&mut ticked, table).changed() {
                if ticked {
                    self.unticked.remove(table);
                } else {
                    self.unticked.insert(table.clone());
                }
            }
        }
    }

    fn matching<'a>(&self, tables: &'a [String]) -> Vec<&'a String> {
        let filter = self.filter.trim().to_lowercase();
        tables
            .iter()
            .filter(|table| table.to_lowercase().contains(&filter))
            .collect()
    }
}

/// The table's name, if the node is a table the explorer shows by default.
fn table_name(node: &CatalogNode) -> Option<String> {
    let table = node
        .path
        .last()
        .filter(|segment| segment.kind == NodeKind::Table)?;
    (!node.system).then(|| table.name.clone())
}
