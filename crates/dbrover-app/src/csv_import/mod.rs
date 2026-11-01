//! Import CSV… on a table: the file, which of its columns go into which of the table's, a
//! preview with the values that look wrong, and the import, which the engine runs in one
//! transaction.

mod preview;

use std::path::PathBuf;

use dbrover_core::{
    CatalogPath, DataSource, DataSourceId, Identifier, NodeKind, RelationName, SafetyLevel,
    TableDescription,
};
use dbrover_engine::{Command, Event, ImportFailure, RowsImport};
use dbrover_language::QueryLanguage;
use dbrover_sql::Dialect;
use dbrover_store::Separator;
use egui::{Align, Button, ComboBox, Grid, Id, Layout, Modal, RichText, ScrollArea, Ui};

use crate::form;
use crate::icons;
use crate::theme::Palette;
use crate::widgets;
use preview::{Preview, Warning};

const DIALOG_WIDTH: f32 = 640.0;
const SECTION_HEIGHT: f32 = 180.0;
/// How many warnings are listed; the rest are counted.
const WARNINGS_SHOWN: usize = 8;
const SEPARATORS: [(Separator, &str); 3] = [
    (Separator::Comma, "Comma"),
    (Separator::Semicolon, "Semicolon"),
    (Separator::Tab, "Tab"),
];

pub enum CsvImportAction {
    /// Pick another file.
    ChooseFile,
    /// Import into the data source's table, as [`Command::ImportRows`].
    Import(DataSourceId, RowsImport),
    Close,
}

/// The table the rows go into, as far as it is known.
enum Table {
    Describing,
    Described(TableDescription),
    Failed(String),
}

enum State {
    Choosing,
    /// A careful data source asks before importing.
    Confirming,
    Importing {
        rows: u64,
    },
    Done(Result<u64, ImportFailure>),
}

/// Where the rows go, and what the data source allows.
struct Target {
    data_source: DataSourceId,
    data_source_name: String,
    safety: SafetyLevel,
    dialect: Dialect,
    relation: RelationName,
    /// `main.orders`, as the heading names it.
    title: String,
}

pub struct CsvImportDialog {
    target: Target,
    table: Table,
    file: PathBuf,
    header: bool,
    separator: Separator,
    preview: Result<Preview, String>,
    /// For each of the table's columns, the file's column it comes from, if any.
    mapping: Vec<Option<usize>>,
    state: State,
}

impl CsvImportDialog {
    /// The dialog for importing `file` into the table at `path`, and the command that
    /// describes the table. `None` if `path` isn't a table's.
    pub fn open(
        data_source: &DataSource,
        path: &CatalogPath,
        file: PathBuf,
    ) -> Option<(Self, Command)> {
        let table = path.last().filter(|node| node.kind == NodeKind::Table)?;
        let schema = path.name_of(NodeKind::Schema);
        let quoted = |name: &str| Identifier {
            name: name.to_owned(),
            quoted: true,
        };
        let relation = RelationName {
            schema: schema.map(quoted),
            name: quoted(&table.name),
        };
        let dialect = QueryLanguage::from(data_source.config.driver()).dialect()?;
        let target = Target {
            data_source: data_source.id,
            data_source_name: data_source.name.clone(),
            safety: data_source.safety,
            dialect,
            title: path.qualified_name(dialect.quoting())?,
            relation: relation.clone(),
        };
        let header = true;
        let separator = Separator::Comma;
        let dialog = Self {
            target,
            table: Table::Describing,
            preview: Preview::read(&file, separator, header),
            file,
            header,
            separator,
            mapping: Vec::new(),
            state: State::Choosing,
        };
        let describe = Command::DescribeTable {
            data_source: data_source.id,
            table: relation,
        };
        Some((dialog, describe))
    }

    /// Reads another file, and matches its columns again.
    pub fn set_file(&mut self, file: PathBuf) {
        self.file = file;
        self.reread();
    }

