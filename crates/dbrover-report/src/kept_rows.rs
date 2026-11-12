//! A partitioned run's result rows, kept a range at a time in the run's work folder
//! (`docs/reports-at-scale.md`, RS5b), so the outputs every range appends to, the charts of the
//! result and the preview are written once the last range is done, from every range's rows in
//! order: by this run, or by a later run that resumes it and reads the earlier ranges' rows
//! from here instead of from their sources.
//!
//! A range's part is a file of JSON Lines: the first line the result's columns, then a line a
//! page, each an array of rows of text values. JSON keeps `NULL` and the empty string apart,
//! which CSV wouldn't, so the rows written from a part are those the range's feed gave.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use dbrover_core::Row;
use dbrover_export::ExportColumn;
use serde::{Deserialize, Serialize};

use crate::PREVIEW_ROWS;

/// The folder in the run's work folder that holds what each range keeps for the run's end:
/// its rows, and its part of each Parquet file every range appends to.
pub(crate) const PARTS_FOLDER: &str = "parts";

/// What a part's first line holds.
#[derive(Serialize, Deserialize)]
struct Header {
    columns: Vec<ExportColumn>,
}

/// Each range's rows, as the run goes.
pub(crate) struct KeptRows {
    folder: PathBuf,
    /// Whether anything but the preview reads the rows; if not, a range keeps only the rows
    /// the preview may still show, and one more to say there are more.
    every_row: bool,
    /// The rows kept so far, every range's.
    kept: u64,
    /// The finished ranges' parts, in the ranges' order.
    parts: Vec<PathBuf>,
    /// The range's part being written.
    writing: Option<(PathBuf, BufWriter<File>)>,
}

impl KeptRows {
    /// Rows kept in `work`'s parts folder; `every_row` as [`KeptRows::every_row`].
    pub(crate) fn new(work: &Path, every_row: bool) -> Self {
        Self {
            folder: work.join(PARTS_FOLDER),
            every_row,
            kept: 0,
            parts: Vec::new(),
            writing: None,
        }
    }

    /// Whether a range's part is being written.
    pub(crate) fn is_writing(&self) -> bool {
        self.writing.is_some()
    }

    /// Starts range `number`'s part, of rows with `columns`.
    ///
    /// # Errors
    ///
    /// When the file can't be made or written.
    pub(crate) fn start(&mut self, number: u64, columns: &[ExportColumn]) -> io::Result<()> {
        std::fs::create_dir_all(&self.folder)?;
        let path = self.folder.join(part_name(number));
        let mut file = BufWriter::new(File::create(&path)?);
        let header = Header {
            columns: columns.to_vec(),
        };
        serde_json::to_writer(&mut file, &header)?;
        file.write_all(b"\n")?;
        self.writing = Some((path, file));
        Ok(())
    }

    /// Keeps `page`, or as much of it as the run's end needs.
    ///
    /// # Errors
    ///
    /// When no part is being written, or it can't be written.
    pub(crate) fn write(&mut self, page: &[Row]) -> io::Result<()> {
        let room = if self.every_row {
            page.len()
        } else {
            usize::try_from((PREVIEW_ROWS as u64 + 1).saturating_sub(self.kept))
                .unwrap_or(usize::MAX)
                .min(page.len())
        };
        if room == 0 {
            return Ok(());
        }
        let Some((_, file)) = &mut self.writing else {
            return Err(io::Error::other("no range's rows are being kept"));
        };
        serde_json::to_writer(&mut *file, &page[..room])?;
        file.write_all(b"\n")?;
        self.kept += room as u64;
        Ok(())
    }

    /// Ends the range's part, written to the disk, so a run that resumes can read it.
    ///
    /// # Errors
    ///
    /// When it can't be written.
    pub(crate) fn end(&mut self) -> io::Result<()> {
        let Some((path, file)) = self.writing.take() else {
            return Ok(());
        };
        let file = file.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        self.parts.push(path);
        Ok(())
    }

    /// The finished ranges' parts, in the ranges' order.
    pub(crate) fn parts(&self) -> &[PathBuf] {
        &self.parts
    }
}

