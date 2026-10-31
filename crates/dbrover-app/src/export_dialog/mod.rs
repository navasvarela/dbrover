//! The export dialog: which rows, the format, its options and a preview, then where to save
//! the file, for a console's rows, a statement run straight to a file, or a table's every row.
//! It starts from the format and options used last, which the app saves. Also the platform's
//! save dialog, for exported rows and the query history.

mod options;
mod subject;

use std::path::PathBuf;

use dbrover_core::{ResultShape, Row, ValueKind};
use dbrover_engine::{Event, Exporter, RunId};
use dbrover_export::{
    CSV_FOR_EXCEL, CsvOptions, ExportChoice, ExportColumn, ExportFormat, ExportOptions, ExportSpec,
    RowSource, SourceTable, SqlTable, export_choices, render,
};
use egui::{
    Button, Id, Modal, Response, RichText, ScrollArea, TextEdit, TextStyle, Ui, WidgetInfo,
    WidgetType, vec2,
};
use serde::{Deserialize, Serialize};

use self::options::{OptionTexts, options_ui};
use self::subject::TableProgress;
pub use self::subject::{ExportSubject, TableTarget};
use crate::console::ExportedRows;
use crate::theme::Palette;
use crate::{form, icons, widgets};

/// Where the app keeps [`ExportChoices`] between runs.
pub const STORAGE_KEY: &str = "export_choices_v1";

/// How many of the rows the preview shows.
pub const PREVIEW_ROWS: usize = 10;

const DIALOG_WIDTH: f32 = 560.0;
const PREVIEW_HEIGHT: f32 = 120.0;
/// Tiles a line in the format grid: eleven formats fit in three lines.
const FORMAT_COLUMNS: usize = 4;
const FORMAT_TILE_HEIGHT: f32 = 26.0;

/// Why a table format can't write documents read straight from the database.
const DOCUMENTS_AS_JSON: &str =
    "Documents read from the database are written whole: choose JSON or JSON Lines";

/// The format and every format's options used last: conveniences rather than configuration,
/// so they live with the window layout rather than in `settings.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportChoices {
    pub format: ExportFormat,
    pub options: ExportOptions,
}

/// What the dialog needs of the rows: their columns, the first few as the grid shows them,
/// how many are loaded, and where they came from.
#[derive(Debug, Clone)]
pub struct ExportPreview {
    pub columns: Vec<ExportColumn>,
    /// The first [`PREVIEW_ROWS`] rows, in the grid's order.
    pub sample: Vec<Row>,
    /// The same rows' documents whole, one a row, for a result of documents: what JSON and JSON
    /// Lines write.
    pub document_sample: Vec<Row>,
    pub loaded: usize,
    pub source: RowSource,
}

pub enum ExportDialogAction {
    /// Ask where to save, then export.
    Export(ExportSpec),
    /// Stop the table's export.
    Cancel(RunId),
    Close,
}

pub struct ExportDialog {
    subject: ExportSubject,
    /// Which of a console's rows; the other subjects export every row.
    rows: ExportedRows,
    choices: ExportChoices,
    preview: ExportPreview,
    /// The table an `INSERT` goes to when the rows' own isn't known.
    table: TableInput,
    texts: OptionTexts,
    progress: TableProgress,
}

/// A table name typed in, with its schema if one is given.
#[derive(Debug, Default)]
struct TableInput {
    schema: String,
    name: String,
}

impl TableInput {
    fn show(&mut self, rows: &mut form::Rows<'_>) {
        rows.full(|ui| {
            form::description_label(
                ui,
                "DbRover doesn't know which table these rows came from: name it.",
            );
        });
        rows.row("Schema", None, |ui| {
            let field = ui.add(
                TextEdit::singleline(&mut self.schema)
                    .hint_text("(the default)")
                    .desired_width(TABLE_FIELD_WIDTH),
            );
            widgets::name(field, WidgetType::TextEdit, "Schema", true);
        });
        rows.row("Table", None, |ui| {
            let field =
                ui.add(TextEdit::singleline(&mut self.name).desired_width(TABLE_FIELD_WIDTH));
            widgets::name(field, WidgetType::TextEdit, "Table", true);
        });
    }
}

const TABLE_FIELD_WIDTH: f32 = 200.0;

