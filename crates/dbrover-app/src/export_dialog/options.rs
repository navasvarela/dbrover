//! Each format's options in the export dialog.

use dbrover_export::{
    CsvOptions, ExportFormat, ExportOptions, FieldSeparator, HtmlDocument, LineEnding,
    MAX_ROWS_PER_INSERT, ParquetCompression, Quoting, RowsPerInsert, SheetName,
};
use egui::{Ui, WidgetType};

use crate::form::{self, Rows, Segment};
use crate::widgets;

/// The separators the dialog names; any other single character is typed in.
const SEPARATORS: [(FieldSeparator, &str); 4] = [
    (FieldSeparator::COMMA, "Comma"),
    (FieldSeparator::SEMICOLON, "Semicolon"),
    (FieldSeparator::TAB, "Tab"),
    (FieldSeparator::PIPE, "Pipe"),
];

const NULL_HINT: &str = "(an empty field)";

const SEPARATOR_FIELD_WIDTH: f32 = 32.0;
const NULL_FIELD_WIDTH: f32 = 140.0;
const TEXT_FIELD_WIDTH: f32 = 200.0;

/// Options typed as text, which may not be valid yet: the typed options keep the last valid
/// value, and [`OptionTexts::problem`] says what is wrong with the text.
pub struct OptionTexts {
    /// Whether the separator is typed rather than one of [`SEPARATORS`].
    other_separator: bool,
    separator: String,
    sheet_name: String,
}

impl OptionTexts {
    pub fn new(options: &ExportOptions) -> Self {
        let separator = options.csv.separator;
        Self {
            other_separator: !SEPARATORS.iter().any(|(named, _)| *named == separator),
            separator: separator.get().to_string(),
            sheet_name: options.xlsx.sheet_name.as_str().to_owned(),
        }
    }

    /// What is wrong with what was typed for `format`'s options.
    pub fn problem(&self, format: ExportFormat) -> Result<(), String> {
        match format {
            ExportFormat::Csv if self.other_separator => parse_separator(&self.separator)
                .map(drop)
                .map_err(|problem| problem.to_owned()),
            ExportFormat::Xlsx => SheetName::try_from(self.sheet_name.clone())
                .map(drop)
                .map_err(|problem| problem.to_string()),
            _ => Ok(()),
        }
    }
}

fn parse_separator(text: &str) -> Result<FieldSeparator, &'static str> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(separator), None) => FieldSeparator::try_from(separator)
            .map_err(|_| "A quote or a line break can't separate fields"),
        _ => Err("Type one character to separate fields"),
    }
}

/// The options of `format`, a row each.
pub fn options_ui(
    rows: &mut Rows<'_>,
    format: ExportFormat,
    options: &mut ExportOptions,
    texts: &mut OptionTexts,
) {
    match format {
        ExportFormat::Csv => csv(rows, options, texts),
        ExportFormat::Xlsx => xlsx(rows, options, texts),
        ExportFormat::Tsv => {
            header(rows, &mut options.tsv.header);
            null_text(rows, &mut options.tsv.null_text);
        }
        ExportFormat::Markdown => null_text(rows, &mut options.markdown.null_text),
        ExportFormat::SqlInsert => rows_per_statement(rows, &mut options.insert.rows_per_statement),
        ExportFormat::Html => html(rows, options),
        ExportFormat::Parquet => parquet(rows, &mut options.parquet.compression),
        ExportFormat::Json | ExportFormat::JsonLines | ExportFormat::SqlUpdate => {
            rows.full(|ui| form::description_label(ui, "This format has no options."));
        }
    }
}

fn csv(rows: &mut Rows<'_>, options: &mut ExportOptions, texts: &mut OptionTexts) {
    let csv = &mut options.csv;
    rows.row("Separator", None, |ui| separator(ui, csv, texts));
    rows.row("Quotes", None, |ui| {
        form::segmented(
            ui,
            &mut csv.quoting,
            &[
                Segment::new(Quoting::WhereNeeded, "Where needed"),
                Segment::new(Quoting::Always, "Around every value"),
            ],
        );
    });
    header(rows, &mut csv.header);
    null_text(rows, &mut csv.null_text);
    rows.row("Line ending", None, |ui| {
        form::segmented(
            ui,
            &mut csv.line_ending,
            &[
                Segment::new(LineEnding::Crlf, "CRLF")
                    .hint("As RFC 4180 asks, and Windows programs expect"),
                Segment::new(LineEnding::Lf, "LF"),
            ],
        );
    });
    toggle_row(
        rows,
        "Byte order mark",
        Some("What Excel needs to read accents in a UTF-8 file"),
        &mut csv.bom,
    );
}

