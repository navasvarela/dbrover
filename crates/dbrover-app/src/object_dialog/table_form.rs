//! The table form: New table's name and schema, a grid of columns, and tabs for unique keys,
//! foreign keys and checks; and Modify table, the same form filled from the live table, which
//! writes the statements that change the table into what the form says.

use std::collections::BTreeMap;

use dbrover_core::{CatalogPath, FolderKind, Identity, NodeKind};
use dbrover_sql::{
    Capabilities, Check, ColumnSpec, DdlError, DdlOperation, Dialect, ExistingTable, ForeignKey,
    ObjectName, ReferentialAction, TableAlteration, TableSpec, UniqueKey,
};
use egui::{Grid, Response, TextEdit, Ui, WidgetInfo, WidgetType};

use super::fields::{column_picker, columns_folder, loaded_columns};
use super::{Destruction, FormContext, ObjectForm};
use crate::catalog_tree::CatalogTree;
use crate::{icons, widgets};

const NAME_WIDTH: f32 = 140.0;
const TYPE_WIDTH: f32 = 130.0;
const DEFAULT_WIDTH: f32 = 110.0;
const EXPRESSION_WIDTH: f32 = 260.0;

pub(crate) struct TableForm {
    purpose: Purpose,
    /// The schema's tables folder, where the table is listed.
    folder: CatalogPath,
    schema: String,
    name: String,
    columns: Vec<ColumnRow>,
    unique_keys: Vec<UniqueRow>,
    foreign_keys: Vec<ForeignRow>,
    checks: Vec<CheckRow>,
    tab: Tab,
}

/// Whether the form creates a table or changes one.
enum Purpose {
    New,
    Modify {
        /// The table's path, which it is loaded from.
        table: CatalogPath,
        loaded: Loaded,
    },
}

enum Loaded {
    Loading,
    Failed(String),
    /// The table as it is, which the form's rows are compared with.
    Ready(Box<ExistingTable>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Columns,
    UniqueKeys,
    ForeignKeys,
    Checks,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Columns => "Columns",
            Self::UniqueKeys => "Unique keys",
            Self::ForeignKeys => "Foreign keys",
            Self::Checks => "Checks",
        }
    }

    /// The tabs the dialect has a use for.
    fn offered(capabilities: &Capabilities) -> Vec<Self> {
        [
            (Self::Columns, true),
            (Self::UniqueKeys, capabilities.unique_keys),
            (Self::ForeignKeys, capabilities.foreign_keys),
            (Self::Checks, true),
        ]
        .into_iter()
        .filter_map(|(tab, offered)| offered.then_some(tab))
        .collect()
    }
}

struct ColumnRow {
    name: String,
    data_type: String,
    nullable: bool,
    default: String,
    primary_key: bool,
    /// The column's name in the table being modified, whatever the row is renamed to; `None`
    /// for a column the form adds. Only this makes a new name a rename.
    origin: Option<String>,
    /// Kept as the table has it: the form shows it but doesn't change it.
    identity: Option<Identity>,
    /// How to convert the values when the type changes (`USING`), where the dialect can.
    conversion: String,
}

impl ColumnRow {
    fn added() -> Self {
        Self {
            name: String::new(),
            data_type: String::new(),
            nullable: true,
            default: String::new(),
            primary_key: false,
            origin: None,
            identity: None,
            conversion: String::new(),
        }
    }

    fn loaded(column: &ColumnSpec, primary_key: &[String]) -> Self {
        Self {
            name: column.name.clone(),
            data_type: column.data_type.clone(),
            nullable: column.nullable,
            default: column.default.clone().unwrap_or_default(),
            primary_key: primary_key.contains(&column.name),
            origin: Some(column.name.clone()),
            identity: column.identity,
            conversion: String::new(),
        }
    }
}

#[derive(Default)]
struct UniqueRow {
    name: String,
    columns: Vec<String>,
}

#[derive(Default)]
struct ForeignRow {
    name: String,
    columns: Vec<String>,
    target: Option<Target>,
    target_columns: Vec<String>,
    on_delete: ReferentialAction,
    on_update: ReferentialAction,
}

/// The table a foreign key points at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// The table being created, for a key between its own rows.
    ItSelf,
    /// A table in the catalog, at its path.
    Table { path: CatalogPath, name: ObjectName },
}

