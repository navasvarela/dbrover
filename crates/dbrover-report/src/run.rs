//! A run: the rows of its one source, or of its combine step, a page at a time, into every
//! output and the preview, under the source's and the outputs' limits (`docs/reporting.md`,
//! "Guardrails in RP1").
//!
//! 1. Map each output's schema onto the source's columns, and start its file under a partial
//!    name in the run's staging folder; start each chart.
//! 2. For each page: count it against the source's caps, keep the preview's rows, convert it
//!    for each output and count that against the output's caps, then write it; give it to each
//!    chart, which keeps what it may draw.
//! 3. Once the source has no more rows, complete every file, draw every chart into the staging
//!    folder, and only then put them all in place, so a run that fails anywhere leaves no file
//!    behind and replaces none. The staging folder goes with the run, and whoever started a run
//!    that was killed removes it.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use dbrover_core::{ResultShape, Row};
use dbrover_export::{
    CompletedFile, EXCEL_MAX_ROWS, ExportColumn, ExportWriter, OutputSchema, RowConverter,
};

use dbrover_sql::ParameterValues;

use crate::chart::ChartRun;
use crate::declared::Shaped;
use crate::live::{ProgressClock, waiting};
use crate::parquet::PartsJoin;
use crate::statement::Statements;
use crate::{
    LimitHit, Logic, PartDone, Plan, PlannedOutput, Preview, QuerySource, RunEvent, RunFailure,
    RunValues, SourcePhases, SourceRanges, Step, WrittenFile,
};

/// How many rows the preview keeps: a screenful or two, enough to see a report is right.
pub const PREVIEW_ROWS: usize = 200;

/// How often a [`RunEvent::Progress`] is sent while a step returns rows.
pub(crate) const PROGRESS_EVERY: Duration = Duration::from_secs(1);

/// Opens a report's sources: the engine on real data sources, tests in memory.
pub trait SourceOpener {
    type Reader: SourceReader;

    /// Connects to the statement's data source and starts it.
    fn open(
        &self,
        statement: &SourceStatement<'_>,
    ) -> impl Future<Output = Result<Self::Reader, SourceError>>;
}

/// One statement a source sends: its whole query, or one batch of its lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatement<'a> {
    pub source: &'a QuerySource,
    /// The query as sent: a lookup's values written into it, and in a language without SQL,
    /// every parameter too.
    pub query: String,
    /// The values for the parameters left in `query`, which a SQL driver binds; empty for a
    /// language without SQL.
    pub parameters: ParameterValues,
    /// When the source's `timeout` runs out, counted from its first statement, or from its
    /// range's for a partitioned source.
    pub deadline: Instant,
    /// What the cost guard judges before the statement starts.
    pub cost: CostCheck,
}

/// What the cost guard of the statement's data source judges, where it has one.
#[derive(Debug, Clone, PartialEq)]
pub enum CostCheck {
    /// The statement itself.
    Statement,
    /// The whole period of a partitioned source, in place of its first range: what every
    /// range reads together, judged before any of them starts (RS4b).
    WholePeriod(Box<WholePeriod>),
    /// Nothing: the first range judged the whole period.
    Judged,
}

/// A partitioned source's statement over its whole period, for the cost guard.
#[derive(Debug, Clone, PartialEq)]
pub struct WholePeriod {
    /// As [`SourceStatement::query`].
    pub query: String,
    /// As [`SourceStatement::parameters`].
    pub parameters: ParameterValues,
}

/// How long opening a source's statement took, for its profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Opening {
    /// Connecting to its data source.
    pub connect: Duration,
    /// From connected to its statement running: a read-only transaction, the cost guard's
    /// estimate, and the engine's answer to the statement.
    pub start: Duration,
}

/// A source's rows, read a page at a time.
pub trait SourceReader {
    /// The result's columns.
    fn columns(&self) -> &[ExportColumn];

    /// How long opening it took; nothing for a reader that doesn't measure it.
    fn opening(&self) -> Opening {
        Opening::default()
    }

    /// Whether its rows are rows, or documents of one cell each (ADR 0006).
    fn shape(&self) -> ResultShape {
        ResultShape::Rows
    }

    /// The next page of rows, or `None` once every row has been read.
    fn next_page(&mut self) -> impl Future<Output = Result<Option<Vec<Row>>, SourceError>>;

