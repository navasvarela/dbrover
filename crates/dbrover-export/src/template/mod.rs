//! Templates (W8c): a header, a template per row and a footer, rendered a part at a time so a
//! template streams like every other format. What a template is given is DbRover's own
//! [`TemplateContext`]; the engine that renders it is behind [`TemplateEngine`], and MiniJinja,
//! its one implementation, is in [`jinja`]. See `docs/exports.md`.

pub(crate) mod jinja;

use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use dbrover_core::{Cell, ValueKind};
use dbrover_sql::Dialect;

use crate::ExportColumn;
use crate::column::unique_keys;
use crate::text::TextFormat;

pub use jinja::check;

/// What a template is written as: its three parts and how values are escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSource {
    pub header: String,
    pub row: String,
    pub footer: String,
    pub escape: Escape,
}

/// How values are written into a template's output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Escape {
    /// As they are.
    #[default]
    None,
    /// HTML-escaped, unless a template marks one `safe`.
    Html,
}

/// One of a template's three parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Header,
    Row,
    Footer,
}

impl Part {
    pub fn name(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Row => "row",
            Self::Footer => "footer",
        }
    }
}

/// How much one render of a part may do. A render that needs more stops with an error rather
/// than holding DbRover up or filling the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateLimits {
    /// Steps of the template's code: about one an expression or a loop's turn.
    pub fuel: u64,
    /// Bytes one render may write.
    pub max_bytes: usize,
}

impl Default for TemplateLimits {
    /// Chosen rather than measured, far above what a row's template needs: a hundred columns
    /// through a loop with a filter each take a few thousand steps and a few kilobytes. A
    /// million steps stop a loop that never ends in well under a second, and 16 MB is more
    /// than any row's values hold yet stops a template repeating a string without end.
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// What every part of a template sees about the export, besides the columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportContext {
    /// The data source's name.
    pub data_source: String,
    /// The table the rows came from, as its name is written (`main.orders`), when there is one.
    pub table: Option<String>,
    /// The run's `:name` parameters and their values, in the order they were asked for.
    pub parameters: Vec<(String, Option<String>)>,
    /// When the export started, as an ISO 8601 timestamp.
    pub exported_at: String,
}

/// What one render of a part is given: the context every part sees, and the part's own.
pub struct TemplateContext<'a> {
    pub columns: &'a [ExportColumn],
    /// Each column's name as `record` keys it: a repeated name suffixed `_2`, `_3`, …
    pub keys: &'a [String],
    pub export: &'a ExportContext,
    pub part: PartContext<'a>,
}

/// What only one part sees.
pub enum PartContext<'a> {
    Header,
    Row {
        values: &'a [Cell],
        /// Counting from 1.
        index: usize,
    },
    Footer {
        count: usize,
    },
}

impl PartContext<'_> {
    pub fn part(&self) -> Part {
        match self {
            Self::Header => Part::Header,
            Self::Row { .. } => Part::Row,
            Self::Footer { .. } => Part::Footer,
        }
    }
}

/// Renders a template's parts. Made from a [`TemplateSource`] by an implementation, which has
/// already checked it; MiniJinja's is `jinja::compile`. Everything else in DbRover sees only
/// this, so another template language would be a second implementation.
pub trait TemplateEngine: Send {
    /// Renders `context`'s part into `out`.
    ///
    /// # Errors
    ///
    /// What the template got wrong while rendering, or what `out` reports.
    fn render(
        &self,
        context: &TemplateContext<'_>,
        out: &mut dyn Write,
    ) -> Result<(), TemplateError>;
}

/// Why a template couldn't be read or rendered, as a sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

/// A template with what it writes about: the export's context, and the dialect the SQL filters
/// quote for. What an export of a template format carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSpec {
    pub source: Arc<TemplateSource>,
    pub context: ExportContext,
    /// `None` for rows of a language without a SQL dialect.
    pub dialect: Option<Dialect>,
}

/// A [`TemplateSpec`] as a [`TextFormat`]: each part rendered as its turn comes.
pub(crate) struct TemplateFormat {
    context: ExportContext,
    /// The compiled parts, or why they wouldn't compile, reported when the header is due.
    engine: Result<Box<dyn TemplateEngine>, TemplateError>,
    columns: Vec<ExportColumn>,
    keys: Vec<String>,
    rows: usize,
}

impl TemplateFormat {
    pub(crate) fn new(spec: &TemplateSpec) -> Self {
        Self {
            context: spec.context.clone(),
            engine: jinja::compile(&spec.source, spec.dialect, TemplateLimits::default()),
            columns: Vec::new(),
            keys: Vec::new(),
            rows: 0,
        }
    }

    fn render(&self, out: &mut dyn Write, part: PartContext<'_>) -> io::Result<()> {
        let engine = self
            .engine
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        let context = TemplateContext {
            columns: &self.columns,
            keys: &self.keys,
            export: &self.context,
            part,
        };
        engine.render(&context, out).map_err(io::Error::other)
    }
}

impl TextFormat for TemplateFormat {
    fn header(&mut self, out: &mut dyn Write, columns: &[ExportColumn]) -> io::Result<()> {
        self.columns = columns.to_vec();
        self.keys = unique_keys(columns);
        self.render(out, PartContext::Header)
    }

    fn row(&mut self, out: &mut dyn Write, row: &[Cell]) -> io::Result<()> {
        self.rows += 1;
        let index = self.rows;
        self.render(out, PartContext::Row { values: row, index })
            .map_err(|error| io::Error::other(format!("row {index}: {error}")))
    }

    fn footer(&mut self, out: &mut dyn Write) -> io::Result<()> {
        let count = self.rows;
        self.render(out, PartContext::Footer { count })
    }
}

/// A column's kind as templates name it: `{{ column.kind }}`.
pub fn kind_name(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Integer => "integer",
        ValueKind::Decimal => "decimal",
        ValueKind::Float => "float",
        ValueKind::Boolean => "boolean",
        ValueKind::Date => "date",
        ValueKind::Timestamp => "timestamp",
        ValueKind::TimestampWithZone => "timestamp_with_zone",
        ValueKind::Text => "text",
        ValueKind::Undeclared => "undeclared",
    }
}