#[derive(Default)]
struct CheckRow {
    name: String,
    expression: String,
}

impl TableForm {
    /// New table in the schema at `schema`, or in the schema whose tables folder that is.
    /// `None` for any other node. Starts with one column, `id`, of the dialect's first common
    /// type, as the primary key: most tables begin that way.
    pub(crate) fn new_table(path: CatalogPath, first_type: &str) -> Option<Self> {
        let folder = match path.last()?.kind {
            NodeKind::Schema => path.child(
                NodeKind::Folder(FolderKind::Tables),
                FolderKind::Tables.label(),
            ),
            NodeKind::Folder(FolderKind::Tables) => path,
            _ => return None,
        };
        Some(Self {
            purpose: Purpose::New,
            schema: folder.name_of(NodeKind::Schema)?.to_owned(),
            folder,
            name: String::new(),
            columns: vec![ColumnRow {
                name: "id".to_owned(),
                data_type: first_type.to_owned(),
                nullable: false,
                primary_key: true,
                ..ColumnRow::added()
            }],
            unique_keys: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            tab: Tab::Columns,
        })
    }

    /// Modify table on the table at `table`, empty until the table is loaded. `None` unless
    /// `table` is a table's path.
    pub(crate) fn modify(table: CatalogPath) -> Option<Self> {
        let segment = table.last()?;
        if segment.kind != NodeKind::Table {
            return None;
        }
        Some(Self {
            schema: table.name_of(NodeKind::Schema)?.to_owned(),
            name: segment.name.clone(),
            folder: table.parent()?,
            purpose: Purpose::Modify {
                table,
                loaded: Loaded::Loading,
            },
            columns: Vec::new(),
            unique_keys: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            tab: Tab::Columns,
        })
    }

    /// Fills the form from the table as it is.
    fn fill(&mut self, table: &ExistingTable) {
        let spec = &table.spec;
        self.name.clone_from(&spec.name.name);
        self.columns = spec
            .columns
            .iter()
            .map(|column| ColumnRow::loaded(column, &spec.primary_key))
            .collect();
        self.unique_keys = spec
            .unique_keys
            .iter()
            .map(|key| UniqueRow {
                name: key.name.clone().unwrap_or_default(),
                columns: key.columns.clone(),
            })
            .collect();
        self.foreign_keys = spec
            .foreign_keys
            .iter()
            .map(|key| ForeignRow {
                name: key.name.clone().unwrap_or_default(),
                columns: key.columns.clone(),
                target: Some(self.target_of(&spec.name, &key.target)),
                target_columns: key.target_columns.clone(),
                on_delete: key.on_delete,
                on_update: key.on_update,
            })
            .collect();
        self.checks = spec
            .checks
            .iter()
            .map(|check| CheckRow {
                name: check.name.clone().unwrap_or_default(),
                expression: check.expression.clone(),
            })
            .collect();
    }

    /// A loaded foreign key's target, as the target picker names it: the table itself, or a
    /// table at its place in the tree.
    fn target_of(&self, table: &ObjectName, target: &ObjectName) -> Target {
        let schema = target.schema.as_deref().unwrap_or(&self.schema);
        if target.name == table.name && table.schema.as_deref().is_none_or(|own| own == schema) {
            return Target::ItSelf;
        }
        let schemas = self
            .folder
            .parent()
            .and_then(|schema| schema.parent())
            .unwrap_or_default();
        let path = schemas
            .child(NodeKind::Schema, schema)
            .child(
                NodeKind::Folder(FolderKind::Tables),
                FolderKind::Tables.label(),
            )
            .child(NodeKind::Table, target.name.clone());
        Target::Table {
            path,
            name: ObjectName::new(Some(schema), &target.name),
        }
    }

    fn loaded(&self) -> Option<&ExistingTable> {
        match &self.purpose {
            Purpose::Modify {
                loaded: Loaded::Ready(table),
                ..
            } => Some(table),
            _ => None,
        }
    }