    /// The bytes its engine scanned for the statement, as the engine reports them once every
    /// row has been read (`docs/reports-at-scale.md`, RS2f): what a warehouse bills by.
    /// `None`, the default, where the engine doesn't say.
    fn scanned(&self) -> Option<u64> {
        None
    }

    /// Ends the source's statement and its connection, however far it was read. What fails
    /// here can only be logged: the run has its rows, or has already failed.
    fn close(self) -> impl Future<Output = ()>;
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

    /// The run's failure, at `step`.
    pub fn at(self, step: Step) -> RunFailure {
        RunFailure {
            step,
            message: self.message,
            limit: self.limit,
        }
    }
}

/// Where a run writes: its files' folder, a staging folder of its own inside it, on the same
/// disk so a complete file is put in place by a rename, and a work folder of its own for its
/// DuckDB, wherever the app keeps them.
#[derive(Debug, Clone, Copy)]
pub struct RunFolders<'a> {
    pub folder: &'a Path,
    pub staging: &'a Path,
    pub work: &'a Path,
}

/// What a finished run read and wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub rows: u64,
    pub bytes: u64,
    pub files: Vec<WrittenFile>,
}

/// Runs `plan` with `values`, opening its sources with `opener`, writing into `folders`, and
/// sending each step to `events` but the run's start and end, which the caller sends.
///
/// A partitioned run (RS4d) runs once a range, each range's rows going to the same outputs:
/// a file per range for an output whose name says the range, one file for the others. Every
/// file is put in place once the last range is done, so a run that fails leaves none.
///
/// # Errors
///
/// [`RunFailure`] naming the step that failed and, for a guardrail, which one.
pub async fn run<O: SourceOpener>(
    plan: &Plan,
    values: &RunValues,
    opener: &O,
    folders: RunFolders<'_>,
    events: &mut dyn FnMut(RunEvent),
) -> Result<RunSummary, RunFailure> {
    let _staging = OwnFolder::make(folders.staging)?;
    let _work = OwnFolder::make(folders.work)?;
    let mut sink = Sink::new(plan, folders);
    let ran = match &values.run_ranges {
        None => run_once(plan, values, opener, &mut sink, events).await,
        Some(ranges) => run_ranges(plan, values, ranges, opener, &mut sink, events).await,
    };
    match ran {
        Ok(()) => sink.finish(events).await,
        Err(failure) => {
            sink.abandon();
            Err(failure)
        }
    }
}

/// Runs each of a partitioned run's `ranges` in turn into `sink`, and says what each wrote.
async fn run_ranges<'a, O: SourceOpener>(
    plan: &'a Plan,
    values: &RunValues,
    ranges: &SourceRanges,
    opener: &O,
    sink: &mut Sink<'a>,
    events: &mut dyn FnMut(RunEvent),
) -> Result<(), RunFailure> {
    let of = ranges.ranges.len() as u64;
    for range in &ranges.ranges {
        let range_values = plan
            .range_values(values, range)
            .map_err(|problem| RunFailure::new(Step::Report, problem))?;
        let started = Instant::now();
        let before = sink.written();
        run_once(plan, &range_values, opener, sink, events).await?;
        sink.end_range()?;
        let after = sink.written();
        events(RunEvent::PartDone(PartDone {
            step: plan.feed_step(),
            number: range.number,
            of,
            from: range.from.clone(),
            to: range.to.clone(),
            rows: after.rows - before.rows,
            bytes: after.bytes - before.bytes,
            first_row_ms: None,
            elapsed_ms: millis(started.elapsed()),
            scanned_bytes: None,
        }));
    }
    Ok(())
}

/// Runs `plan` once with `values`, its rows going to `sink`: the whole run, or one range of a
/// partitioned run.
async fn run_once<'a, O: SourceOpener>(
    plan: &'a Plan,
    values: &RunValues,
    opener: &O,
    sink: &mut Sink<'a>,
    events: &mut dyn FnMut(RunEvent),
) -> Result<(), RunFailure> {
    match &plan.logic {
        Logic::Direct(source) => {
            let opened = Instant::now();
            let step = || Step::Source {
                id: source.id.clone(),
            };
            let statement = Statements::new(plan, values, source).whole(opened);
            let mut clock = ProgressClock::new();
            let (reader, _) = waiting(Box::pin(Shaped::open(opener, &statement)), |so_far| {
                events(RunEvent::Progress {
                    step: step(),
                    rows: 0,
                    bytes: 0,
                    elapsed_ms: millis(opened.elapsed()),
                    batch: None,
                    parts: None,
                    busy: Some(clock.lap(so_far)),
                });
            })
            .await;
            let mut reader = reader.map_err(|error| error.at(step()))?;
            let opening = reader.opening();
            let streamed = sink
                .stream(&mut reader, &Feed::Source(source), values, opened, events)
                .await;
            let scanned = reader.scanned();
            reader.close().await;
            let read = streamed?;
            sink.count(&read);
            events(source_done(
                source,
                &read,
                opened.elapsed(),
                opening,
                scanned,
            ));
            Ok(())
        }
        Logic::Combine(combined) => {
            let combining = crate::combine::Combining {
                plan,
                values,
                folders: sink.folders(),
                combined,
                opener,
            };
            combining.run(sink, events).await
        }
    }
}

