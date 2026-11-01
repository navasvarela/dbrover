//! Copying the selected cells in an export format: **Copy as ›**, **Copy with headers** and their
//! keys.

use dbrover_core::Row;
use dbrover_export::{
    CustomFormat, ExportColumn, ExportContext, ExportFormat, ExportOptions, ExportSpec,
    HtmlDocument, RowSource, TsvOptions, try_render,
};

/// The formats the clipboard takes: those that write text. Its own type, so a key can't be
/// bound to copying as Excel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClipboardFormat {
    Csv,
    Tsv,
    Markdown,
    SqlInsert,
    Json,
    JsonLines,
    Html,
    SqlUpdate,
}

impl ClipboardFormat {
    /// In the order the export formats are listed.
    pub const ALL: [Self; 8] = [
        Self::Csv,
        Self::Tsv,
        Self::Markdown,
        Self::SqlInsert,
        Self::Json,
        Self::JsonLines,
        Self::Html,
        Self::SqlUpdate,
    ];

    pub fn format(self) -> ExportFormat {
        match self {
            Self::Csv => ExportFormat::Csv,
            Self::Tsv => ExportFormat::Tsv,
            Self::Markdown => ExportFormat::Markdown,
            Self::SqlInsert => ExportFormat::SqlInsert,
            Self::Json => ExportFormat::Json,
            Self::JsonLines => ExportFormat::JsonLines,
            Self::Html => ExportFormat::Html,
            Self::SqlUpdate => ExportFormat::SqlUpdate,
        }
    }

    /// The keymap id: `grid.copy-as.` and the format's id.
    pub fn keymap_id(self) -> &'static str {
        match self {
            Self::Csv => "grid.copy-as.csv",
            Self::Tsv => "grid.copy-as.tsv",
            Self::Markdown => "grid.copy-as.markdown",
            Self::SqlInsert => "grid.copy-as.sql-insert",
            Self::Json => "grid.copy-as.json",
            Self::JsonLines => "grid.copy-as.jsonl",
            Self::Html => "grid.copy-as.html",
            Self::SqlUpdate => "grid.copy-as.sql-update",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Csv => "Copy the selected cells as CSV",
            Self::Tsv => "Copy the selected cells as TSV, with their column names",
            Self::Markdown => "Copy the selected cells as a Markdown table",
            Self::SqlInsert => "Copy the selected rows as INSERT statements",
            Self::Json => "Copy the selected rows as a JSON array",
            Self::JsonLines => "Copy the selected rows as JSON Lines",
            Self::Html => "Copy the selected cells as an HTML table",
            Self::SqlUpdate => "Copy the selected rows as UPDATE statements",
        }
    }
}

/// What to put on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardCopy {
    /// TSV with the header line, whatever TSV's options in the export dialog.
    WithHeaders,
    /// The format with the export dialog's options for it, HTML as a table without a page.
    As(ClipboardFormat),
    /// A format from `exports.toml` that writes text, as it is defined there.
    Custom(Box<CustomFormat>),
}

/// The selected cells, as [`ClipboardCopy`] turns them into text.
pub struct Selection {
    pub columns: Vec<ExportColumn>,
    pub rows: Vec<Row>,
    /// The selected rows' documents whole, one a row, for a result of documents: what JSON and
    /// JSON Lines copy.
    pub documents: Vec<Row>,
    /// Where the rows come from, narrowed to the selected columns.
    pub source: RowSource,
    /// What a template is told about the rows.
    pub context: ExportContext,
}

impl ClipboardCopy {
    /// The text to copy, or why this selection can't be copied this way: the format can't
    /// write these rows, or a template stopped.
    pub fn text(&self, selection: &Selection, options: &ExportOptions) -> Result<String, String> {
        let spec = match self {
            Self::WithHeaders => ExportSpec::Tsv(TsvOptions::default()),
            Self::As(format) => {
                let mut options = options.clone();
                options.html.document = HtmlDocument::Fragment;
                options.spec(format.format(), &selection.source)?
            }
            Self::Custom(format) => format.spec(&selection.source, &selection.context)?,
        };
        let rows = if spec.writes_documents() {
            &selection.documents
        } else {
            &selection.rows
        };
        match try_render(&spec, &selection.columns, rows) {
            Some(text) => text.map_err(|error| error.to_string()),
            None => Err("This format writes only files".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_as_every_format_that_writes_text() {
        let text_formats: Vec<ExportFormat> = ExportFormat::ALL
            .into_iter()
            .filter(|format| format.writes_text())
            .collect();
        let clipboard: Vec<ExportFormat> = ClipboardFormat::ALL
            .into_iter()
            .map(ClipboardFormat::format)
            .collect();
        assert_eq!(clipboard, text_formats);
        for format in ClipboardFormat::ALL {
            assert_eq!(
                format.keymap_id(),
                format!("grid.copy-as.{}", format.format().id())
            );
        }
    }
}
