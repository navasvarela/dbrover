//! A run: the source's rows, a page at a time, into every output and the preview, under the
//! source's and the outputs' limits (`docs/reporting.md`, "Guardrails in RP1").
//!
//! 1. Map each output's schema onto the source's columns, and start its file under a partial
//!    name in the run's staging folder.
//! 2. For each page: count it against the source's caps, keep the preview's rows, convert it
//!    for each output and count that against the output's caps, then write it.
//! 3. Once the source has no more rows, complete every file, and only then put them all in
//!    place, so a run that fails anywhere leaves no file behind and replaces none. The staging
//!    folder goes with the run, and whoever started a run that was killed removes it.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use dbrover_core::Row;
use dbrover_export::{
    CompletedFile, EXCEL_MAX_ROWS, ExportColumn, ExportWriter, OutputSchema, RowConverter,
};

use crate::{
    LimitHit, Plan, PlannedOutput, Preview, RunEvent, RunFailure, RunValues, Step, WrittenFile,
};

/// How many rows the preview keeps: a screenful or two, enough to see a report is right.
pub const PREVIEW_ROWS: usize = 200;

/// How often a [`RunEvent::Progress`] is sent while the source returns rows.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);

/// A source's rows, read a page at a time.
pub trait SourceReader {
    /// The result's columns.
    fn columns(&self) -> &[ExportColumn];

    /// The next page of rows, or `None` once every row has been read.
    fn next_page(&mut self) -> impl Future<Output = Result<Option<Vec<Row>>, SourceError>>;
}

/// Why a source stopped returning rows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SourceError {
    pub message: String,
    /// The guardrail it hit, such as its timeout.
    pub limit: Option<Box<LimitHit>>,
}

impl SourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            limit: None,
        }
    }
}

/// Where a run writes: its files' folder, and a staging folder of its own inside it, on the
/// same disk so a complete file is put in place by a rename.
#[derive(Debug, Clone, Copy)]
pub struct RunFolders<'a> {
    pub folder: &'a Path,
    pub staging: &'a Path,
}

/// What a finished run read and wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub rows: u64,
    pub bytes: u64,
    pub files: Vec<WrittenFile>,
}

/// Runs `plan` with `values`, reading `reader`, writing into `folders`, and sending each step
/// to `events` but the run's start and end, which the caller sends. `opened` is when the
/// source started to be opened, for the time to its first row.
///
/// # Errors
///
/// [`RunFailure`] naming the step that failed and, for a guardrail, which one.
pub async fn run<R: SourceReader>(
    plan: &Plan,
    values: &RunValues,
    reader: &mut R,
    folders: RunFolders<'_>,
    opened: Instant,
    events: &mut dyn FnMut(RunEvent),
) -> Result<RunSummary, RunFailure> {
    let _staging = Staging::make(folders)?;
    let mut outputs = start_outputs(plan, values, reader.columns(), folders)?;
    let source = Step::Source {
        id: plan.source.id.clone(),
    };
    let mut read = SourceCount::default();
    let mut preview = Preview {
        columns: reader
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        rows: Vec::new(),
        more: false,
    };
    let mut last_progress = Instant::now();
    let streamed = loop {
        let page = match reader.next_page().await {
            Ok(Some(page)) => page,
            Ok(None) => break Ok(()),
            Err(error) => {
                break Err(RunFailure {
                    step: source.clone(),
                    message: error.message,
                    limit: error.limit,
                });
            }
        };
        if page.is_empty() {
            continue;
        }
        read.first_row.get_or_insert_with(|| opened.elapsed());
        if let Err(failure) = read.add(&page, plan) {
            break Err(failure);
        }
        keep_for_preview(&mut preview, &page);
        if let Err(failure) = outputs
            .iter_mut()
            .try_for_each(|output| output.write(&page))
        {
            break Err(failure);
        }
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            events(RunEvent::Progress {
                rows: read.rows,
                bytes: read.bytes,
                elapsed_ms: millis(opened.elapsed()),
            });
        }
    };
    if let Err(failure) = streamed {
        outputs.into_iter().for_each(OutputRun::abandon);
        return Err(failure);
    }
    events(RunEvent::SourceDone {
        rows: read.rows,
        bytes: read.bytes,
        first_row_ms: read.first_row.map(millis),
        elapsed_ms: millis(opened.elapsed()),
    });
    events(RunEvent::Preview(preview));
    let files = put_in_place(complete_outputs(outputs)?)?;
    for file in &files {
        events(RunEvent::OutputWritten(file.clone()));
    }
    Ok(RunSummary {
        rows: read.rows,
        bytes: read.bytes,
        files,
    })
}