impl ExportDialog {
    pub fn new(subject: ExportSubject, preview: ExportPreview, remembered: &ExportChoices) -> Self {
        Self {
            subject,
            rows: ExportedRows::Loaded,
            texts: OptionTexts::new(&remembered.options),
            choices: remembered.clone(),
            preview,
            table: TableInput::default(),
            progress: TableProgress::Choosing,
        }
    }

    pub fn subject(&self) -> &ExportSubject {
        &self.subject
    }

    /// Which of a console's rows to export.
    pub fn rows(&self) -> ExportedRows {
        self.rows
    }

    /// A table's export has started: the dialog shows how it goes until it ends.
    pub fn table_export_started(&mut self, run: RunId, path: PathBuf) {
        self.progress = TableProgress::Exporting { run, path, rows: 0 };
    }

    /// Takes in a table's description and its export's progress.
    pub fn apply(&mut self, event: &Event) {
        let ExportSubject::Table(target) = &self.subject else {
            return;
        };
        let ours = |exporter: &Exporter| *exporter == Exporter::DataSource(target.data_source);
        match event {
            Event::TableDescribed {
                data_source,
                table,
                result: Ok(Some(description)),
            } if *data_source == target.data_source && *table == target.relation => {
                self.preview = target.described(description);
            }
            Event::ExportProgress { exporter, rows } if ours(exporter) => {
                if let TableProgress::Exporting { rows: written, .. } = &mut self.progress {
                    *written = *rows;
                }
            }
            Event::ExportFinished {
                exporter,
                path,
                result,
            } if ours(exporter) && matches!(self.progress, TableProgress::Exporting { .. }) => {
                self.progress = TableProgress::finished(path, result);
            }
            _ => {}
        }
    }