/// What the outputs are written from.
pub(crate) enum Feed<'a> {
    /// A source's rows, as it returns them, under its caps.
    Source(&'a QuerySource),
    /// The combine step's rows.
    Combine,
}

impl Feed<'_> {
    fn step(&self) -> Step {
        match self {
            Self::Source(source) => Step::Source {
                id: source.id.clone(),
            },
            Self::Combine => Step::Combine,
        }
    }

    /// Whether what has been read so far is within the feed's caps.
    fn check(&self, read: &Count) -> Result<(), RunFailure> {
        match self {
            Self::Source(source) => read.check(source),
            Self::Combine => Ok(()),
        }
    }
}

/// The event that says a source whose rows went straight to the outputs gave its last row.
/// The time its outputs took is theirs, so it loaded nothing.
fn source_done(
    source: &QuerySource,
    read: &Count,
    elapsed: Duration,
    opening: Opening,
    scanned: Option<u64>,
) -> RunEvent {
    RunEvent::SourceDone {
        id: source.id.clone(),
        rows: read.rows,
        bytes: read.bytes,
        first_row_ms: read.first_row.map(millis),
        elapsed_ms: millis(elapsed),
        table: Vec::new(),
        warnings: Vec::new(),
        batches: None,
        parts: None,
        phases: Some(SourcePhases {
            connect_ms: millis(opening.connect),
            server_ms: millis(opening.start + read.waited_first),
            transfer_ms: millis(read.waited_rest),
            load_ms: 0,
            type_ms: 0,
        }),
        scanned_bytes: scanned,
    }
}

/// Where a run's rows go: every output and chart, and the preview, across every range of a
/// partitioned run. Its files wait under their partial names until [`Sink::finish`] puts them
/// all in place.
pub(crate) struct Sink<'a> {
    plan: &'a Plan,
    folders: RunFolders<'a>,
    /// The result's columns, as the first feed gave them, which every later range's must be.
    columns: Option<Vec<String>>,
    /// The outputs being written, each started by the first feed after it was last completed.
    outputs: Vec<OutputRun<'a>>,
    /// Files complete and waiting to be put in place: those DuckDB wrote whole, and each range's
    /// own.
    done: Vec<Completed>,
    /// Each range's part of a Parquet output every range appends to, by the output's number,
    /// joined into its one file at the end (RS4e).
    parquet_parts: BTreeMap<usize, Vec<Completed>>,
    /// Started by the first feed.
    charts: Option<Vec<ChartRun<'a>>>,
    preview: Option<Preview>,
    /// What every feed gave the outputs.
    written: Count,
}

/// The rows and bytes a run has given its outputs so far.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Written {
    pub(crate) rows: u64,
    pub(crate) bytes: u64,
}

impl<'a> Sink<'a> {
    pub(crate) fn new(plan: &'a Plan, folders: RunFolders<'a>) -> Self {
        Self {
            plan,
            folders,
            columns: None,
            outputs: Vec::new(),
            done: Vec::new(),
            parquet_parts: BTreeMap::new(),
            charts: None,
            preview: None,
            written: Count::default(),
        }
    }