/// The staging folder, made for the run and removed when it ends, however it ends.
struct Staging<'a>(&'a Path);

impl<'a> Staging<'a> {
    fn make(folders: RunFolders<'a>) -> Result<Self, RunFailure> {
        std::fs::create_dir_all(folders.staging).map_err(|error| {
            RunFailure::new(
                Step::Worker,
                format!(
                    "could not make the folder {}: {error}",
                    folders.staging.display()
                ),
            )
        })?;
        Ok(Self(folders.staging))
    }
}

impl Drop for Staging<'_> {
    fn drop(&mut self) {
        // Empty by now unless the run failed; what can't be removed is left for the app's
        // side, which removes it again after the worker ends.
        let _ = std::fs::remove_dir_all(self.0);
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// What the source has returned so far.
#[derive(Default)]
struct SourceCount {
    rows: u64,
    bytes: u64,
    first_row: Option<Duration>,
}

impl SourceCount {
    /// Counts `page`, or says which cap it crossed.
    fn add(&mut self, page: &[Row], plan: &Plan) -> Result<(), RunFailure> {
        let source = &plan.source;
        let limits = source.limits;
        let step = Step::Source {
            id: source.id.clone(),
        };
        let raise_in = format!("the [[source]] {:?}", source.id);
        self.rows += page.len() as u64;
        self.bytes += page.iter().map(|row| row_bytes(row)).sum::<u64>();
        if self.rows > limits.max_rows {
            let value = format!("{} rows", grouped(limits.max_rows));
            return Err(RunFailure::at_limit(
                step,
                format!(
                    "the source returned more than {value}, its max_rows; filter in its query, \
                     or raise max_rows"
                ),
                LimitHit {
                    key: "max_rows".to_owned(),
                    value,
                    raise_in,
                },
            ));
        }
        if self.bytes > limits.max_bytes.0 {
            let value = limits.max_bytes.to_string();
            return Err(RunFailure::at_limit(
                step,
                format!(
                    "the source returned more than {value} of values, its max_bytes; read fewer \
                     columns or rows in its query, or raise max_bytes"
                ),
                LimitHit {
                    key: "max_bytes".to_owned(),
                    value,
                    raise_in,
                },
            ));
        }
        Ok(())
    }
}

/// The bytes of a row's values, as the text they arrive as.
fn row_bytes(row: &[Option<String>]) -> u64 {
    row.iter().flatten().map(|value| value.len() as u64).sum()
}

/// `1234567` as `1,234,567`, for messages about limits and counts of rows.
pub fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn keep_for_preview(preview: &mut Preview, page: &[Row]) {
    let room = PREVIEW_ROWS.saturating_sub(preview.rows.len());
    preview.rows.extend(page.iter().take(room).cloned());
    preview.more |= page.len() > room;
}

/// An output being written.
struct OutputRun<'a> {
    number: usize,
    file: String,
    output: &'a PlannedOutput,
    converter: RowConverter,
    writer: ExportWriter,
    rows: u64,
    bytes: u64,
}

fn start_outputs<'a>(
    plan: &'a Plan,
    values: &RunValues,
    columns: &[ExportColumn],
    folders: RunFolders<'_>,
) -> Result<Vec<OutputRun<'a>>, RunFailure> {
    let mut started: Vec<OutputRun<'a>> = Vec::new();
    for (index, (output, file)) in plan.outputs.iter().zip(&values.files).enumerate() {
        let step = Step::Output {
            number: index + 1,
            file: file.clone(),
        };
        let schema = match &output.columns {
            Some(declared) => OutputSchema::new(declared, columns),
            None => Ok(OutputSchema::all(columns)),
        };
        let writer = schema
            .map_err(|missing| missing.to_string())
            .and_then(|schema| {
                let export_columns = schema.export_columns();
                let converter = RowConverter::new(schema, output.format.target());
                ExportWriter::create_staged(
                    &folders.folder.join(file),
                    folders.staging,
                    &output.format.spec(),
                    &export_columns,
                )
                .map(|writer| (converter, writer))
                .map_err(|error| describe(&error))
            });
        match writer {
            Ok((converter, writer)) => started.push(OutputRun {
                number: index + 1,
                file: file.clone(),
                output,
                converter,
                writer,
                rows: 0,
                bytes: 0,
            }),
            Err(message) => {
                started.into_iter().for_each(OutputRun::abandon);
                return Err(RunFailure::new(step, message));
            }
        }
    }
    Ok(started)
}