    /// The choices to remember once the rows are exported.
    pub fn choices(&self) -> &ExportChoices {
        &self.choices
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ExportDialogAction> {
        let modal = Modal::new(Id::new("export_rows")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(ExportDialogAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<ExportDialogAction> {
        ui.set_width(DIALOG_WIDTH);
        let (title, subtitle) = (self.subject.heading(), self.subtitle());
        let header = form::Header {
            glyph: icons::EXPORT,
            title: &title,
            subtitle: &subtitle,
        };
        header.show(ui, |ui| {
            if matches!(self.subject, ExportSubject::Grid(_)) {
                self.rows_choice(ui);
            }
        });
        // Not in a scroll area: one sizes itself from the frame before, so the modal would
        // grow over several frames, and a click meant for one format would land on the next.
        let spec = self.body(ui);
        form::footer(ui, |ui| self.actions(ui, spec))
    }

    /// The rows, the format, its options and the preview. Returns the format and options
    /// chosen, or what is wrong with them.
    fn body(&mut self, ui: &mut Ui) -> Result<ExportSpec, String> {
        let choosing = self.progress == TableProgress::Choosing;
        form::section(ui, "Format", |ui| {
            ui.add_enabled_ui(choosing, |ui| self.format_choice(ui));
        });
        let format = self.choices.format;
        let asks_for_table = format == ExportFormat::SqlInsert
            && self.preview.source.table.is_none()
            && self.can_name_table();
        form::section(ui, "Options", |ui| {
            ui.add_enabled_ui(choosing, |ui| {
                form::card(ui, |rows| {
                    options_ui(rows, format, &mut self.choices.options, &mut self.texts);
                    if asks_for_table {
                        self.table.show(rows);
                    }
                });
            });
        });
        let spec = self.spec();
        form::section(ui, "Preview", |ui| self.preview_ui(ui, spec.as_ref().ok()));
        spec
    }

    /// Export… and Cancel while choosing, the problem with the options if any; then how a
    /// table's export goes.
    fn actions(&self, ui: &mut Ui, spec: Result<ExportSpec, String>) -> Option<ExportDialogAction> {
        if self.progress != TableProgress::Choosing {
            return self.progress_row(ui);
        }
        let clicked = form::primary_button(ui, "Export…", spec.is_ok())
            .on_hover_text("Choose where to save the file")
            .clicked();
        let closed = ui.button("Cancel").clicked();
        if let Err(problem) = &spec {
            let palette = Palette::for_visuals(ui.visuals());
            ui.add(
                egui::Label::new(RichText::new(problem.as_str()).color(palette.error)).truncate(),
            );
        }
        match (clicked, closed, spec) {
            (true, _, Ok(spec)) => Some(ExportDialogAction::Export(spec)),
            (_, true, _) => Some(ExportDialogAction::Close),
            _ => None,
        }
    }

    /// What the export reads, under the heading.
    fn subtitle(&self) -> String {
        match &self.subject {
            ExportSubject::Grid(_) => match &self.preview.source.table {
                Some(table) => match &table.name.schema {
                    Some(schema) => format!("Rows of {schema}.{}", table.name.name),
                    None => format!("Rows of {}", table.name.name),
                },
                None => "The rows of the console's result".to_owned(),
            },
            ExportSubject::Statement { .. } => {
                "Every row the statement returns, written as they arrive".to_owned()
            }
            ExportSubject::Table(_) => "Every row, read a page at a time".to_owned(),
        }
    }

    /// A table's export under way, with Cancel, or how it ended, with Close.
    fn progress_row(&self, ui: &mut Ui) -> Option<ExportDialogAction> {
        let palette = Palette::for_visuals(ui.visuals());
        match &self.progress {
            TableProgress::Exporting { run, rows, .. } => {
                let cancel = ui.button("Cancel export").clicked();
                ui.spinner();
                ui.label(format!("Exporting… {rows} rows"));
                cancel.then_some(ExportDialogAction::Cancel(*run))
            }
            TableProgress::Done(result) => {
                let close = form::primary_button(ui, "Close", true).clicked();
                match result {
                    Ok(done) => ui.label(done.as_str()),
                    Err(error) => ui.colored_label(palette.error, error.as_str()),
                };
                close.then_some(ExportDialogAction::Close)
            }
            TableProgress::Choosing => None,
        }
    }

    /// Loaded or all rows of a console's result, or of its documents.
    fn rows_choice(&mut self, ui: &mut Ui) {
        let loaded = self.preview.loaded;
        let (loaded_label, all_label, all_hint) =
            if self.preview.source.shape == ResultShape::Documents {
                (
                    format!("Loaded documents: {loaded}"),
                    "All documents",
                    "Runs the command again and writes every document it returns, as JSON or \
                     JSON Lines",
                )
            } else {
                (
                    format!("Loaded rows: {loaded}"),
                    "All rows",
                    "Runs the statement again and writes every row it returns, a page at a time",
                )
            };
        form::segmented(
            ui,
            &mut self.rows,
            &[
                form::Segment::new(ExportedRows::Loaded, &loaded_label)
                    .hint("As sorted and filtered here"),
                form::Segment::new(ExportedRows::All, all_label).hint(all_hint),
            ],
        );
    }

    /// Whether the documents come straight from the database rather than from the grid: Run to
    /// file, or All documents. They aren't flattened into columns then, so only the JSON
    /// formats, which write documents whole, can write them.
    fn documents_from_database(&self) -> bool {
        self.preview.source.shape == ResultShape::Documents
            && (matches!(self.subject, ExportSubject::Statement { .. })
                || self.rows == ExportedRows::All)
    }

    /// Why `format` can't write these rows when it otherwise could.
    fn refusal(&self, format: ExportFormat) -> Option<&'static str> {
        (self.documents_from_database()
            && !matches!(format, ExportFormat::Json | ExportFormat::JsonLines))
        .then_some(DOCUMENTS_AS_JSON)
    }

    /// Why `choice` can't write these rows, or `None` when it can. `INSERT` stays available for
    /// SQL rows of no known table, since a table can be named below.
    fn unavailable(&self, choice: &ExportChoice) -> Option<&'static str> {
        if let Some(refusal) = self.refusal(choice.format) {
            return Some(refusal);
        }
        match &choice.spec {
            Err(_) if choice.format == ExportFormat::SqlInsert && self.can_name_table() => None,
            Err(reason) => Some(*reason),
            Ok(_) => None,
        }
    }

    /// Whether a table can be named for `INSERT`s of rows from no known table: only rows from a
    /// SQL database.
    fn can_name_table(&self) -> bool {
        let source = &self.preview.source;
        source.dialect.is_some() && source.shape == ResultShape::Rows
    }

    /// The formats as tiles, with CSV for Excel after CSV. A format that can't write these rows
    /// is greyed out, saying why.
    fn format_choice(&mut self, ui: &mut Ui) {
        let current = self.format_label();
        let spacing = ui.spacing().item_spacing.x;
        let tile_width =
            (ui.available_width() - spacing * (FORMAT_COLUMNS - 1) as f32) / FORMAT_COLUMNS as f32;
        let choices = export_choices(&self.preview.source);
        for line in choices.chunks(FORMAT_COLUMNS) {
            ui.horizontal(|ui| {
                for choice in line {
                    let selected = current == choice.name;
                    let unavailable = self.unavailable(choice);
                    let tile = format_tile(ui, choice, selected, unavailable, tile_width);
                    if tile.clicked() {
                        self.choose(choice.name, choice.format);
                    }
                }
            });
        }
    }

    fn choose(&mut self, name: &str, format: ExportFormat) {
        self.choices.format = format;
        if name == CSV_FOR_EXCEL {
            self.choices.options.csv = CsvOptions::for_excel();
            self.texts = OptionTexts::new(&self.choices.options);
        }
    }

    /// The format's name, or CSV for Excel while CSV has that preset's options.
    fn format_label(&self) -> &'static str {
        let format = self.choices.format;
        if format == ExportFormat::Csv && self.choices.options.csv == CsvOptions::for_excel() {
            CSV_FOR_EXCEL
        } else {
            format.name()
        }
    }

    /// The rows' source, with the typed table when they have none of their own.
    fn source(&self) -> RowSource {
        let mut source = self.preview.source.clone();
        let name = self.table.name.trim();
        if source.table.is_none() && !name.is_empty() {
            let schema = self.table.schema.trim();
            source.table = Some(SourceTable {
                name: SqlTable {
                    schema: (!schema.is_empty()).then(|| schema.to_owned()),
                    name: name.to_owned(),
                },
                columns: None,
                key: None,
            });
        }
        source
    }

    /// The format and options chosen, or what is wrong with them.
    fn spec(&self) -> Result<ExportSpec, String> {
        self.texts.problem(self.choices.format)?;
        if let Some(refusal) = self.refusal(self.choices.format) {
            return Err(refusal.to_owned());
        }
        let source = self.source();
        self.choices
            .options
            .spec(self.choices.format, &source)
            .map_err(|reason| match self.choices.format {
                ExportFormat::SqlInsert if self.can_name_table() => {
                    "Name the table to insert into".to_owned()
                }
                _ => reason.to_owned(),
            })
    }

    /// The file's first lines for a text format; for Excel and Parquet, what each column
    /// becomes.
    fn preview_ui(&self, ui: &mut Ui, spec: Option<&ExportSpec>) {
        let Some(spec) = spec else {
            form::description_label(ui, "Nothing to preview until the options are complete");
            return;
        };
        let columns = &self.preview.columns;
        if columns.is_empty() {
            form::description_label(
                ui,
                match self.subject {
                    ExportSubject::Table(_) => "The columns show once the table is described",
                    _ => "The file's columns are the statement's, known once it runs",
                },
            );
            return;
        }
        let sample = if spec.writes_documents() {
            &self.preview.document_sample
        } else {
            &self.preview.sample
        };
        match render(spec, columns, sample) {
            Some(text) => text_preview(ui, &text),
            None => column_preview(ui, spec.format(), columns),
        }
    }
}

/// A format's tile: its icon and name, chosen like a radio button.
fn format_tile(
    ui: &mut Ui,
    choice: &ExportChoice,
    selected: bool,
    unavailable: Option<&str>,
    width: f32,
) -> Response {
    let palette = Palette::for_visuals(ui.visuals());
    // The selected text's colour: the system's accent on a Mac.
    let icon_color = if selected {
        ui.visuals().selection.stroke.color
    } else {
        palette.text_weak
    };
    let text = widgets::icon_text(ui, format_glyph(choice), icon_color, choice.name);
    let tile = Button::selectable(selected, text)
        .frame_when_inactive(true)
        .min_size(vec2(width, FORMAT_TILE_HEIGHT));
    let available = unavailable.is_none();
    let mut response = ui
        .add_enabled(available, tile)
        .on_hover_text(choice.description);
    if let Some(reason) = unavailable {
        response = response.on_disabled_hover_text(reason);
    }
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::RadioButton, available, selected, choice.name)
    });
    response
}