/// One of [`SEPARATORS`], or a character typed in beside them. Laid out right to left, so the
/// field comes first.
fn separator(ui: &mut Ui, csv: &mut CsvOptions, texts: &mut OptionTexts) {
    if texts.other_separator {
        let field = ui.add(
            egui::TextEdit::singleline(&mut texts.separator)
                .desired_width(SEPARATOR_FIELD_WIDTH)
                .char_limit(1),
        );
        let field = widgets::name(field, WidgetType::TextEdit, "Other separator", true);
        if field.changed()
            && let Ok(separator) = parse_separator(&texts.separator)
        {
            csv.separator = separator;
        }
    }
    let mut chosen = (!texts.other_separator).then_some(csv.separator);
    let mut segments: Vec<Segment<'_, Option<FieldSeparator>>> = SEPARATORS
        .iter()
        .map(|&(separator, name)| Segment::new(Some(separator), name))
        .collect();
    segments.push(Segment::new(None, "Other"));
    if form::segmented(ui, &mut chosen, &segments) {
        match chosen {
            Some(separator) => {
                csv.separator = separator;
                texts.other_separator = false;
            }
            None => texts.other_separator = true,
        }
    }
}

fn xlsx(rows: &mut Rows<'_>, options: &mut ExportOptions, texts: &mut OptionTexts) {
    let xlsx = &mut options.xlsx;
    header(rows, &mut xlsx.header);
    let has_header = xlsx.header;
    rows.row("Keep the header in view", None, |ui| {
        ui.add_enabled_ui(has_header, |ui| {
            form::toggle(ui, &mut xlsx.freeze_header, "Keep the header in view");
        });
    });
    rows.row("Filter buttons on the header", None, |ui| {
        ui.add_enabled_ui(has_header, |ui| {
            form::toggle(ui, &mut xlsx.autofilter, "Filter buttons on the header");
        });
    });
    rows.row("Sheet name", None, |ui| {
        let field = ui
            .add(egui::TextEdit::singleline(&mut texts.sheet_name).desired_width(TEXT_FIELD_WIDTH));
        let field = widgets::name(field, WidgetType::TextEdit, "Sheet name", true);
        if field.changed()
            && let Ok(name) = SheetName::try_from(texts.sheet_name.clone())
        {
            xlsx.sheet_name = name;
        }
    });
}

fn html(rows: &mut Rows<'_>, options: &mut ExportOptions) {
    let html = &mut options.html;
    rows.row("Write", None, |ui| {
        form::segmented(
            ui,
            &mut html.document,
            &[
                Segment::new(HtmlDocument::Page, "A whole page"),
                Segment::new(HtmlDocument::Fragment, "The table only")
                    .hint("To paste into an email or a page"),
            ],
        );
    });
    null_text(rows, &mut html.null_text);
}

fn parquet(rows: &mut Rows<'_>, compression: &mut ParquetCompression) {
    rows.row("Compression", None, |ui| {
        form::segmented(
            ui,
            compression,
            &[
                Segment::new(ParquetCompression::Zstd, "zstd")
                    .hint("Smaller files; what most tools default to"),
                Segment::new(ParquetCompression::Snappy, "Snappy")
                    .hint("For tools that read only Snappy"),
            ],
        );
    });
}

fn rows_per_statement(rows: &mut Rows<'_>, per_statement: &mut RowsPerInsert) {
    rows.row(
        "Rows a statement",
        Some("Up to 1,000, SQL Server's limit for one VALUES list"),
        |ui| {
            let mut count = per_statement.get();
            ui.add(egui::DragValue::new(&mut count).range(1..=MAX_ROWS_PER_INSERT));
            *per_statement = RowsPerInsert::clamped(count);
        },
    );
}

fn header(rows: &mut Rows<'_>, header: &mut bool) {
    toggle_row(rows, "Header with the column names", None, header);
}

fn toggle_row(rows: &mut Rows<'_>, title: &str, description: Option<&str>, on: &mut bool) {
    rows.row(title, description, |ui| form::toggle(ui, on, title));
}

fn null_text(rows: &mut Rows<'_>, text: &mut String) {
    rows.row("NULL as", None, |ui| {
        let hint = if text.is_empty() { NULL_HINT } else { "" };
        let field = ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width(NULL_FIELD_WIDTH),
        );
        widgets::name(field, WidgetType::TextEdit, "NULL as", true);
    });
}