impl OutputRun<'_> {
    fn step(&self) -> Step {
        Step::Output {
            number: self.number,
            file: self.file.clone(),
        }
    }

    fn raise_in(&self) -> String {
        format!("output {}", self.number)
    }

    fn write(&mut self, page: &[Row]) -> Result<(), RunFailure> {
        let mut converted = Vec::with_capacity(page.len());
        for row in page {
            let row = self
                .converter
                .convert(row)
                .map_err(|invalid| RunFailure::new(self.step(), invalid.to_string()))?;
            self.bytes += row_bytes(&row);
            converted.push(row);
        }
        self.rows += converted.len() as u64;
        self.check_limits()?;
        self.writer
            .write(&converted)
            .map_err(|error| RunFailure::new(self.step(), describe(&error)))
    }

    fn check_limits(&self) -> Result<(), RunFailure> {
        let max_size = self.output.limits.max_size;
        if self.bytes > max_size.0 {
            let value = max_size.to_string();
            return Err(RunFailure::at_limit(
                self.step(),
                format!(
                    "the output would write more than {value} of values, its max_size; write \
                     fewer rows or columns, or raise max_size"
                ),
                LimitHit {
                    key: "max_size".to_owned(),
                    value,
                    raise_in: self.raise_in(),
                },
            ));
        }
        if let Some(per_sheet) = self.rows_per_sheet()
            && self.rows > per_sheet
        {
            return Err(RunFailure::at_limit(
                self.step(),
                format!(
                    "the output has more than the {} rows an Excel sheet holds; set \
                     continue_on_new_sheet = true to go on in a new sheet",
                    grouped(per_sheet)
                ),
                LimitHit {
                    key: "continue_on_new_sheet".to_owned(),
                    value: format!("{} rows a sheet", grouped(per_sheet)),
                    raise_in: self.raise_in(),
                },
            ));
        }
        Ok(())
    }

    /// The rows one sheet takes, when the output fails past it rather than going on.
    fn rows_per_sheet(&self) -> Option<u64> {
        match &self.output.format {
            crate::OutputFormat::Xlsx(options) if !self.output.continue_on_new_sheet => {
                Some(u64::from(EXCEL_MAX_ROWS) - u64::from(options.header))
            }
            _ => None,
        }
    }

    fn abandon(self) {
        self.writer.abandon();
    }
}

/// An output whose file is complete, still under its partial name.
struct Completed {
    step: Step,
    rows: u64,
    warnings: Vec<String>,
    file: CompletedFile,
}

fn complete_outputs(outputs: Vec<OutputRun<'_>>) -> Result<Vec<Completed>, RunFailure> {
    let mut completed: Vec<Completed> = Vec::new();
    let mut outputs = outputs.into_iter();
    while let Some(output) = outputs.next() {
        let step = output.step();
        let warnings = output.converter.warnings();
        match output.writer.complete() {
            Ok(file) => completed.push(Completed {
                step,
                rows: output.rows,
                warnings,
                file,
            }),
            Err(error) => {
                completed.into_iter().for_each(|done| done.file.abandon());
                outputs.for_each(OutputRun::abandon);
                return Err(RunFailure::new(step, describe(&error)));
            }
        }
    }
    Ok(completed)
}

/// Renames every completed file to its name. Done last, and in one go, so a failure before it
/// replaces no file the folder had.
fn put_in_place(completed: Vec<Completed>) -> Result<Vec<WrittenFile>, RunFailure> {
    let mut written = Vec::new();
    let mut completed = completed.into_iter();
    while let Some(done) = completed.next() {
        let size = done.file.size().unwrap_or_default();
        let path = done.file.path().to_owned();
        if let Err(error) = done.file.put_in_place() {
            completed.for_each(|rest| rest.file.abandon());
            return Err(RunFailure::new(done.step, describe(&error)));
        }
        written.push(WrittenFile {
            path,
            rows: done.rows,
            size,
            warnings: done.warnings,
        });
    }
    Ok(written)
}

/// An error and its causes, as one sentence.
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    dbrover_core::describe_error(error)
}