fn format_glyph(choice: &ExportChoice) -> &'static str {
    if choice.name == CSV_FOR_EXCEL {
        return icons::MICROSOFT_EXCEL_LOGO;
    }
    match choice.format {
        ExportFormat::Csv => icons::FILE_CSV,
        ExportFormat::Xlsx => icons::FILE_XLS,
        ExportFormat::Tsv => icons::FILE_TXT,
        ExportFormat::Markdown => icons::MARKDOWN_LOGO,
        ExportFormat::SqlInsert => icons::FILE_SQL,
        ExportFormat::SqlUpdate => icons::PENCIL_SIMPLE_LINE,
        ExportFormat::Json => icons::BRACKETS_CURLY,
        ExportFormat::JsonLines => icons::LIST_BULLETS,
        ExportFormat::Parquet => icons::COLUMNS,
        ExportFormat::Html => icons::FILE_HTML,
    }
}

fn text_preview(ui: &mut Ui, text: &str) {
    let shown: String = text
        .trim_start_matches('\u{feff}')
        .lines()
        .take(PREVIEW_ROWS + 2)
        .collect::<Vec<_>>()
        .join("\n");
    form::card(ui, |rows| {
        rows.full(|ui| {
            ScrollArea::both()
                .id_salt("export_preview")
                .max_height(PREVIEW_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(shown).text_style(TextStyle::Monospace))
                            .extend(),
                    );
                });
        });
    });
}

