//! Programs as export formats (W8d): the rows go to a program's stdin as JSON Lines, and what
//! it writes on stdout becomes the file. This crate writes the rows; the engine starts the
//! program, since it knows the data source and whether the program is trusted. See
//! `docs/exports.md`.

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dbrover_core::{Cell, CommandTemplate};

use crate::text::{JsonLines, SqlTable, TextFormat};
use crate::writer::FileFormat;
use crate::{CustomFormatId, ExportColumn};

/// What `exports.toml` says about a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramFormat {
    pub command: CommandTemplate,
    /// How long the program may go without taking more input, and how long it has to finish
    /// once its input has ended.
    pub timeout: Duration,
    /// Whether it writes text, which puts it under Copy as.
    pub text: bool,
}

/// A program to export rows through, as an export carries it.
#[derive(Clone)]
pub struct ProgramSpec {
    pub id: CustomFormatId,
    pub program: Arc<ProgramFormat>,
    /// The table the rows came from, for `{table}` and `{schema}`.
    pub table: Option<SqlTable>,
    /// The program filled in and ready to start: set by the engine once it has checked that
    /// the program is trusted; the app sends none.
    pub prepared: Option<Arc<dyn ExportProgram>>,
}

impl fmt::Debug for ProgramSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgramSpec")
            .field("id", &self.id)
            .field("program", &self.program)
            .field("table", &self.table)
            .field("prepared", &self.prepared.is_some())
            .finish()
    }
}

/// Two exports through the same program are the same, prepared or not.
impl PartialEq for ProgramSpec {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.program == other.program && self.table == other.table
    }
}

impl Eq for ProgramSpec {}

/// A program filled in for one export, which the engine starts when the file is.
pub trait ExportProgram: Send + Sync {
    /// Starts the program with its stdout going to a new file at `path`.
    ///
    /// # Errors
    ///
    /// The file couldn't be created, or the program couldn't start.
    fn start(&self, path: &Path) -> io::Result<Box<dyn ExportPipe>>;
}

/// A running program's stdin.
pub trait ExportPipe: Send {
    /// Sends the program more of its input.
    ///
    /// # Errors
    ///
    /// The program stopped taking input for longer than its timeout, or failed.
    fn send(&mut self, input: &[u8]) -> io::Result<()>;

    /// Ends the program's input and waits for it to finish writing the file.
    ///
    /// # Errors
    ///
    /// The program failed, with its stderr, or didn't finish in time.
    fn finish(self: Box<Self>) -> io::Result<()>;
}

/// The rows as JSON Lines into a program, as a [`FileFormat`]: each page encoded, then sent.
pub(crate) struct ProgramFile {
    pipe: Box<dyn ExportPipe>,
    lines: JsonLines,
}

impl ProgramFile {
    pub(crate) fn start(
        program: &dyn ExportProgram,
        path: &Path,
        columns: &[ExportColumn],
    ) -> io::Result<Self> {
        let mut lines = JsonLines::default();
        // JSON Lines writes nothing before its rows.
        lines.header(&mut io::sink(), columns)?;
        Ok(Self {
            pipe: program.start(path)?,
            lines,
        })
    }
}

impl FileFormat for ProgramFile {
    fn write(&mut self, rows: &[Vec<Cell>]) -> io::Result<()> {
        let mut input = Vec::new();
        for row in rows {
            self.lines.row(&mut input, row)?;
        }
        self.pipe.send(&input)
    }

    fn finish(self: Box<Self>) -> io::Result<()> {
        self.pipe.finish()
    }
}