    /// The change from the loaded table to what the form says: renames come from the rows'
    /// origins, never from names alone.
    fn alteration(&self, table: &ExistingTable) -> TableAlteration {
        let kept = || {
            self.columns
                .iter()
                .filter_map(|row| Some((row.name.trim().to_owned(), row, row.origin.as_ref()?)))
        };
        TableAlteration {
            table: table.clone(),
            after: self.spec(),
            renamed: kept()
                .filter(|(name, _, origin)| name != *origin)
                .map(|(name, _, origin)| (name, origin.clone()))
                .collect::<BTreeMap<_, _>>(),
            conversions: kept()
                .filter(|(_, row, _)| !row.conversion.trim().is_empty())
                .map(|(name, row, _)| (name, row.conversion.trim().to_owned()))
                .collect(),
        }
    }

    /// The loaded table's columns that no row keeps.
    fn dropped_columns(&self) -> Vec<&str> {
        let Some(table) = self.loaded() else {
            return Vec::new();
        };
        table
            .spec
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .filter(|name| {
                !self
                    .columns
                    .iter()
                    .any(|row| row.origin.as_deref() == Some(*name))
            })
            .collect()
    }

    fn column_names_by_row(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.name.trim().to_owned())
            .collect()
    }

    /// Follows a column's new name in the keys that list it, so renaming a column keeps its
    /// keys. Checks are conditions as typed, which the user changes themselves.
    fn rename_in_keys(&mut self, old: &str, new: &str) {
        let rename = |columns: &mut Vec<String>| {
            for column in columns.iter_mut().filter(|column| column.as_str() == old) {
                *column = new.to_owned();
            }
        };
        for key in &mut self.unique_keys {
            rename(&mut key.columns);
        }
        for key in &mut self.foreign_keys {
            rename(&mut key.columns);
            if key.target == Some(Target::ItSelf) {
                rename(&mut key.target_columns);
            }
        }
    }

    fn column_names(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect()
    }

    fn spec(&self) -> TableSpec {
        TableSpec {
            name: ObjectName::new(Some(&self.schema), self.name.trim()),
            columns: self
                .columns
                .iter()
                .map(|column| ColumnSpec {
                    name: column.name.trim().to_owned(),
                    data_type: column.data_type.trim().to_owned(),
                    nullable: column.nullable && !column.primary_key,
                    default: optional(&column.default),
                    identity: column.identity,
                })
                .collect(),
            primary_key: self
                .columns
                .iter()
                .filter(|column| column.primary_key)
                .map(|column| column.name.trim().to_owned())
                .collect(),
            unique_keys: self
                .unique_keys
                .iter()
                .map(|key| UniqueKey {
                    name: optional(&key.name),
                    columns: key.columns.clone(),
                })
                .collect(),
            foreign_keys: self
                .foreign_keys
                .iter()
                .map(|key| ForeignKey {
                    name: optional(&key.name),
                    columns: key.columns.clone(),
                    target: match &key.target {
                        Some(Target::ItSelf) => {
                            ObjectName::new(Some(&self.schema), self.name.trim())
                        }
                        Some(Target::Table { name, .. }) => name.clone(),
                        None => ObjectName::new(None, ""),
                    },
                    target_columns: key.target_columns.clone(),
                    on_delete: key.on_delete,
                    on_update: key.on_update,
                })
                .collect(),
            checks: self
                .checks
                .iter()
                .map(|check| Check {
                    name: optional(&check.name),
                    expression: check.expression.clone(),
                })
                .collect(),
        }
    }

    fn columns_tab(&mut self, ui: &mut Ui, capabilities: &Capabilities) {
        let names_before = self.column_names_by_row();
        let mut removed = None;
        Grid::new("table_columns")
            .num_columns(6)
            .striped(true)
            .show(ui, |ui| {
                for heading in ["Name", "Type", "Nullable", "Default", "Primary key", ""] {
                    ui.strong(heading);
                }
                ui.end_row();
                for (index, column) in self.columns.iter_mut().enumerate() {
                    let row = index + 1;
                    text_cell(
                        ui,
                        &mut column.name,
                        NAME_WIDTH,
                        &format!("Column {row} name"),
                    );
                    type_cell(ui, column, capabilities.common_types, row);
                    let nullable = ui.add_enabled(
                        !column.primary_key,
                        egui::Checkbox::without_text(&mut column.nullable),
                    );
                    widgets::name(
                        nullable,
                        WidgetType::Checkbox,
                        &format!("Column {row} nullable"),
                        !column.primary_key,
                    );
                    text_cell(
                        ui,
                        &mut column.default,
                        DEFAULT_WIDTH,
                        &format!("Column {row} default"),
                    );
                    let key = ui.checkbox(&mut column.primary_key, "");
                    widgets::name(
                        key,
                        WidgetType::Checkbox,
                        &format!("Column {row} primary key"),
                        true,
                    );
                    if remove_button(ui, &format!("Remove column {row}")).clicked() {
                        removed = Some(index);
                    }
                    ui.end_row();
                }
            });
        let names_after = self.column_names_by_row();
        for (before, after) in names_before.iter().zip(&names_after) {
            // A name typed over passes through an empty field, which names no column.
            if !before.is_empty() && !after.is_empty() && before != after {
                self.rename_in_keys(before, after);
            }
        }
        if let Some(index) = removed {
            self.columns.remove(index);
        }
        if ui.button("Add column").clicked() {
            self.columns.push(ColumnRow::added());
        }
        if capabilities.converts_with_using {
            self.conversions(ui);
        }
    }

    /// A conversion field (`USING`) for each loaded column whose type the form changes.
    fn conversions(&mut self, ui: &mut Ui) {
        let Some(table) = self.loaded() else {
            return;
        };
        let loaded_type = |origin: &str| {
            table
                .spec
                .columns
                .iter()
                .find(|column| column.name == origin)
                .map(|column| column.data_type.trim().to_owned())
        };
        let retyped: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.origin
                    .as_deref()
                    .and_then(loaded_type)
                    .as_deref()
                    .is_some_and(|before| before != row.data_type.trim())
            })
            .map(|(index, _)| index)
            .collect();
        if retyped.is_empty() {
            return;
        }
        ui.add_space(4.0);
        ui.label("Convert the values of retyped columns (USING, optional):");
        Grid::new("table_conversions")
            .num_columns(2)
            .show(ui, |ui| {
                for index in retyped {
                    let row = &mut self.columns[index];
                    ui.label(row.name.trim());
                    text_cell(
                        ui,
                        &mut row.conversion,
                        EXPRESSION_WIDTH,
                        &format!("Column {} conversion", index + 1),
                    );
                    ui.end_row();
                }
            });
    }

    fn unique_keys_tab(&mut self, ui: &mut Ui) {
        let columns = self.column_names();
        let mut removed = None;
        Grid::new("table_unique_keys")
            .num_columns(3)
            .show(ui, |ui| {
                for heading in ["Name (optional)", "Columns", ""] {
                    ui.strong(heading);
                }
                ui.end_row();
                for (index, key) in self.unique_keys.iter_mut().enumerate() {
                    let row = index + 1;
                    text_cell(
                        ui,
                        &mut key.name,
                        NAME_WIDTH,
                        &format!("Unique key {row} name"),
                    );
                    column_picker(
                        ui,
                        &mut key.columns,
                        &columns,
                        &format!("Unique key {row} columns"),
                    );
                    if remove_button(ui, &format!("Remove unique key {row}")).clicked() {
                        removed = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = removed {
            self.unique_keys.remove(index);
        }
        if ui.button("Add unique key").clicked() {
            self.unique_keys.push(UniqueRow::default());
        }
    }

    fn foreign_keys_tab(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        let columns = self.column_names();
        let tables = cached_tables(context.catalog, &self.folder);
        let actions = context.target.dialect.capabilities().referential_actions;
        let mut removed = None;
        for (index, key) in self.foreign_keys.iter_mut().enumerate() {
            let row = index + 1;
            ui.group(|ui| {
                Grid::new(("table_foreign_key", index))
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Name (optional)");
                        text_cell(
                            ui,
                            &mut key.name,
                            NAME_WIDTH,
                            &format!("Foreign key {row} name"),
                        );
                        ui.end_row();
                        ui.label("Columns");
                        column_picker(
                            ui,
                            &mut key.columns,
                            &columns,
                            &format!("Foreign key {row} columns"),
                        );
                        ui.end_row();
                        ui.label("References");
                        target_picker(ui, key, &tables, row);
                        ui.end_row();
                        ui.label("Their columns");
                        let target_columns = match &key.target {
                            Some(Target::ItSelf) => Some(columns.clone()),
                            Some(Target::Table { path, .. }) => {
                                loaded_columns(context.catalog, path)
                            }
                            None => Some(Vec::new()),
                        };
                        match target_columns {
                            Some(target_columns) => column_picker(
                                ui,
                                &mut key.target_columns,
                                &target_columns,
                                &format!("Foreign key {row} referenced columns"),
                            ),
                            None => {
                                ui.weak("Loading its columns…");
                            }
                        }
                        ui.end_row();
                        if actions.len() > 1 {
                            ui.label("On delete");
                            action_picker(ui, &mut key.on_delete, actions, row, "delete");
                            ui.end_row();
                            ui.label("On update");
                            action_picker(ui, &mut key.on_update, actions, row, "update");
                            ui.end_row();
                        }
                    });
                if remove_button(ui, &format!("Remove foreign key {row}")).clicked() {
                    removed = Some(index);
                }
            });
        }
        if let Some(index) = removed {
            self.foreign_keys.remove(index);
        }
        if ui.button("Add foreign key").clicked() {
            self.foreign_keys.push(ForeignRow::default());
        }
    }

    fn checks_tab(&mut self, ui: &mut Ui) {
        let mut removed = None;
        Grid::new("table_checks").num_columns(3).show(ui, |ui| {
            for heading in ["Name (optional)", "Condition", ""] {
                ui.strong(heading);
            }
            ui.end_row();
            for (index, check) in self.checks.iter_mut().enumerate() {
                let row = index + 1;
                text_cell(
                    ui,
                    &mut check.name,
                    NAME_WIDTH,
                    &format!("Check {row} name"),
                );
                text_cell(
                    ui,
                    &mut check.expression,
                    EXPRESSION_WIDTH,
                    &format!("Check {row} condition"),
                );
                if remove_button(ui, &format!("Remove check {row}")).clicked() {
                    removed = Some(index);
                }
                ui.end_row();
            }
        });
        if let Some(index) = removed {
            self.checks.remove(index);
        }
        if ui.button("Add check").clicked() {
            self.checks.push(CheckRow::default());
        }
    }
}

impl ObjectForm for TableForm {
    fn title(&self) -> String {
        match &self.purpose {
            Purpose::New => "New table".to_owned(),
            Purpose::Modify { table, .. } => format!(
                "Modify table {}",
                table
                    .last()
                    .map(|table| table.name.as_str())
                    .unwrap_or_default()
            ),
        }
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        let capabilities = context.target.dialect.capabilities();
        match &self.purpose {
            Purpose::Modify {
                loaded: Loaded::Loading,
                ..
            } => {
                ui.weak("Loading the table…");
                return;
            }
            Purpose::Modify {
                loaded: Loaded::Failed(error),
                ..
            } => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            _ => {}
        }
        let modifying = matches!(self.purpose, Purpose::Modify { .. });
        Grid::new("table_name").num_columns(2).show(ui, |ui| {
            let label = ui.label("Schema");
            // A table stays in its schema: moving it is another statement on every engine.
            if modifying {
                ui.label(&self.schema);
            } else {
                ui.add(TextEdit::singleline(&mut self.schema).desired_width(NAME_WIDTH * 2.0))
                    .labelled_by(label.id);
            }
            ui.end_row();
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.name).desired_width(NAME_WIDTH * 2.0))
                .labelled_by(label.id);
            ui.end_row();
        });
        ui.horizontal(|ui| {
            for tab in Tab::offered(capabilities) {
                ui.selectable_value(&mut self.tab, tab, tab.label());
            }
        });
        ui.separator();
        match self.tab {
            Tab::Columns => self.columns_tab(ui, capabilities),
            Tab::UniqueKeys => self.unique_keys_tab(ui),
            Tab::ForeignKeys => self.foreign_keys_tab(ui, context),
            Tab::Checks => self.checks_tab(ui),
        }
    }

    /// The schema's tables, for picking a foreign key's target, and each target's columns.
    fn catalog_requests(&self) -> Vec<CatalogPath> {
        let mut requests = vec![self.folder.clone()];
        for key in &self.foreign_keys {
            if let Some(Target::Table { path, .. }) = &key.target {
                requests.push(columns_folder(path));
            }
        }
        requests
    }

    fn table_request(&self) -> Option<CatalogPath> {
        match &self.purpose {
            Purpose::New => None,
            Purpose::Modify { table, .. } => Some(table.clone()),
        }
    }

    fn table_loaded(&mut self, result: &Result<ExistingTable, String>) {
        let loaded = match result {
            Ok(table) => {
                self.fill(table);
                Loaded::Ready(Box::new(table.clone()))
            }
            Err(error) => Loaded::Failed(format!("Could not load the table: {error}")),
        };
        if let Purpose::Modify { loaded: slot, .. } = &mut self.purpose {
            *slot = loaded;
        }
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        match (&self.purpose, self.loaded()) {
            (Purpose::New, _) => Ok(vec![DdlOperation::CreateTable(self.spec())]),
            (Purpose::Modify { .. }, Some(table)) => Ok(vec![DdlOperation::AlterTable(Box::new(
                self.alteration(table),
            ))]),
            (Purpose::Modify { .. }, None) => Err(DdlError::NothingChanged),
        }
    }

    fn changed(&self) -> CatalogPath {
        self.folder.clone()
    }

    /// Dropping columns loses the values in them, and a rebuild drops the table after
    /// copying its rows: both are spelt out before Execute.
    fn destruction(&self, dialect: Dialect) -> Option<Destruction> {
        let table = self.loaded()?;
        let qualified = table.spec.name.to_sql(dialect);
        let dropped = self.dropped_columns();
        let rebuilds = self.alteration(table).rebuilds(dialect);
        let mut parts = Vec::new();
        if let Some((first, rest)) = dropped.split_first() {
            let names = rest.iter().fold((*first).to_owned(), |names, name| {
                format!("{names}, {name}")
            });
            let (noun, them) = if rest.is_empty() {
                ("column", "it")
            } else {
                ("columns", "them")
            };
            parts.push(format!(
                "Drops the {noun} {names} of the table {qualified}, with every value in {them}."
            ));
        }
        if rebuilds {
            parts.push(format!(
                "{} can't make this change in place, so it rebuilds the table {qualified}: \
                 creates it again under another name, copies every row into it, drops the old \
                 one, gives the new one its name, and creates its indexes and triggers again, \
                 all in one transaction.",
                dialect.display_name()
            ));
        }
        let (summary, consequences) = parts.split_first()?;
        Some(Destruction {
            name: table.spec.name.name.clone(),
            summary: summary.clone(),
            consequences: consequences.to_vec(),
        })
    }
}

fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A text field in a grid, named for screen readers (and tests) since the grid's heading is
/// its only visible label.
fn text_cell(ui: &mut Ui, text: &mut String, width: f32, label: &str) -> Response {
    // Sized explicitly: in a grid, a text field's desired width yields to the column's width
    // from the frame before, which starts as narrow as the heading.
    let height = ui.spacing().interact_size.y;
    let response = ui.add_sized([width, height], TextEdit::singleline(text));
    let value = text.clone();
    response.widget_info(|| {
        let mut info = WidgetInfo::text_edit(true, &value, &value, "");
        info.label = Some(label.to_owned());
        info
    });
    response
}

/// The type as typed, and a menu of the dialect's common types beside it.
fn type_cell(ui: &mut Ui, column: &mut ColumnRow, common: &[&str], row: usize) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        text_cell(
            ui,
            &mut column.data_type,
            TYPE_WIDTH,
            &format!("Column {row} type"),
        );
        let menu = widgets::menu_button(ui, widgets::glyph(icons::CARET_DOWN), |ui| {
            for data_type in common {
                if ui.button(*data_type).clicked() {
                    column.data_type = (*data_type).to_owned();
                    ui.close();
                }
            }
        });
        widgets::name(
            menu.response,
            WidgetType::Button,
            &format!("Column {row} common types"),
            true,
        );
        if column.identity.is_some() {
            ui.weak("identity")
                .on_hover_text("The database numbers this column itself");
        }
    });
}