/// Each column and what it becomes in a file with types.
fn column_preview(ui: &mut Ui, format: ExportFormat, columns: &[ExportColumn]) {
    if format == ExportFormat::Parquet {
        form::description_label(
            ui,
            "Each column takes the type its values all fit, keeping them as they are:",
        );
    }
    form::card(ui, |rows| {
        rows.full(|ui| {
            ScrollArea::vertical()
                .id_salt("export_preview")
                .max_height(PREVIEW_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new("export_column_types")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for column in columns {
                                ui.label(&column.name);
                                ui.weak(becomes(format, column.kind));
                                ui.end_row();
                            }
                        });
                });
        });
    });
}

/// What a column of `kind` becomes in `format`.
fn becomes(format: ExportFormat, kind: ValueKind) -> &'static str {
    match (format, kind) {
        (ExportFormat::Parquet, ValueKind::Integer) => "BIGINT",
        (ExportFormat::Parquet, ValueKind::Decimal) => "DECIMAL",
        (ExportFormat::Parquet, ValueKind::Float) => "DOUBLE",
        (ExportFormat::Parquet, ValueKind::Boolean) => "BOOLEAN",
        (ExportFormat::Parquet, ValueKind::Date) => "DATE",
        (ExportFormat::Parquet, ValueKind::Timestamp) => "TIMESTAMP",
        (ExportFormat::Parquet, ValueKind::TimestampWithZone) => "TIMESTAMPTZ",
        (ExportFormat::Parquet, ValueKind::Text | ValueKind::Undeclared) => {
            "From its values, else text"
        }
        (_, ValueKind::Integer | ValueKind::Decimal | ValueKind::Float) => "Numbers",
        (_, ValueKind::Boolean) => "TRUE or FALSE",
        (_, ValueKind::Date) => "Dates",
        (_, ValueKind::Timestamp) => "Dates and times",
        (_, ValueKind::TimestampWithZone) => "Text: Excel has no time zones",
        (_, ValueKind::Text) => "Text",
        (_, ValueKind::Undeclared) => "Numbers and dates where each value is one, else text",
    }
}

/// Shows the save dialog for exporting `what` ("rows", "history") and returns the chosen file,
/// or `None` if it was cancelled.
///
/// Blocks the UI thread until the dialog closes, unlike everything else in the UI (see
/// ADR-0003): the dialog is modal anyway, and macOS requires it to run on the main thread.
/// The file itself is written by the engine.
pub fn pick_export_path(format: ExportFormat, what: &str) -> Option<PathBuf> {
    let (name, extension) = (format.name(), format.extension());
    rfd::FileDialog::new()
        .set_title(format!("Export {what} as {name}"))
        .add_filter(name, &[extension])
        .set_file_name(format!("{what}.{extension}"))
        .save_file()
}
