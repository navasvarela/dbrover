//! Excel workbooks (`.xlsx`), written with `rust_xlsxwriter` in its constant-memory mode: each
//! sheet's rows go to a temporary file as they are written, so an export of any size holds
//! about one row in memory.

mod typing;

use std::io;
use std::path::{Path, PathBuf};

use dbrover_core::Cell;
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook, Worksheet, XlsxError};
use serde::{Deserialize, Serialize};

use self::typing::{CellValue, Date, cell_value};
use crate::ExportColumn;
use crate::writer::FileFormat;

/// The most rows an Excel sheet holds, its header included.
pub const EXCEL_MAX_ROWS: u32 = 1_048_576;

/// The most characters an Excel cell holds. Longer values are cut there.
const EXCEL_MAX_TEXT: usize = 32_767;

/// Column widths, in characters, guessed from the header and the first page of rows: wide
/// enough for most values, never so wide that one long value hides the other columns.
const MIN_WIDTH: usize = 8;
const MAX_WIDTH: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct XlsxOptions {
    /// Whether the first row names the columns, in bold.
    pub header: bool,
    pub sheet_name: SheetName,
    /// Whether the header row stays in view while scrolling. Needs the header.
    pub freeze_header: bool,
    /// Whether the header row has Excel's filter buttons. Needs the header.
    pub autofilter: bool,
}

impl Default for XlsxOptions {
    fn default() -> Self {
        Self {
            header: true,
            sheet_name: SheetName::default(),
            freeze_header: true,
            autofilter: true,
        }
    }
}

/// A name Excel takes for a sheet: 1 to 31 characters, none of `[ ] : * ? / \`, and not
/// starting or ending with an apostrophe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SheetName(String);

impl SheetName {
    const MAX_CHARS: usize = 31;
    const FORBIDDEN: [char; 7] = ['[', ']', ':', '*', '?', '/', '\\'];

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of the `number`th sheet: this name for the first, then `Rows 2`, `Rows 3`, …,
    /// shortened to fit.
    pub(crate) fn numbered(&self, number: usize) -> String {
        if number == 1 {
            return self.0.clone();
        }
        let suffix = format!(" {number}");
        let kept = Self::MAX_CHARS.saturating_sub(suffix.chars().count());
        let stem: String = self.0.chars().take(kept).collect();
        format!("{}{suffix}", stem.trim_end_matches('\''))
    }
}

impl Default for SheetName {
    fn default() -> Self {
        Self("Rows".to_owned())
    }
}

impl From<SheetName> for String {
    fn from(name: SheetName) -> Self {
        name.0
    }
}

