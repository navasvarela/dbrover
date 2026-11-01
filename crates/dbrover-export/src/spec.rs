//! The formats, and a format with its options: what an export is asked to write.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ExportColumn;
use crate::options::ExportOptions;
use crate::parquet::ParquetOptions;
use crate::template::{TemplateFormat, TemplateSpec};
use crate::text::{
    Csv, CsvOptions, Html, HtmlOptions, Insert, InsertSpec, Json, JsonDocuments, JsonLines,
    JsonLinesDocuments, Markdown, MarkdownOptions, RowSource, TextFormat, Tsv, TsvOptions, Update,
    UpdateSpec,
};
use crate::writer::{FileFormat, TextFile};
use crate::xlsx::{XlsxFile, XlsxOptions, xlsx_sheets};

/// A format DbRover writes. What each one is, where it can go and what its file is called are
/// methods here, so adding a format is one compiler-checked change.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    #[default]
    Csv,
    Xlsx,
    Tsv,
    Markdown,
    SqlInsert,
    Json,
    JsonLines,
    Parquet,
    Html,
    SqlUpdate,
}

impl ExportFormat {
    /// In the order menus list them: the most asked for first.
    pub const ALL: [Self; 10] = [
        Self::Csv,
        Self::Xlsx,
        Self::Tsv,
        Self::Markdown,
        Self::SqlInsert,
        Self::Json,
        Self::JsonLines,
        Self::Parquet,
        Self::Html,
        Self::SqlUpdate,
    ];

    /// The format's name, as menus and the save dialog show it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Xlsx => "Excel",
            Self::Tsv => "TSV",
            Self::Markdown => "Markdown",
            Self::SqlInsert => "SQL INSERT",
            Self::Json => "JSON",
            Self::JsonLines => "JSON Lines",
            Self::Parquet => "Parquet",
            Self::Html => "HTML",
            Self::SqlUpdate => "SQL UPDATE",
        }
    }

    /// A stable name for the format, for key bindings (`grid.copy-as.<id>`) and MCP; never
    /// changed once released.
    pub fn id(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
            Self::Tsv => "tsv",
            Self::Markdown => "markdown",
            Self::SqlInsert => "sql-insert",
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::Parquet => "parquet",
            Self::Html => "html",
            Self::SqlUpdate => "sql-update",
        }
    }

    /// The format whose [`ExportFormat::id`] is `id`.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.id() == id)
    }

    /// Whether the format writes text, which can go on the clipboard or into an MCP answer,
    /// rather than only files.
    pub fn writes_text(self) -> bool {
        !matches!(self, Self::Xlsx | Self::Parquet)
    }

    /// The file name extension, without its dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
            Self::Tsv => "tsv",
            Self::Markdown => "md",
            Self::SqlInsert | Self::SqlUpdate => "sql",
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::Parquet => "parquet",
            Self::Html => "html",
        }
    }

    /// What the format writes, for a tooltip.
    pub fn description(self) -> &'static str {
        match self {
            Self::Csv => "Comma-separated values, as RFC 4180 describes them",
            Self::Xlsx => "An Excel workbook, with numbers and dates typed and the header frozen",
            Self::Tsv => "Tab-separated values, as Copy puts them on the clipboard",
            Self::Markdown => "A table for a pull request, an issue or a chat",
            Self::SqlInsert => "INSERT statements that put the rows into their table",
            Self::Json => "An array of objects, one a row, every value a string",
            Self::JsonLines => "One JSON object a line, as data pipelines and jq -c read",
            Self::Parquet => "A typed, compressed file for data tools: DuckDB, pandas, Spark",
            Self::Html => "A web page with the rows as a table",
            Self::SqlUpdate => "UPDATE statements that set each row's values, by its primary key",
        }
    }

    /// The format with every option at its default, for rows from `source`.
    ///
    /// # Errors
    ///
    /// Why the format can't write these rows: the SQL formats need to know their table.
    pub fn default_spec(self, source: &RowSource) -> Result<ExportSpec, &'static str> {
        ExportOptions::default().spec(self, source)
    }
}

/// A format with its options. Each format has its own options type, so no option can be
/// given to a format it doesn't apply to. The SQL formats, which carry their table, are boxed
/// to keep the commands that carry a spec small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportSpec {
    Csv(CsvOptions),
    Xlsx(XlsxOptions),
    Tsv(TsvOptions),
    Markdown(MarkdownOptions),
    SqlInsert(Box<InsertSpec>),
    /// An array of objects, values as strings: what DbRover has always written.
    Json,
    JsonLines,
    /// An array of documents (ADR 0006), each as it came rather than flattened: each row is
    /// one cell holding a document's JSON.
    JsonDocuments,
    /// One document a line, as it came.
    JsonLinesDocuments,
    Parquet(ParquetOptions),
    Html(HtmlOptions),
    SqlUpdate(Box<UpdateSpec>),
    /// A template from `exports.toml` (W8c), with what it writes about.
    Template(Box<TemplateSpec>),
}

