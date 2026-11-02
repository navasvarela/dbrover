//! Writing rows to a file a page at a time, put in place only once complete.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use dbrover_core::Cell;

use crate::text::TextFormat;
use crate::{ExportColumn, ExportSpec};

#[derive(Debug, thiserror::Error)]
#[error("could not write {}", path.display())]
pub struct ExportError {
    path: PathBuf,
    #[source]
    source: io::Error,
}

/// A format that writes a file of its own: started for a path and its columns, given rows a
/// page at a time, then finished into that path.
pub trait FileFormat: Send {
    /// Appends rows, each with a cell per column.
    ///
    /// # Errors
    ///
    /// The file couldn't be written.
    fn write(&mut self, rows: &[Vec<Cell>]) -> io::Result<()>;

    /// Ends the file.
    ///
    /// # Errors
    ///
    /// The file couldn't be written.
    fn finish(self: Box<Self>) -> io::Result<()>;
}

/// A text format writing to a file through a buffer.
pub(crate) struct TextFile {
    out: BufWriter<File>,
    format: Box<dyn TextFormat>,
}

impl TextFile {
    /// Creates the file at `path` and writes the format's header.
    pub(crate) fn create(
        path: &Path,
        mut format: Box<dyn TextFormat>,
        columns: &[ExportColumn],
    ) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        format.header(&mut out, columns)?;
        Ok(Self { out, format })
    }
}

impl FileFormat for TextFile {
    fn write(&mut self, rows: &[Vec<Cell>]) -> io::Result<()> {
        rows.iter()
            .try_for_each(|row| self.format.row(&mut self.out, row))
    }

    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.format.footer(&mut self.out)?;
        self.out.flush()
    }
}

/// Writes `rows` under a header of `columns` to `path`, replacing it.
///
/// # Errors
///
/// [`ExportError`] if the file can't be created or written.
pub fn export_rows(
    path: &Path,
    spec: &ExportSpec,
    columns: &[ExportColumn],
    rows: &[Vec<Cell>],
) -> Result<(), ExportError> {
    let mut writer = ExportWriter::create(path, spec, columns)?;
    writer.write(rows)?;
    writer.finish()
}

/// Writes rows to a file a page at a time, so an export never holds them all.
///
/// They go to a file beside `path` (its name plus `.partial`), renamed to `path` by
/// [`ExportWriter::finish`]: an export that fails or is abandoned never leaves half a file
/// under the name chosen, nor replaces a file that was there.
pub struct ExportWriter {
    format: Box<dyn FileFormat>,
    /// How many rows were written so far.
    rows: usize,
    path: PathBuf,
    partial: PathBuf,
}

impl ExportWriter {
    /// Starts the file in the format `spec` says.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the file can't be created or written.
    pub fn create(
        path: &Path,
        spec: &ExportSpec,
        columns: &[ExportColumn],
    ) -> Result<Self, ExportError> {
        Self::with_format(path, |partial| spec.file_format(partial, columns))
    }

    /// Starts the file in a format made by `start`, which is given the path to write to.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if `start` fails.
    pub fn with_format(
        path: &Path,
        start: impl FnOnce(&Path) -> io::Result<Box<dyn FileFormat>>,
    ) -> Result<Self, ExportError> {
        let mut partial_name = path.file_name().unwrap_or_default().to_owned();
        partial_name.push(".partial");
        let partial = path.with_file_name(partial_name);
        let format = start(&partial).map_err(|source| {
            // Starting failed; the partial file's removal, if there is one, can only add to
            // that.
            let _ = fs::remove_file(&partial);
            ExportError {
                path: path.to_owned(),
                source,
            }
        })?;
        Ok(Self {
            format,
            rows: 0,
            path: path.to_owned(),
            partial,
        })
    }

    /// Appends rows.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the file can't be written.
    pub fn write(&mut self, rows: &[Vec<Cell>]) -> Result<(), ExportError> {
        self.format
            .write(rows)
            .map_err(|source| self.error(source))?;
        self.rows += rows.len();
        Ok(())
    }

    /// How many rows have been written.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Ends the file and puts it in place.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the file can't be written or renamed.
    pub fn finish(self) -> Result<(), ExportError> {
        self.complete()?.put_in_place()
    }

    /// Ends the file, but leaves it under its partial name, so that several files can be
    /// completed before any is put in place: a report's outputs appear together or not at all.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the file can't be written; the partial file is removed.
    pub fn complete(self) -> Result<CompletedFile, ExportError> {
        let Self {
            format,
            path,
            partial,
            ..
        } = self;
        match format.finish() {
            Ok(()) => Ok(CompletedFile { path, partial }),
            Err(source) => {
                // Nothing more can be done about a partial file that won't go.
                let _ = fs::remove_file(&partial);
                Err(ExportError { path, source })
            }
        }
    }

    /// Stops writing and removes what was written.
    pub fn abandon(self) {
        let Self {
            format, partial, ..
        } = self;
        drop(format);
        // Nothing more can be done about a partial file that won't go; its name says what it is.
        let _ = fs::remove_file(partial);
    }

    /// The file it will be put in place as.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn error(&self, source: io::Error) -> ExportError {
        ExportError {
            path: self.path.clone(),
            source,
        }
    }
}

/// A file written whole, still under its partial name.
#[derive(Debug)]
pub struct CompletedFile {
    path: PathBuf,
    partial: PathBuf,
}

impl CompletedFile {
    /// The file it will be put in place as.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many bytes the file holds.
    ///
    /// # Errors
    ///
    /// If the file's size can't be read.
    pub fn size(&self) -> io::Result<u64> {
        fs::metadata(&self.partial).map(|metadata| metadata.len())
    }

    /// Renames the file to its name, replacing any file there.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the rename fails; the partial file is removed.
    pub fn put_in_place(self) -> Result<(), ExportError> {
        fs::rename(&self.partial, &self.path).map_err(|source| {
            // Nothing more can be done about a partial file that won't go.
            let _ = fs::remove_file(&self.partial);
            ExportError {
                path: self.path,
                source,
            }
        })
    }

    /// Removes the file.
    pub fn abandon(self) {
        // Nothing more can be done about a partial file that won't go; its name says what it is.
        let _ = fs::remove_file(self.partial);
    }
}