impl TryFrom<String> for SheetName {
    type Error = InvalidSheetName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let length = name.chars().count();
        if !(1..=Self::MAX_CHARS).contains(&length) {
            return Err(InvalidSheetName::Length);
        }
        if name.contains(Self::FORBIDDEN) {
            return Err(InvalidSheetName::Character);
        }
        if name.starts_with('\'') || name.ends_with('\'') {
            return Err(InvalidSheetName::Apostrophe);
        }
        Ok(Self(name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidSheetName {
    #[error("A sheet name has 1 to 31 characters")]
    Length,
    #[error("A sheet name can't have [ ] : * ? / or \\ in it")]
    Character,
    #[error("A sheet name can't start or end with an apostrophe")]
    Apostrophe,
}

/// How many sheets `rows` rows fill: Excel holds [`EXCEL_MAX_ROWS`] a sheet, a header row
/// included on each one.
pub(crate) fn xlsx_sheets(rows: usize, header: bool) -> usize {
    let per_sheet = rows_per_sheet(EXCEL_MAX_ROWS, header);
    rows.div_ceil(per_sheet).max(1)
}

fn rows_per_sheet(max_rows: u32, header: bool) -> usize {
    let rows = usize::try_from(max_rows).unwrap_or(usize::MAX);
    rows - usize::from(header)
}

/// The number formats dates are shown in: ISO 8601, as databases print them.
struct Formats {
    header: Format,
    date: Format,
    date_time: Format,
    date_time_millis: Format,
}

impl Formats {
    fn new() -> Self {
        Self {
            header: Format::new().set_bold(),
            date: Format::new().set_num_format("yyyy-mm-dd"),
            date_time: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
            date_time_millis: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss.000"),
        }
    }
}

pub(crate) struct XlsxFile {
    workbook: Workbook,
    path: PathBuf,
    options: XlsxOptions,
    columns: Vec<ExportColumn>,
    formats: Formats,
    /// Each column's own number format, when it declares one.
    column_formats: Vec<Option<Format>>,
    /// How many sheets there are; rows go to the last.
    sheets: usize,
    /// The next row to write in the last sheet, counted from 0.
    row: u32,
    /// The most rows a sheet takes, its header included.
    max_rows: u32,
    /// Each column's width, once the first page of rows has been seen.
    widths: Option<Vec<f64>>,
}

impl XlsxFile {
    /// Starts a workbook that will be saved at `path`.
    pub(crate) fn create(
        path: &Path,
        options: XlsxOptions,
        columns: &[ExportColumn],
    ) -> io::Result<Self> {
        Self::with_max_rows(path, options, columns, EXCEL_MAX_ROWS)
    }

    /// As [`XlsxFile::create`], with sheets of at most `max_rows` rows, so tests can fill one.
    pub(crate) fn with_max_rows(
        path: &Path,
        options: XlsxOptions,
        columns: &[ExportColumn],
        max_rows: u32,
    ) -> io::Result<Self> {
        let mut file = Self {
            workbook: Workbook::new(),
            path: path.to_owned(),
            options,
            columns: columns.to_vec(),
            formats: Formats::new(),
            column_formats: columns
                .iter()
                .map(|column| {
                    let format = column.excel_format.as_deref()?;
                    Some(Format::new().set_num_format(format))
                })
                .collect(),
            sheets: 0,
            row: 0,
            max_rows,
            widths: None,
        };
        file.add_sheet().map_err(io::Error::other)?;
        Ok(file)
    }

    /// Adds a sheet, with the header row if there is one.
    fn add_sheet(&mut self) -> Result<(), XlsxError> {
        self.sheets += 1;
        let name = self.options.sheet_name.numbered(self.sheets);
        let header = self.options.header;
        let freeze = header && self.options.freeze_header;
        let sheet = self.workbook.add_worksheet_with_constant_memory()?;
        sheet.set_name(name)?;
        if freeze {
            sheet.set_freeze_panes(1, 0)?;
        }
        if let Some(widths) = &self.widths {
            set_widths(sheet, widths)?;
        }
        self.row = 0;
        if header {
            for (column, info) in (0..).zip(&self.columns) {
                sheet.write_string_with_format(0, column, &info.name, &self.formats.header)?;
            }
            self.row = 1;
        }
        Ok(())
    }

    fn sheet(&mut self) -> Result<&mut Worksheet, XlsxError> {
        self.workbook.worksheet_from_index(self.sheets - 1)
    }

    /// Ends the last sheet: its filter buttons, over the rows it holds.
    fn end_sheet(&mut self) -> Result<(), XlsxError> {
        let columns = u16::try_from(self.columns.len()).unwrap_or(u16::MAX);
        if !(self.options.header && self.options.autofilter) || columns == 0 {
            return Ok(());
        }
        let last_row = self.row.saturating_sub(1);
        self.sheet()?.autofilter(0, 0, last_row, columns - 1)?;
        Ok(())
    }

    fn write_row(&mut self, row: &[Cell]) -> Result<(), XlsxError> {
        if self.row >= self.max_rows {
            self.end_sheet()?;
            self.add_sheet()?;
        }
        let at = self.row;
        let sheet = self.workbook.worksheet_from_index(self.sheets - 1)?;
        let columns = self.columns.iter().zip(&self.column_formats);
        for ((column, cell), (info, own_format)) in (0..).zip(row).zip(columns) {
            let Some(text) = cell else {
                continue;
            };
            let formats = CellFormats {
                defaults: &self.formats,
                column: own_format.as_ref(),
            };
            write_cell(sheet, &formats, (at, column), cell_value(info.kind, text))?;
        }
        self.row += 1;
        Ok(())
    }

    /// Guesses each column's width from the header and `rows`, and sets it on the sheet.
    fn set_widths(&mut self, rows: &[Vec<Cell>]) -> Result<(), XlsxError> {
        let widths: Vec<f64> = (0..self.columns.len())
            .map(|column| {
                let header = self.columns[column].name.chars().count();
                let longest = rows
                    .iter()
                    .filter_map(|row| row.get(column)?.as_deref())
                    .map(|value| value.chars().count())
                    .fold(header, usize::max);
                let width = u32::try_from(longest.clamp(MIN_WIDTH, MAX_WIDTH)).unwrap_or(0);
                f64::from(width) + 2.0
            })
            .collect();
        set_widths(self.sheet()?, &widths)?;
        self.widths = Some(widths);
        Ok(())
    }
}

fn set_widths(sheet: &mut Worksheet, widths: &[f64]) -> Result<(), XlsxError> {
    for (column, width) in (0..).zip(widths) {
        sheet.set_column_width(column, *width)?;
    }
    Ok(())
}

/// The formats a cell can be written in: its column's own, else DbRover's for its kind.
struct CellFormats<'a> {
    defaults: &'a Formats,
    column: Option<&'a Format>,
}

fn write_cell(
    sheet: &mut Worksheet,
    formats: &CellFormats<'_>,
    (row, column): (u32, u16),
    value: CellValue<'_>,
) -> Result<(), XlsxError> {
    if let (Some(format), CellValue::Number(number)) = (formats.column, &value) {
        sheet.write_number_with_format(row, column, *number, format)?;
        return Ok(());
    }
    match value {
        CellValue::Number(number) => sheet.write_number(row, column, number)?,
        CellValue::Boolean(boolean) => sheet.write_boolean(row, column, boolean)?,
        CellValue::Date(date) => {
            let format = formats.column.unwrap_or(&formats.defaults.date);
            sheet.write_datetime_with_format(row, column, excel_date(date)?, format)?
        }
        CellValue::DateTime(date, time) => {
            let default = if time.fraction {
                &formats.defaults.date_time_millis
            } else {
                &formats.defaults.date_time
            };
            let format = formats.column.unwrap_or(default);
            let value = excel_date(date)?.and_hms_milli(
                time.hour,
                time.minute,
                time.second,
                time.millisecond,
            )?;
            sheet.write_datetime_with_format(row, column, value, format)?
        }
        CellValue::Text(text) => sheet.write_string(row, column, cut(text))?,
    };
    Ok(())
}

fn excel_date(date: Date) -> Result<ExcelDateTime, XlsxError> {
    ExcelDateTime::from_ymd(date.year, date.month, date.day)
}

/// `text`, cut to what an Excel cell holds.
fn cut(text: &str) -> &str {
    match text.char_indices().nth(EXCEL_MAX_TEXT) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

impl FileFormat for XlsxFile {
    fn write(&mut self, rows: &[Vec<Cell>]) -> io::Result<()> {
        if self.widths.is_none() {
            self.set_widths(rows).map_err(io::Error::other)?;
        }
        rows.iter()
            .try_for_each(|row| self.write_row(row))
            .map_err(io::Error::other)
    }

    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.end_sheet().map_err(io::Error::other)?;
        let path = self.path.clone();
        self.workbook.save(&path).map_err(io::Error::other)
    }
}