/// The name of range `number`'s part.
fn part_name(number: u64) -> String {
    format!("rows-{number:05}.jsonl")
}

/// A range's kept rows, read back a page at a time.
pub(crate) struct PartReader {
    path: PathBuf,
    lines: io::Lines<BufReader<File>>,
    columns: Vec<ExportColumn>,
}

impl PartReader {
    /// Opens the part at `path` and reads its columns.
    ///
    /// # Errors
    ///
    /// When it can't be read, or isn't a part.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let mut lines = BufReader::new(File::open(path)?).lines();
        let header = lines
            .next()
            .ok_or_else(|| io::Error::other(format!("{} is empty", path.display())))??;
        let header: Header = serde_json::from_str(&header).map_err(io::Error::other)?;
        Ok(Self {
            path: path.to_owned(),
            lines,
            columns: header.columns,
        })
    }

    pub(crate) fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    /// The next page, or `None` after the last.
    ///
    /// # Errors
    ///
    /// When the file can't be read, or a line isn't a page.
    pub(crate) fn next_page(&mut self) -> io::Result<Option<Vec<Row>>> {
        let Some(line) = self.lines.next() else {
            return Ok(None);
        };
        serde_json::from_str(&line?)
            .map(Some)
            .map_err(|error| io::Error::other(format!("{}: {error}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::ValueKind;

    use super::*;

    fn columns() -> Vec<ExportColumn> {
        vec![ExportColumn {
            name: "note".to_owned(),
            type_name: Some("TEXT".to_owned()),
            kind: ValueKind::Text,
            excel_format: None,
        }]
    }

    fn rows(values: &[Option<&str>]) -> Vec<Row> {
        values
            .iter()
            .map(|value| vec![value.map(str::to_owned)])
            .collect()
    }

    fn read_all(path: &Path) -> (Vec<ExportColumn>, Vec<Row>) {
        let mut reader = PartReader::open(path).unwrap();
        let mut read = Vec::new();
        while let Some(page) = reader.next_page().unwrap() {
            read.extend(page);
        }
        (reader.columns().to_vec(), read)
    }

    #[test]
    fn keeps_each_ranges_rows_with_null_apart_from_empty_text() {
        let work = tempfile::tempdir().unwrap();
        let mut kept = KeptRows::new(work.path(), true);
        let first = rows(&[Some("a"), None, Some("")]);
        let second = rows(&[Some("line\nbreak \"quoted\"")]);
        kept.start(1, &columns()).unwrap();
        kept.write(&first).unwrap();
        kept.write(&second).unwrap();
        kept.end().unwrap();
        kept.start(2, &columns()).unwrap();
        kept.end().unwrap();

        let parts = kept.parts().to_vec();
        assert_eq!(
            parts,
            [
                work.path().join("parts/rows-00001.jsonl"),
                work.path().join("parts/rows-00002.jsonl")
            ]
        );
        let (read_columns, read) = read_all(&parts[0]);
        assert_eq!(read_columns, columns());
        assert_eq!(read, [first, second].concat());
        assert_eq!(read_all(&parts[1]).1, Vec::<Row>::new());
    }

    #[test]
    fn keeps_only_what_the_preview_needs_when_nothing_else_reads_the_rows() {
        let work = tempfile::tempdir().unwrap();
        let mut kept = KeptRows::new(work.path(), false);
        let page: Vec<Row> = (0..150).map(|n| vec![Some(n.to_string())]).collect();
        for number in 1..=3 {
            kept.start(number, &columns()).unwrap();
            kept.write(&page).unwrap();
            kept.end().unwrap();
        }
        let counts: Vec<usize> = kept
            .parts()
            .iter()
            .map(|part| read_all(part).1.len())
            .collect();
        assert_eq!(counts, [150, PREVIEW_ROWS + 1 - 150, 0]);
    }

    #[test]
    fn says_a_part_that_is_not_one() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("rows-00001.jsonl");
        std::fs::write(&path, "").unwrap();
        assert!(PartReader::open(&path).is_err());
        std::fs::write(&path, "{\"columns\":[]}\nnot json\n").unwrap();
        let mut reader = PartReader::open(&path).unwrap();
        assert!(reader.next_page().is_err());
    }
}