impl ExportSpec {
    /// The built-in format written; `None` for a format of the user's own.
    pub fn format(&self) -> Option<ExportFormat> {
        Some(match self {
            Self::Csv(_) => ExportFormat::Csv,
            Self::Xlsx(_) => ExportFormat::Xlsx,
            Self::Tsv(_) => ExportFormat::Tsv,
            Self::Markdown(_) => ExportFormat::Markdown,
            Self::SqlInsert(_) => ExportFormat::SqlInsert,
            Self::Json | Self::JsonDocuments => ExportFormat::Json,
            Self::JsonLines | Self::JsonLinesDocuments => ExportFormat::JsonLines,
            Self::Parquet(_) => ExportFormat::Parquet,
            Self::Html(_) => ExportFormat::Html,
            Self::SqlUpdate(_) => ExportFormat::SqlUpdate,
            Self::Template(_) => return None,
        })
    }

    /// Whether the format writes documents whole, so its rows must be the documents, one cell
    /// each, rather than their flattened columns.
    pub fn writes_documents(&self) -> bool {
        matches!(self, Self::JsonDocuments | Self::JsonLinesDocuments)
    }

    /// How many sheets `rows` rows fill, for a format that has sheets.
    pub fn sheets(&self, rows: usize) -> Option<usize> {
        match self {
            Self::Xlsx(options) => Some(xlsx_sheets(rows, options.header)),
            _ => None,
        }
    }

    /// What writes this format: the one place each format's writer is chosen.
    fn writer(&self) -> Writer {
        Writer::Text(match self {
            Self::Xlsx(options) => return Writer::Xlsx(options.clone()),
            Self::Parquet(_) => return Writer::Engine,
            Self::Csv(options) => Box::new(Csv::new(options.clone())),
            Self::Tsv(options) => Box::new(Tsv::new(options.clone())),
            Self::Markdown(options) => Box::new(Markdown::new(options.clone())),
            Self::SqlInsert(spec) => Box::new(Insert::new(spec.as_ref().clone())),
            Self::Json => Box::<Json>::default(),
            Self::JsonLines => Box::<JsonLines>::default(),
            Self::JsonDocuments => Box::<JsonDocuments>::default(),
            Self::JsonLinesDocuments => Box::new(JsonLinesDocuments),
            Self::Html(options) => Box::new(Html::new(options.clone())),
            Self::SqlUpdate(spec) => Box::new(Update::new(spec.as_ref().clone())),
            Self::Template(spec) => Box::new(TemplateFormat::new(spec)),
        })
    }

    /// The format's writer for a file at `path` with `columns`.
    ///
    /// Parquet is refused: DuckDB writes it, which the engine has and this crate doesn't, so
    /// that the crates writing only text don't carry DuckDB. The engine starts it with
    /// `ExportWriter::with_format`.
    pub(crate) fn file_format(
        &self,
        path: &Path,
        columns: &[ExportColumn],
    ) -> io::Result<Box<dyn FileFormat>> {
        Ok(match self.writer() {
            Writer::Text(format) => Box::new(TextFile::create(path, format, columns)?),
            Writer::Xlsx(options) => Box::new(XlsxFile::create(path, options, columns)?),
            Writer::Engine => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Parquet files are written by the engine",
                ));
            }
        })
    }

    /// A writer of this format, ready for its header; `None` for a format that only writes
    /// files.
    pub(crate) fn text_format(&self) -> Option<Box<dyn TextFormat>> {
        match self.writer() {
            Writer::Text(format) => Some(format),
            Writer::Xlsx(_) | Writer::Engine => None,
        }
    }
}

/// How a format is written: as text, which can go anywhere, or as a file of its own kind.
enum Writer {
    Text(Box<dyn TextFormat>),
    Xlsx(XlsxOptions),
    /// Written by the engine, with DuckDB.
    Engine,
}

/// The name of the CSV preset Excel opens with accents intact.
pub const CSV_FOR_EXCEL: &str = "CSV for Excel";

/// An entry in a list of formats to export to: a format with its defaults, or a preset of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportChoice {
    pub name: &'static str,
    pub format: ExportFormat,
    /// What the entry writes, for a tooltip.
    pub description: &'static str,
    /// The format to write, or why it can't write these rows.
    pub spec: Result<ExportSpec, &'static str>,
}

/// Every format at its defaults for rows from `source`, in [`ExportFormat::ALL`]'s order, with
/// **CSV for Excel** after CSV.
pub fn export_choices(source: &RowSource) -> Vec<ExportChoice> {
    let mut choices: Vec<ExportChoice> = ExportFormat::ALL
        .into_iter()
        .map(|format| ExportChoice {
            name: format.name(),
            format,
            description: format.description(),
            spec: format.default_spec(source),
        })
        .collect();
    choices.insert(
        1,
        ExportChoice {
            name: CSV_FOR_EXCEL,
            format: ExportFormat::Csv,
            description: "CSV with a byte order mark, so Excel reads accents and other \
                          non-English letters. Excel set to a language that separates lists \
                          with semicolons may still put each line in one column.",
            spec: Ok(ExportSpec::Csv(CsvOptions::for_excel())),
        },
    );
    choices
}