    pub(crate) fn folders(&self) -> RunFolders<'a> {
        self.folders
    }

    pub(crate) fn written(&self) -> Written {
        Written {
            rows: self.written.rows,
            bytes: self.written.bytes,
        }
    }

    /// Counts what a feed gave the outputs, once its caller knows: a combine step that wrote
    /// Parquet counts the rows `COPY` wrote, not the preview's it read back.
    pub(crate) fn count(&mut self, read: &Count) {
        self.written.rows += read.rows;
        self.written.bytes += read.bytes;
    }

    /// Writes `reader`'s rows, a page at a time, into every output and the preview, starting
    /// each output that isn't being written yet with the files `values` name. `opened` is when
    /// the feed started, for the time to its first row. Returns what the feed read.
    ///
    /// The caller closes `reader` and says the feed is done in between, so the feed's event
    /// can tell what only its end shows, such as the combine step's peaks.
    pub(crate) async fn stream<R: SourceReader>(
        &mut self,
        reader: &mut R,
        feed: &Feed<'_>,
        values: &RunValues,
        opened: Instant,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<Count, RunFailure> {
        let step = feed.step();
        self.check_columns(reader.columns(), &step)?;
        self.start(values, reader.columns())?;
        let mut read = Count::default();
        let mut clock = ProgressClock::new();
        loop {
            let (page, waited) = waiting(reader.next_page(), |so_far| {
                events(RunEvent::Progress {
                    step: step.clone(),
                    rows: read.rows,
                    bytes: read.bytes,
                    elapsed_ms: millis(opened.elapsed()),
                    batch: None,
                    parts: None,
                    busy: Some(clock.lap(so_far)),
                });
            })
            .await;
            read.waited(waited, read.first_row.is_none());
            clock.waited(waited);
            let page = match page {
                Ok(Some(page)) => page,
                Ok(None) => return Ok(read),
                Err(error) => return Err(error.at(step)),
            };
            if page.is_empty() {
                continue;
            }
            read.first_row.get_or_insert_with(|| opened.elapsed());
            read.add(&page);
            feed.check(&read)?;
            if let Some(preview) = &mut self.preview {
                keep_for_preview(preview, &page);
            }
            let (written, writing) = timed(async {
                self.outputs
                    .iter_mut()
                    .try_for_each(|output| output.write(&page))
            })
            .await;
            clock.wrote(writing);
            written?;
            for chart in self.charts.iter_mut().flatten() {
                chart.add(&page)?;
            }
            if clock.due() {
                events(RunEvent::Progress {
                    step: step.clone(),
                    rows: read.rows,
                    bytes: read.bytes,
                    elapsed_ms: millis(opened.elapsed()),
                    batch: None,
                    parts: None,
                    busy: Some(clock.lap(Duration::ZERO)),
                });
            }
        }
    }

    /// Whether `columns` are the first feed's, which an output appended to by every range was
    /// started with.
    fn check_columns(&mut self, columns: &[ExportColumn], step: &Step) -> Result<(), RunFailure> {
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        match &self.columns {
            None => {
                self.columns = Some(names);
                Ok(())
            }
            Some(first) if *first == names => Ok(()),
            Some(first) => Err(RunFailure::new(
                step.clone(),
                format!(
                    "this range returned the columns {}, unlike the first range's {}",
                    names.join(", "),
                    first.join(", ")
                ),
            )),
        }
    }

    /// Starts each output not being written, the charts and the preview, with `columns`.
    fn start(&mut self, values: &RunValues, columns: &[ExportColumn]) -> Result<(), RunFailure> {
        let running: Vec<usize> = self.outputs.iter().map(|output| output.number).collect();
        let started = start_outputs(self.plan, values, columns, self.folders, &running)?;
        self.outputs.extend(started);
        self.outputs.sort_by_key(|output| output.number);
        if self.charts.is_none() {
            self.charts = Some(start_charts(self.plan, values, columns));
        }
        self.preview.get_or_insert_with(|| Preview {
            columns: columns.iter().map(|column| column.name.clone()).collect(),
            rows: Vec::new(),
            more: false,
        });
        Ok(())
    }

    /// A chart that read its rows elsewhere, its own query's, to draw with the others.
    pub(crate) fn add_chart(&mut self, chart: ChartRun<'a>) {
        self.charts.get_or_insert_with(Vec::new).push(chart);
    }

    /// Files written whole elsewhere, to be put in place with the others; in a partitioned run,
    /// a range's part of a Parquet file every range appends to waits to be joined.
    pub(crate) fn add_copied(&mut self, copied: Vec<Completed>) {
        for file in copied {
            match self.appended_parquet(&file.step) {
                Some(number) => self.parquet_parts.entry(number).or_default().push(file),
                None => self.done.push(file),
            }
        }
    }

    /// The output `step` names, when it is a Parquet file that every range of a partitioned
    /// run appends to.
    fn appended_parquet(&self, step: &Step) -> Option<usize> {
        let Step::Output { number, .. } = step else {
            return None;
        };
        self.plan.partition.as_ref()?;
        let output = self.plan.outputs.get(number.checked_sub(1)?)?;
        let appended = output.format.written_by_duckdb().is_some() && !output.is_per_range();
        appended.then_some(*number)
    }

    /// Ends a range of a partitioned run: completes the files of the outputs whose name says
    /// the range, so the next range starts its own.
    pub(crate) fn end_range(&mut self) -> Result<(), RunFailure> {
        let (per_range, appended): (Vec<_>, Vec<_>) = std::mem::take(&mut self.outputs)
            .into_iter()
            .partition(|output| output.output.is_per_range());
        self.outputs = appended;
        let completed = complete_outputs(per_range)?;
        self.done.extend(completed);
        Ok(())
    }

    /// Removes every output's file, for a run that failed.
    pub(crate) fn abandon(self) {
        self.outputs.into_iter().for_each(OutputRun::abandon);
        self.done
            .into_iter()
            .chain(self.parquet_parts.into_values().flatten())
            .for_each(|done| done.file.abandon());
    }

    /// Joins each appended Parquet output's parts into its one file.
    async fn join_parquet_parts(&mut self) -> Result<(), RunFailure> {
        let join = PartsJoin {
            plan: self.plan,
            folders: self.folders,
        };
        let outputs = std::mem::take(&mut self.parquet_parts);
        let mut outputs = outputs.into_iter();
        while let Some((number, parts)) = outputs.next() {
            match join.join(number, parts).await {
                Ok(joined) => self.done.push(joined),
                Err(failure) => {
                    outputs
                        .flat_map(|(_, parts)| parts)
                        .for_each(|part| part.file.abandon());
                    return Err(failure);
                }
            }
        }
        Ok(())
    }

    /// Completes every file, draws every chart, and puts them all in place, sending the preview
    /// and each file.
    pub(crate) async fn finish(
        mut self,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<RunSummary, RunFailure> {
        if let Err(failure) = self.join_parquet_parts().await {
            self.abandon();
            return Err(failure);
        }
        let Self {
            outputs,
            mut done,
            charts,
            preview,
            folders,
            written,
            ..
        } = self;
        events(RunEvent::Preview(preview.unwrap_or_else(|| Preview {
            columns: Vec::new(),
            rows: Vec::new(),
            more: false,
        })));
        match complete_outputs(outputs) {
            Ok(completed) => done.extend(completed),
            Err(failure) => {
                done.into_iter().for_each(|done| done.file.abandon());
                return Err(failure);
            }
        }
        // In the file's order of outputs, wherever each was written, and a partitioned run's
        // ranges in their order within an output.
        done.sort_by_key(|done| match &done.step {
            Step::Output { number, .. } => *number,
            _ => usize::MAX,
        });
        let mut charts = charts.unwrap_or_default();
        charts.sort_by_key(ChartRun::number);
        for chart in charts {
            match chart.draw(folders) {
                Ok(drawn) => done.push(drawn),
                Err(failure) => {
                    done.into_iter().for_each(|done| done.file.abandon());
                    return Err(failure);
                }
            }
        }
        let files = put_in_place(done)?;
        for file in &files {
            events(RunEvent::OutputWritten(file.clone()));
        }
        Ok(RunSummary {
            rows: written.rows,
            bytes: written.bytes,
            files,
        })
    }
}

/// A folder of the run's own, its staging folder or its work folder, made for the run and
/// removed when it ends, however it ends.
struct OwnFolder<'a>(&'a Path);