fn remove_button(ui: &mut Ui, label: &str) -> Response {
    widgets::IconButton::new(icons::X_CIRCLE, label).show(ui)
}

fn target_picker(
    ui: &mut Ui,
    key: &mut ForeignRow,
    tables: &[(CatalogPath, ObjectName)],
    row: usize,
) {
    let selected = match &key.target {
        None => "Choose…".to_owned(),
        Some(Target::ItSelf) => "This table".to_owned(),
        Some(Target::Table { name, .. }) => qualified_label(name),
    };
    let before = key.target.clone();
    let combo = egui::ComboBox::from_id_salt(("foreign_key_target", row))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut key.target, Some(Target::ItSelf), "This table");
            for (path, name) in tables {
                let target = Some(Target::Table {
                    path: path.clone(),
                    name: name.clone(),
                });
                ui.selectable_value(&mut key.target, target, qualified_label(name));
            }
        });
    widgets::name(
        combo.response,
        WidgetType::ComboBox,
        &format!("Foreign key {row} references"),
        true,
    );
    if key.target != before {
        key.target_columns.clear();
    }
}

fn action_picker(
    ui: &mut Ui,
    action: &mut ReferentialAction,
    actions: &[ReferentialAction],
    row: usize,
    event: &str,
) {
    let combo = egui::ComboBox::from_id_salt(("foreign_key_action", row, event))
        .selected_text(action.sql())
        .show_ui(ui, |ui| {
            for option in actions {
                ui.selectable_value(action, *option, option.sql());
            }
        });
    widgets::name(
        combo.response,
        WidgetType::ComboBox,
        &format!("Foreign key {row} on {event}"),
        true,
    );
}

