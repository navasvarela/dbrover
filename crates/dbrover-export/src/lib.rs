//! Writing rows out, in every format DbRover exports: to a file a page at a time, or all at
//! once to text for the clipboard, a program's stdin or an MCP answer. The design is in
//! `docs/exports.md`.
//!
//! A format is an [`ExportSpec`]: an [`ExportFormat`] with its options. Text formats implement
//! [`TextFormat`], which writes a header, each row and a footer to any writer, so a file and
//! the clipboard get the same bytes.

mod column;
mod custom;
mod options;
mod parquet;
mod program;
mod spec;
mod template;
mod text;
mod writer;
mod xlsx;

pub use column::ExportColumn;
pub use custom::{CustomFormat, CustomFormatId, CustomFormatKind, Preset};
pub use options::ExportOptions;
pub use parquet::{ParquetCompression, ParquetOptions};
pub use program::{ExportPipe, ExportProgram, ProgramFormat, ProgramSpec};
pub use spec::{CSV_FOR_EXCEL, ExportChoice, ExportFormat, ExportSpec, export_choices};
pub use template::{
    Escape, ExportContext, Part, TemplateContext, TemplateEngine, TemplateError, TemplateLimits,
    TemplateSource, TemplateSpec, check as check_template, kind_name,
};
pub use text::{
    CsvOptions, FieldSeparator, HtmlDocument, HtmlOptions, InsertOptions, InsertSpec,
    InvalidSeparator, LineEnding, MAX_ROWS_PER_INSERT, MarkdownOptions, Quoting, RowSource,
    RowsPerInsert, SourceTable, SqlTable, TextFormat, TsvOptions, UpdateSpec, render, try_render,
};
pub use writer::{ExportError, ExportWriter, FileFormat, export_rows};
pub use xlsx::{EXCEL_MAX_ROWS, InvalidSheetName, SheetName, XlsxOptions};

#[cfg(test)]
mod custom_tests;
#[cfg(test)]
mod sql_tests;
#[cfg(test)]
mod template_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod xlsx_tests;