    fn reread(&mut self) {
        self.preview = Preview::read(&self.file, self.separator, self.header);
        self.mapping = match (&self.table, &self.preview) {
            (Table::Described(table), Ok(preview)) => preview.match_columns(table),
            _ => Vec::new(),
        };
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::TableDescribed {
                data_source,
                table,
                result,
            } if *data_source == self.target.data_source && *table == self.target.relation => {
                self.table = match result {
                    Ok(Some(description)) => Table::Described(description.clone()),
                    Ok(None) => Table::Failed(format!("{} isn't a table", self.target.title)),
                    Err(error) => Table::Failed(error.clone()),
                };
                self.reread();
            }
            Event::ImportProgress { data_source, rows }
                if *data_source == self.target.data_source =>
            {
                if let State::Importing { rows: imported } = &mut self.state {
                    *imported = *rows;
                }
            }
            Event::RowsImported {
                data_source,
                result,
            } if *data_source == self.target.data_source
                && matches!(self.state, State::Importing { .. }) =>
            {
                self.state = State::Done(result.clone());
            }
            _ => {}
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<CsvImportAction> {
        let modal = Modal::new(Id::new("csv_import")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() && !matches!(self.state, State::Importing { .. }) {
            return Some(CsvImportAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<CsvImportAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading(format!("Import into {}", self.target.title));
        let action = self.file_row(ui, palette);
        ui.add_space(ui.spacing().item_spacing.y);
        match (&self.table, &self.preview) {
            (Table::Describing, _) => {
                ui.label("Reading the table…");
            }
            (Table::Failed(error), _) => {
                ui.colored_label(palette.error, format!("Could not read the table: {error}"));
            }
            (_, Err(error)) => {
                ui.colored_label(palette.error, format!("Could not read the file: {error}"));
            }
            (Table::Described(table), Ok(preview)) => {
                let enabled = matches!(self.state, State::Choosing);
                ui.add_enabled_ui(enabled, |ui| mapping(ui, table, preview, &mut self.mapping));
                ui.add_space(ui.spacing().item_spacing.y);
                preview_rows(ui, table, preview, &self.mapping);
                warnings(ui, &preview::check(table, &self.mapping, preview), palette);
            }
        }
        ui.separator();
        self.status(ui, palette);
        let clicked = self.buttons(ui);
        action.or(clicked)
    }

    /// The file, Choose file…, whether it has a header, and its separator. Changing either
    /// reads the file again.
    fn file_row(&mut self, ui: &mut Ui, palette: &Palette) -> Option<CsvImportAction> {
        let choosing = matches!(self.state, State::Choosing);
        let mut action = None;
        ui.horizontal(|ui| {
            let name = self.file.file_name().map_or_else(
                || self.file.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            ui.label(RichText::new(name).color(palette.text_weak))
                .on_hover_text(self.file.display().to_string());
            if ui
                .add_enabled(choosing, Button::new("Choose file…"))
                .clicked()
            {
                action = Some(CsvImportAction::ChooseFile);
            }
        });
        let (header, separator) = (self.header, self.separator);
        ui.add_enabled_ui(choosing, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.header, "First line is a header");
                ui.label("Separator");
                let selected = SEPARATORS
                    .iter()
                    .find(|(choice, _)| *choice == self.separator)
                    .map_or("", |(_, label)| label);
                ComboBox::from_id_salt("csv_separator")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (choice, label) in SEPARATORS {
                            ui.selectable_value(&mut self.separator, choice, label);
                        }
                    });
            });
        });
        if (header, separator) != (self.header, self.separator) {
            self.reread();
        }
        action
    }

    /// What is happening, or what happened.
    fn status(&self, ui: &mut Ui, palette: &Palette) {
        match &self.state {
            State::Choosing => {
                if let Some(reason) = self.refusal() {
                    ui.colored_label(palette.warning, reason);
                }
            }
            State::Confirming => {
                ui.colored_label(
                    palette.warning,
                    format!(
                        "\u{201c}{}\u{201d} is careful: this inserts the file's rows into {}.",
                        self.target.data_source_name, self.target.title
                    ),
                );
            }
            State::Importing { rows } => {
                ui.label(format!("Importing… {rows} rows"));
            }
            State::Done(Ok(rows)) => widgets::status(
                ui,
                icons::CHECK_CIRCLE,
                palette.success,
                &format!("Imported {rows} rows into {}", self.target.title),
            ),
            State::Done(Err(failure)) => {
                let place = match failure.lines {
                    Some((first, last)) if first == last => format!("Line {first}: "),
                    Some((first, last)) => format!("Lines {first}\u{2013}{last}: "),
                    None => String::new(),
                };
                ui.colored_label(
                    palette.error,
                    format!("{place}{}. Nothing was imported.", failure.message),
                );
            }
        }
    }

    /// Why nothing can be imported here, whatever the file holds.
    fn refusal(&self) -> Option<String> {
        if self.target.safety == SafetyLevel::ReadOnly {
            return Some(format!(
                "\u{201c}{}\u{201d} is read-only, so nothing can be imported into it.",
                self.target.data_source_name
            ));
        }
        self.target.dialect.import_refusal().map(str::to_owned)
    }

    /// The import, if the file, the table and the mapping are ready for one.
    fn import(&self) -> Result<RowsImport, String> {
        if let Some(reason) = self.refusal() {
            return Err(reason);
        }
        let Table::Described(table) = &self.table else {
            return Err("The table hasn't been read".to_owned());
        };
        self.preview.as_ref().map_err(Clone::clone)?;
        let mapping: Vec<(String, usize)> = table
            .columns
            .iter()
            .zip(&self.mapping)
            .filter_map(|(column, field)| Some((column.name.clone(), (*field)?)))
            .collect();
        if mapping.is_empty() {
            return Err("Choose where at least one column comes from".to_owned());
        }
        Ok(RowsImport {
            table: table.table_name(),
            file: self.file.clone(),
            separator: self.separator,
            header: self.header,
            mapping,
        })
    }

    fn buttons(&mut self, ui: &mut Ui) -> Option<CsvImportAction> {
        let import = self.import();
        let (import_clicked, close_clicked) = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let can_import = matches!(self.state, State::Choosing | State::Confirming);
                let import_clicked = can_import
                    && form::primary_button(ui, "Import", import.is_ok())
                        .on_disabled_hover_text(import.as_ref().err().cloned().unwrap_or_default())
                        .clicked();
                let close_label = match self.state {
                    State::Choosing => "Cancel",
                    State::Confirming => "Back",
                    State::Importing { .. } | State::Done(_) => "Close",
                };
                let closable = !matches!(self.state, State::Importing { .. });
                let close_clicked = ui.add_enabled(closable, Button::new(close_label)).clicked();
                (import_clicked, close_clicked)
            })
            .inner;
        if close_clicked {
            if matches!(self.state, State::Confirming) {
                self.state = State::Choosing;
                return None;
            }
            return Some(CsvImportAction::Close);
        }
        let import = import.ok().filter(|_| import_clicked)?;
        if self.target.safety == SafetyLevel::Careful && matches!(self.state, State::Choosing) {
            self.state = State::Confirming;
            return None;
        }
        self.state = State::Importing { rows: 0 };
        Some(CsvImportAction::Import(self.target.data_source, import))
    }
}