impl<'a> OwnFolder<'a> {
    fn make(folder: &'a Path) -> Result<Self, RunFailure> {
        std::fs::create_dir_all(folder).map_err(|error| {
            RunFailure::new(
                Step::Worker,
                format!("could not make the folder {}: {error}", folder.display()),
            )
        })?;
        Ok(Self(folder))
    }
}

impl Drop for OwnFolder<'_> {
    fn drop(&mut self) {
        // The staging folder is empty by now unless the run failed; what can't be removed is
        // left for the app's side, which removes it again after the worker ends.
        let _ = std::fs::remove_dir_all(self.0);
    }
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `work`'s output, and how long it took.
pub(crate) async fn timed<T>(work: impl Future<Output = T>) -> (T, Duration) {
    let started = Instant::now();
    let output = work.await;
    (output, started.elapsed())
}

/// What a source, or the combine step, has returned so far, and how long it waited for it.
#[derive(Default)]
pub(crate) struct Count {
    pub(crate) rows: u64,
    pub(crate) bytes: u64,
    pub(crate) first_row: Option<Duration>,
    /// Waiting for each statement's first page of rows, or for its end when it returned none.
    pub(crate) waited_first: Duration,
    /// Waiting for the pages after each statement's first.
    pub(crate) waited_rest: Duration,
}