fn qualified_label(name: &ObjectName) -> String {
    match &name.schema {
        Some(schema) => format!("{schema}.{}", name.name),
        None => name.name.clone(),
    }
}

/// Every table loaded so far in the schemas beside the new table's, to point a foreign key
/// at. Only what the explorer or completion has loaded: listing a schema's tables is a query,
/// and the new table's own schema is asked for (see `catalog_requests`).
fn cached_tables(catalog: &CatalogTree, folder: &CatalogPath) -> Vec<(CatalogPath, ObjectName)> {
    let schemas_parent = folder
        .parent()
        .and_then(|schema| schema.parent())
        .unwrap_or_default();
    let Some(schemas) = catalog.loaded_children(&schemas_parent) else {
        return Vec::new();
    };
    schemas
        .iter()
        .filter(|schema| !schema.system && schema.kind() == Some(NodeKind::Schema))
        .flat_map(|schema| {
            let tables = schema.path.child(
                NodeKind::Folder(FolderKind::Tables),
                FolderKind::Tables.label(),
            );
            catalog
                .loaded_children(&tables)
                .unwrap_or_default()
                .iter()
                .filter(|table| table.kind() == Some(NodeKind::Table))
                .map(|table| {
                    let name = ObjectName::new(Some(&schema.label), &table.label);
                    (table.path.clone(), name)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables_folder() -> CatalogPath {
        CatalogPath::root()
            .child(NodeKind::Schema, "main")
            .child(NodeKind::Folder(FolderKind::Tables), "tables")
    }

    #[test]
    fn opens_on_a_schema_or_its_tables_folder_only() {
        let schema = CatalogPath::root().child(NodeKind::Schema, "main");
        let from_schema = TableForm::new_table(schema.clone(), "integer").map(|form| form.folder);
        let from_folder = TableForm::new_table(tables_folder(), "integer").map(|form| form.folder);
        assert_eq!(from_schema, Some(tables_folder()));
        assert_eq!(from_folder, Some(tables_folder()));
        let table = tables_folder().child(NodeKind::Table, "orders");
        assert!(TableForm::new_table(table, "integer").is_none());
    }

    #[test]
    fn writes_the_spec_the_rows_describe() {
        let mut form = TableForm::new_table(tables_folder(), "integer").unwrap();
        form.name = " orders ".to_owned();
        form.columns.push(ColumnRow {
            name: "customer".to_owned(),
            data_type: "integer".to_owned(),
            default: " ".to_owned(),
            ..ColumnRow::added()
        });
        form.foreign_keys.push(ForeignRow {
            columns: vec!["customer".to_owned()],
            target: Some(Target::Table {
                path: tables_folder().child(NodeKind::Table, "customers"),
                name: ObjectName::new(Some("main"), "customers"),
            }),
            target_columns: vec!["id".to_owned()],
            on_delete: ReferentialAction::Cascade,
            ..ForeignRow::default()
        });
        form.checks.push(CheckRow {
            name: String::new(),
            expression: "customer > 0".to_owned(),
        });
        let spec = form.spec();
        assert_eq!(spec.name, ObjectName::new(Some("main"), "orders"));
        assert_eq!(spec.primary_key, ["id"]);
        assert!(!spec.columns[0].nullable, "a key column is never nullable");
        assert_eq!(spec.columns[1].default, None);
        assert_eq!(spec.foreign_keys[0].target.name, "customers");
        assert_eq!(spec.checks[0].name, None);
    }

    fn loaded_orders() -> ExistingTable {
        let column = |name: &str, nullable: bool| ColumnSpec {
            name: name.to_owned(),
            data_type: "integer".to_owned(),
            nullable,
            default: None,
            identity: None,
        };
        ExistingTable::from(TableSpec {
            name: ObjectName::new(Some("main"), "orders"),
            columns: vec![
                column("id", false),
                column("customer", true),
                column("legacy", true),
            ],
            primary_key: vec!["id".to_owned()],
            unique_keys: vec![],
            foreign_keys: vec![ForeignKey {
                name: None,
                columns: vec!["customer".to_owned()],
                target: ObjectName::new(Some("main"), "orders"),
                target_columns: vec!["id".to_owned()],
                on_delete: ReferentialAction::NoAction,
                on_update: ReferentialAction::NoAction,
            }],
            checks: vec![],
        })
    }

    fn modify_orders() -> TableForm {
        let path = tables_folder().child(NodeKind::Table, "orders");
        let mut form = TableForm::modify(path).unwrap();
        assert_eq!(
            form.operations(),
            Err(DdlError::NothingChanged),
            "still loading"
        );
        form.table_loaded(&Ok(loaded_orders()));
        form
    }

    #[test]
    fn modify_starts_from_the_loaded_table_and_changes_nothing_until_edited() {
        let form = modify_orders();
        assert_eq!(form.title(), "Modify table orders");
        assert_eq!(form.columns.len(), 3);
        assert_eq!(form.foreign_keys[0].target, Some(Target::ItSelf));
        let statements = dbrover_sql::ddl_statements(&form.operations().unwrap(), Dialect::Sqlite);
        assert_eq!(statements, Err(DdlError::NothingChanged));
        assert_eq!(form.destruction(Dialect::Sqlite), None);
    }

    #[test]
    fn renaming_a_column_renames_it_in_its_keys() {
        let mut form = modify_orders();
        form.columns[0].name = "number".to_owned();
        form.rename_in_keys("id", "number");
        assert_eq!(form.foreign_keys[0].target_columns, ["number"]);
        form.rename_in_keys("customer", "buyer");
        assert_eq!(form.foreign_keys[0].columns, ["buyer"]);
    }

    #[test]
    fn a_renamed_row_is_a_rename_and_a_removed_one_a_drop() {
        let mut form = modify_orders();
        form.foreign_keys.clear();
        if let Some(Loaded::Ready(table)) = match &mut form.purpose {
            Purpose::Modify { loaded, .. } => Some(loaded),
            Purpose::New => None,
        } {
            table.spec.foreign_keys.clear();
        }
        form.columns[2].name = "archived".to_owned();
        form.columns.remove(1);
        form.columns.push(ColumnRow {
            name: "note".to_owned(),
            data_type: "text".to_owned(),
            ..ColumnRow::added()
        });
        let statements = dbrover_sql::ddl_statements(&form.operations().unwrap(), Dialect::Sqlite);
        assert_eq!(
            statements,
            Ok(vec![
                "ALTER TABLE main.orders DROP COLUMN customer".to_owned(),
                "ALTER TABLE main.orders RENAME COLUMN legacy TO archived".to_owned(),
                "ALTER TABLE main.orders ADD COLUMN note text".to_owned(),
            ])
        );
        let destruction = form.destruction(Dialect::Sqlite).unwrap();
        assert_eq!(
            destruction.summary,
            "Drops the column customer of the table main.orders, with every value in it."
        );
    }
}