/// For each of the table's columns, its type and the file's column it comes from, or its
/// default.
fn mapping(
    ui: &mut Ui,
    table: &TableDescription,
    preview: &Preview,
    mapping: &mut [Option<usize>],
) {
    let file_columns = preview.columns();
    ui.label(RichText::new("Columns").strong());
    ScrollArea::vertical()
        .id_salt("csv_mapping")
        .max_height(SECTION_HEIGHT)
        .show(ui, |ui| {
            Grid::new("csv_mapping_grid")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    for (index, (column, source)) in
                        table.columns.iter().zip(mapping.iter_mut()).enumerate()
                    {
                        ui.label(&column.name);
                        ui.weak(&column.data_type);
                        let shown = source
                            .and_then(|field| file_columns.get(field))
                            .map_or("(default)", String::as_str);
                        let combo = ComboBox::from_id_salt(("csv_source", index))
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(source, None, "(default)");
                                for (field, name) in file_columns.iter().enumerate() {
                                    ui.selectable_value(source, Some(field), name);
                                }
                            });
                        widgets::name(
                            combo.response,
                            egui::WidgetType::ComboBox,
                            &format!("Source of {}", column.name),
                            true,
                        );
                        ui.end_row();
                    }
                });
        });
}

/// The previewed records as they would go into the table: one column per table column,
/// empty where it gets its default.
fn preview_rows(
    ui: &mut Ui,
    table: &TableDescription,
    preview: &Preview,
    mapping: &[Option<usize>],
) {
    ui.label(RichText::new(format!("First {} rows", preview.records.len())).strong());
    ScrollArea::both()
        .id_salt("csv_preview")
        .max_height(SECTION_HEIGHT)
        .show(ui, |ui| {
            Grid::new("csv_preview_grid").striped(true).show(ui, |ui| {
                ui.weak("Line");
                for column in &table.columns {
                    ui.label(RichText::new(&column.name).strong());
                }
                ui.end_row();
                for record in &preview.records {
                    ui.weak(record.line.to_string());
                    for source in mapping {
                        match source.map(|field| record.fields.get(field)) {
                            Some(Some(value)) if !value.is_empty() => {
                                ui.monospace(value);
                            }
                            Some(_) => {
                                ui.weak("NULL");
                            }
                            None => {
                                ui.weak("DEFAULT");
                            }
                        }
                    }
                    ui.end_row();
                }
            });
        });
}

fn warnings(ui: &mut Ui, warnings: &[Warning], palette: &Palette) {
    for warning in warnings.iter().take(WARNINGS_SHOWN) {
        widgets::status(
            ui,
            icons::WARNING,
            palette.warning,
            &format!(
                "Line {}, {}: {}",
                warning.line, warning.column, warning.message
            ),
        );
    }
    if warnings.len() > WARNINGS_SHOWN {
        ui.weak(format!(
            "and {} more in the first rows",
            warnings.len() - WARNINGS_SHOWN
        ));
    }
}