impl Count {
    /// Counts `wait` for a page, which came `before_first_row` of its statement or after.
    pub(crate) fn waited(&mut self, wait: Duration, before_first_row: bool) {
        if before_first_row {
            self.waited_first += wait;
        } else {
            self.waited_rest += wait;
        }
    }

    pub(crate) fn add(&mut self, page: &[Row]) {
        self.rows += page.len() as u64;
        self.bytes += page.iter().map(|row| row_bytes(row)).sum::<u64>();
    }

    /// Whether `source`'s caps hold what it has returned, or which one it crossed.
    pub(crate) fn check(&self, source: &QuerySource) -> Result<(), RunFailure> {
        let limits = source.limits;
        let step = Step::Source {
            id: source.id.clone(),
        };
        let raise_in = format!("the [[source]] {:?}", source.id);
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
                    set_in: None,
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
                    set_in: None,
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
    /// The time spent converting and writing its rows so far.
    busy: Duration,
}

/// Starts each output DbRover writes, but those numbered in `running`, which are being written
/// already.
fn start_outputs<'a>(
    plan: &'a Plan,
    values: &RunValues,
    columns: &[ExportColumn],
    folders: RunFolders<'_>,
    running: &[usize],
) -> Result<Vec<OutputRun<'a>>, RunFailure> {
    let mut started: Vec<OutputRun<'a>> = Vec::new();
    // DuckDB writes the Parquet outputs itself, before the rows come here (RS3d).
    let written_here =
        plan.outputs
            .iter()
            .zip(&values.files)
            .enumerate()
            .filter(|(index, (output, _))| {
                output.format.written_by_duckdb().is_none() && !running.contains(&(index + 1))
            });
    for (index, (output, file)) in written_here {
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
                busy: Duration::ZERO,
            }),
            Err(message) => {
                started.into_iter().for_each(OutputRun::abandon);
                return Err(RunFailure::new(step, message));
            }
        }
    }
    Ok(started)
}

/// A chart for each `[[chart]]` that draws the result, reading its rows with `columns`; a chart
/// with a query of its own reads that once the combine step is done.
fn start_charts<'a>(
    plan: &'a Plan,
    values: &RunValues,
    columns: &[ExportColumn],
) -> Vec<ChartRun<'a>> {
    plan.charts
        .iter()
        .zip(&values.charts)
        .enumerate()
        .filter(|(_, (chart, _))| chart.query.is_none())
        .map(|(index, (chart, file))| ChartRun::new(index + 1, chart, file, columns))
        .collect()
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
        let started = Instant::now();
        let written = self.convert_and_write(page);
        self.busy += started.elapsed();
        written
    }

    fn convert_and_write(&mut self, page: &[Row]) -> Result<(), RunFailure> {
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
                    set_in: None,
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
                    set_in: None,
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

/// An output's or a chart's file, complete, still under its partial name.
pub(crate) struct Completed {
    pub(crate) step: Step,
    pub(crate) rows: u64,
    pub(crate) warnings: Vec<String>,
    /// The time spent in its writer, completing the file included.
    pub(crate) busy: Duration,
    pub(crate) file: CompletedFile,
}

fn complete_outputs(outputs: Vec<OutputRun<'_>>) -> Result<Vec<Completed>, RunFailure> {
    let mut completed: Vec<Completed> = Vec::new();
    let mut outputs = outputs.into_iter();
    while let Some(output) = outputs.next() {
        let step = output.step();
        let warnings = output.converter.warnings();
        let started = Instant::now();
        match output.writer.complete() {
            Ok(file) => completed.push(Completed {
                step,
                rows: output.rows,
                warnings,
                busy: output.busy + started.elapsed(),
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
        let output = match &done.step {
            Step::Output { number, .. } => Some(*number),
            _ => None,
        };
        written.push(WrittenFile {
            output,
            path,
            rows: done.rows,
            size,
            elapsed_ms: millis(done.busy),
            warnings: done.warnings,
        });
    }
    Ok(written)
}

/// An error and its causes, as one sentence.
pub(crate) fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    dbrover_core::describe_error(error)
}
