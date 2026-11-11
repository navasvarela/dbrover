//! A run with a combine step (`docs/reporting.md`, "RP2 — Several sources"):
//!
//! 1. open the run's DuckDB on a file in the run's work folder, spilling beside it;
//! 2. attach each DuckDB file, and load every other source in rounds, the sources of a round
//!    at the same time, each under its caps, a batch of its lookup's values at a time, typing
//!    its columns once its last row is in;
//! 3. lock the database down, and run the combine query under its timeout, its rows going to
//!    the outputs as a source's do without a combine step;
//! 4. run each chart's own query, if it has one, under the same limits, its rows going to the
//!    chart.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbrover_core::{CancelHandle, Connection, DriverError, ResultCursor, Row, describe_error};
use dbrover_driver_duckdb::{
    DuckDbType, LoadColumn, TableLoader, Workspace, WorkspaceLimit, WorkspaceLimits,
    WorkspaceProfile, WorkspaceStore,
};
use dbrover_export::ExportColumn;
use dbrover_language::Literal;
use dbrover_sql::Dialect;
use futures_util::future::try_join_all;
use tokio::sync::mpsc;

use crate::chart::ChartRun;
use crate::declared::Shaped;
use crate::live::{ProgressClock, waiting};
use crate::parquet::{ParquetCopy, parquet_files};
use crate::run::{Count, Feed, Outputs, Streamed, millis, timed};
use crate::statement::{Statements, lookup_literal};
use crate::typing::{ColumnTyper, DriverTypes};
use crate::{
    AttachedSource, Batch, Busy, Chart, CombineLimits, Combined, CostCheck, LimitHit, LimitSetIn,
    LoadedColumn, Parts, PlannedLookup, PlannedSource, PlannedSourcePartition, PlannedSplit,
    QuerySource, RunEvent, RunFailure, RunSummary, RunValues, SourceError, SourceOpener,
    SourcePhases, SourceRanges, SourceReader, SourceStatement, Step, grouped,
};

/// Rows asked of the combine query at a time, as a source's are.
const PAGE_ROWS: usize = 1_000;

/// The pages a source's reader may be ahead of its loader: enough that receiving and appending
/// overlap, few enough that a fast network never piles rows up in the worker. With the page
/// being read and the one being appended, a source holds at most two more than this at once,
/// which is what bounds the worker's memory outside DuckDB (`docs/reports-at-scale.md`, RS3c).
pub const QUEUE_PAGES: usize = 4;

/// How long an interrupted query gets to stop before the run gives up on it.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// The run's database file, in its work folder.
const WORKSPACE_FILE: &str = "workspace.duckdb";

/// The folder in the run's work folder that DuckDB spills to.
const SPILL_FOLDER: &str = "spill";

/// A run with a combine step, and what its steps read.
pub(crate) struct Combining<'a, O> {
    pub(crate) outputs: Outputs<'a>,
    pub(crate) combined: &'a Combined,
    pub(crate) opener: &'a O,
}

impl<'a, O: SourceOpener> Combining<'a, O> {
    /// Runs the combine step: see the module.
    pub(crate) async fn run(
        &self,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<RunSummary, RunFailure> {
        let limits = self.limits();
        let work = self.outputs.folders.work;
        let store = WorkspaceStore::File(work.join(WORKSPACE_FILE));
        let mut workspace = Workspace::open(
            &store,
            &WorkspaceLimits {
                memory_limit: limits.memory_limit.0,
                threads: limits.threads(),
                temp_directory: work.join(SPILL_FOLDER),
                max_temp_size: limits.max_temp_size.0,
            },
        )
        .await
        .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;
        let outcome = self.combine(&mut workspace, events).await;
        workspace.close().await;
        outcome
    }

    fn limits(&self) -> &CombineLimits {
        &self.combined.combine.limits
    }

    async fn combine(
        &self,
        workspace: &mut Workspace,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<RunSummary, RunFailure> {
        for source in &self.combined.sources {
            if let PlannedSource::Attached(attached) = source {
                attach(workspace, attached).await?;
                events(RunEvent::SourceAttached {
                    id: attached.id.clone(),
                    path: attached.path.clone(),
                });
            }
        }
        let mut loaded: Vec<LoadedTable> = Vec::new();
        for round in self.combined.rounds() {
            let events = RefCell::new(&mut *events);
            let loads = round
                .into_iter()
                .map(|source| self.load(source, workspace, &loaded, &events));
            let tables = try_join_all(loads).await?;
            loaded.extend(tables);
        }
        let folders = self.outputs.folders;
        let parquet = parquet_files(
            self.outputs.plan,
            self.outputs.values,
            folders.folder,
            folders.staging,
        );
        // DuckDB writes in the staging folder only when it writes the Parquet files there.
        let writable = (!parquet.is_empty()).then_some(folders.staging);
        workspace
            .lock(writable)
            .await
            .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;

        let started = Instant::now();
        let combine_query = self.combined.combine.query.sql();
        let copied = if parquet.is_empty() {
            None
        } else {
            let copy = ParquetCopy {
                query: combine_query,
                values: self.outputs.values,
                limits: self.limits(),
            };
            Some(copy.write(workspace, parquet, events).await?)
        };
        let query = copied.as_ref().map_or_else(
            || combine_query.to_owned(),
            |copied| copied.read_back(self.streams_every_row()),
        );
        let mut clock = ProgressClock::new();
        let (reader, starting) = waiting(
            Box::pin(CombineReader::start(
                workspace,
                &query,
                self.outputs.values,
                DuckDbQuery::Combine,
                self.limits(),
            )),
            |so_far| {
                events(progress(
                    &Step::Combine,
                    &Count::default(),
                    started,
                    clock.lap(so_far),
                ));
            },
        )
        .await;
        let mut reader = reader.map_err(|error| error.at(Step::Combine))?;
        let streamed = self
            .outputs
            .stream(&mut reader, &Feed::Combine, started, events)
            .await;
        reader.close().await;
        let mut streamed = streamed?;
        let read_ms = millis(starting + streamed.read.waited_first + streamed.read.waited_rest);
        let (profile, query_ms) = match copied {
            None => (read_profile(workspace).await, read_ms),
            Some(copied) => {
                // The preview may have read only its rows of the file.
                streamed.read.rows = copied.rows;
                streamed.add_copied(copied.files);
                (copied.profile, millis(copied.query))
            }
        };
        let peaks = profile.peaks;
        events(RunEvent::CombineDone {
            rows: streamed.read.rows,
            bytes: streamed.read.bytes,
            first_row_ms: streamed.read.first_row.map(millis),
            elapsed_ms: millis(started.elapsed()),
            peak_memory: peaks.memory,
            peak_temp_size: peaks.temp_size,
            query_ms: Some(query_ms),
            plan: profile.plan,
        });
        if let Err(failure) = self.query_charts(workspace, &mut streamed).await {
            streamed.abandon();
            return Err(failure);
        }
        streamed.finish(events)
    }

    /// Whether anything but the preview reads every row of the result: an output DbRover
    /// writes, or a chart drawn from the result rather than a query of its own.
    fn streams_every_row(&self) -> bool {
        let plan = self.outputs.plan;
        plan.outputs
            .iter()
            .any(|output| output.format.written_by_duckdb().is_none())
            || plan.charts.iter().any(|chart| chart.query.is_none())
    }

    /// Each chart with a query of its own, which runs in the locked-down workspace now the
    /// combine step is done, a chart at a time in the file's order, under the combine step's
    /// limits (`docs/charts-as-code.md`, CH3e), its rows going to the chart.
    async fn query_charts(
        &self,
        workspace: &mut Workspace,
        streamed: &mut Streamed<'a>,
    ) -> Result<(), RunFailure> {
        let plan = self.outputs.plan;
        let files = &self.outputs.values.charts;
        for (index, (chart, file)) in plan.charts.iter().zip(files).enumerate() {
            if let Some(query) = &chart.query {
                let read = self.chart_rows(workspace, index + 1, chart, file, query);
                streamed.add_chart(read.await?);
            }
        }
        Ok(())
    }

    /// Runs a chart's `query` and gives its rows to the chart.
    async fn chart_rows(
        &self,
        workspace: &mut Workspace,
        number: usize,
        chart: &'a Chart,
        file: &str,
        query: &str,
    ) -> Result<ChartRun<'a>, RunFailure> {
        let step = Step::Chart {
            number,
            file: file.to_owned(),
        };
        let query_of = DuckDbQuery::Chart;
        let mut reader = CombineReader::start(
            workspace,
            query,
            self.outputs.values,
            query_of,
            self.limits(),
        )
        .await
        .map_err(|error| error.at(step.clone()))?;
        let mut run = ChartRun::new(number, chart, file, reader.columns());
        let read = loop {
            match reader.next_page().await {
                Ok(Some(page)) => {
                    if let Err(failure) = run.add(&page) {
                        break Err(failure);
                    }
                }
                Ok(None) => break Ok(()),
                Err(error) => break Err(error.at(step.clone())),
            }
        };
        reader.close().await;
        read.map(|()| run)
    }

    /// Reads `source` into its table under its caps, a batch of its lookup's values at a
    /// time if it has one, then types its columns.
    async fn load(
        &self,
        source: &QuerySource,
        workspace: &Workspace,
        loaded: &[LoadedTable],
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<LoadedTable, RunFailure> {
        let opened = Instant::now();
        let mut loading = Loading {
            source,
            limits: self.limits(),
            opened,
            table: None,
            read: Count::default(),
            spent: Spent::default(),
            clock: ProgressClock::new(),
            scanned: None,
            at: At::Whole,
        };
        let statements = Statements::new(self.outputs.plan, self.outputs.values, source);
        let deadline = opened + source.limits.timeout;
        let sent = match &source.split {
            PlannedSplit::Whole => {
                let statement = statements.whole(opened);
                loading
                    .fill(self.opener, &statement, workspace, events)
                    .await?;
                Sent::Whole
            }
            PlannedSplit::Lookup(lookup) => {
                let (values, reading) = timed(lookup_values(lookup, workspace, loaded)).await;
                loading.spent.load += reading;
                let values = values.map_err(|error| error.at(loading.step()))?;
                let batches: Vec<&[Literal]> = values.chunks(lookup.batch_size.get()).collect();
                let of = batches.len() as u64;
                for (index, batch) in batches.into_iter().enumerate() {
                    let statement = statements.batch(batch, deadline);
                    loading.at = At::Batch(Batch {
                        number: index as u64 + 1,
                        of,
                    });
                    loading
                        .fill(self.opener, &statement, workspace, events)
                        .await?;
                }
                if of == 0 {
                    loading
                        .load_nothing(&statements, deadline, self.opener, workspace, events)
                        .await?;
                }
                Sent::Batches(of)
            }
            PlannedSplit::Partition(partition) => {
                let ranges = self.outputs.values.ranges.get(&source.id).ok_or_else(|| {
                    RunFailure::new(loading.step(), "the run's values have no ranges for it")
                })?;
                let reading = Ranges {
                    statements: &statements,
                    ranges,
                    partition,
                };
                reading
                    .load(&mut loading, self.opener, workspace, events)
                    .await?;
                Sent::Parts(ranges.ranges.len() as u64)
            }
        };
        loading.finish(sent, events).await
    }
}

/// A loaded source's table, for the lookups of the rounds after it: its name, and each
/// column's name and type.
struct LoadedTable {
    id: String,
    columns: Vec<(String, DuckDbType)>,
}

/// The distinct values of `lookup`'s column, in the table of the source it reads, as
/// literals of the column's type.
async fn lookup_values(
    lookup: &PlannedLookup,
    workspace: &Workspace,
    loaded: &[LoadedTable],
) -> Result<Vec<Literal>, SourceError> {
    let table = loaded
        .iter()
        .find(|table| table.id.eq_ignore_ascii_case(&lookup.from))
        .ok_or_else(|| {
            SourceError::new(format!(
                "lookup: {:?} wasn't loaded before this source",
                lookup.from
            ))
        })?;
    let (name, column_type) = table
        .columns
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&lookup.column))
        .ok_or_else(|| {
            let names: Vec<&str> = table
                .columns
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            SourceError::new(format!(
                "lookup: {:?} has no column {:?}; it has {}",
                lookup.from,
                lookup.column,
                names.join(", ")
            ))
        })?;
    let values = workspace
        .distinct_values(&table.id, name)
        .await
        .map_err(|error| SourceError::new(describe_error(&error)))?;
    Ok(values
        .into_iter()
        .map(|value| lookup_literal(*column_type, value))
        .collect())
}

async fn attach(workspace: &Workspace, source: &AttachedSource) -> Result<(), RunFailure> {
    workspace.attach(&source.id, &source.path).await.map_err(|error| {
        let message = describe_error(&error);
        // DuckDB locks a file one way at a time between processes, and the app may have it
        // open for writing.
        let hint = if message.contains("lock") {
            ". DuckDB opens a file for writing in one process at a time: if DbRover has it open \
             for writing, make the data source read-only, or disconnect it"
        } else {
            ""
        };
        RunFailure::new(
            Step::Source {
                id: source.id.clone(),
            },
            format!(
                "could not attach {} ({:?}): {message}{hint}",
                source.path.display(),
                source.data_source.name
            ),
        )
    })
}

/// A source's table being loaded, as text, the types its values say, and the bytes each
/// column's values took.
struct Table {
    loader: TableLoader,
    typer: ColumnTyper,
    column_bytes: Vec<u64>,
}

impl Table {
    /// Adds the bytes of each column's values in `page`.
    fn count(&mut self, page: &[Row]) {
        for row in page {
            for (bytes, value) in self.column_bytes.iter_mut().zip(row) {
                *bytes += value.as_ref().map_or(0, |value| value.len() as u64);
            }
        }
    }
}

/// The time a source being loaded spent outside waiting for its rows, which its [`Count`]
/// holds.
#[derive(Default)]
struct Spent {
    connect: Duration,
    /// Each statement's start, up to its cursor; its wait for the first page is in `Count`.
    start: Duration,
    /// Held by DuckDB: waiting for room in the queue, and for the last pages to be appended,
    /// and reading a lookup's values. Appending itself overlaps receiving, so it isn't counted.
    load: Duration,
    typing: Duration,
}

/// A source being loaded into its table.
struct Loading<'a> {
    source: &'a QuerySource,
    limits: &'a CombineLimits,
    /// When the source started to be opened, for the time to its first row.
    opened: Instant,
    /// Made when the first statement's columns are known.
    table: Option<Table>,
    /// What every statement has returned so far, which the source's caps hold.
    read: Count,
    spent: Spent,
    clock: ProgressClock,
    /// The bytes its engine scanned for every statement so far, where it says.
    scanned: Option<u64>,
    /// Which of its statements it is sending.
    at: At,
}

/// Which of a source's statements is being read, for its progress and its caps.
#[derive(Debug, Clone, Copy)]
enum At {
    Whole,
    Batch(Batch),
    /// A range of a partitioned source, the rows it has returned so far, and the most it may.
    Range {
        parts: Parts,
        number: u64,
        rows: u64,
        max_rows: Option<u64>,
    },
}

impl At {
    fn batch(self) -> Option<Batch> {
        match self {
            Self::Batch(batch) => Some(batch),
            Self::Whole | Self::Range { .. } => None,
        }
    }

    fn parts(self) -> Option<Parts> {
        match self {
            Self::Range { parts, .. } => Some(parts),
            Self::Whole | Self::Batch(_) => None,
        }
    }

    /// The statement's number, for a message about one of several.
    fn number(self) -> u64 {
        match self {
            Self::Whole => 1,
            Self::Batch(batch) => batch.number,
            Self::Range { number, .. } => number,
        }
    }
}

/// What statements a loaded source sent.
enum Sent {
    Whole,
    /// One a batch of its lookup's values.
    Batches(u64),
    /// One a range of its partition.
    Parts(u64),
}

/// A partitioned source's ranges, read one after another into its table (RS4b).
struct Ranges<'s, 'a> {
    statements: &'s Statements<'a>,
    ranges: &'s SourceRanges,
    partition: &'s PlannedSourcePartition,
}

impl Ranges<'_, '_> {
    /// Reads each range in turn. The first range's statement has the cost guard judge the
    /// whole period, so a source too costly to read is refused before any range runs.
    async fn load<O: SourceOpener>(
        &self,
        loading: &mut Loading<'_>,
        opener: &O,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let kind = self.partition.partition.kind;
        let of = self.ranges.ranges.len() as u64;
        for (done, range) in (0..).zip(&self.ranges.ranges) {
            let cost = if done == 0 {
                self.statements.whole_period(&self.ranges.whole, kind)
            } else {
                CostCheck::Judged
            };
            let statement = self.statements.range(range, kind, Instant::now(), cost);
            loading.at = At::Range {
                parts: Parts {
                    done,
                    running: 1,
                    of,
                },
                number: range.number,
                rows: 0,
                max_rows: self.partition.max_rows_per_part,
            };
            loading.fill(opener, &statement, workspace, events).await?;
        }
        Ok(())
    }
}

/// Appends the pages `queued` to `table` as they come, until the reader ends; and when it was
/// done. Stops at the first page DuckDB refuses, which closes the queue.
async fn append_queued(
    table: &mut Table,
    mut queued: mpsc::Receiver<Vec<Row>>,
) -> (Result<(), DriverError>, Instant) {
    let mut outcome = Ok(());
    while let Some(page) = queued.recv().await {
        table.typer.see(&page);
        table.count(&page);
        if let Err(error) = table.loader.append(page).await {
            outcome = Err(error);
            break;
        }
    }
    queued.close();
    (outcome, Instant::now())
}

/// What the profiler measured of the last statement, or nothing when it can't be read.
pub(crate) async fn read_profile(workspace: &Workspace) -> WorkspaceProfile {
    workspace.profile().await.unwrap_or_else(|error| {
        tracing::warn!(
            "could not read the run's DuckDB's profile: {}",
            describe_error(&error)
        );
        WorkspaceProfile::default()
    })
}

/// A step's progress: what it has read since `opened`, and what it was busy with.
pub(crate) fn progress(step: &Step, read: &Count, opened: Instant, busy: Busy) -> RunEvent {
    progress_at(step, read, opened, At::Whole, busy)
}

/// A source's progress, on the statement `at`.
fn progress_at(step: &Step, read: &Count, opened: Instant, at: At, busy: Busy) -> RunEvent {
    RunEvent::Progress {
        step: step.clone(),
        rows: read.rows,
        bytes: read.bytes,
        elapsed_ms: millis(opened.elapsed()),
        batch: at.batch(),
        parts: at.parts(),
        busy: Some(busy),
    }
}

impl Loading<'_> {
    fn step(&self) -> Step {
        Step::Source {
            id: self.source.id.clone(),
        }
    }

    fn duckdb_failure(&self, error: &DriverError) -> RunFailure {
        duckdb_failure(error, self.limits).at(self.step())
    }

    /// Counts `rows` against the range being read, which `max_rows_per_part` bounds.
    fn count_range(&mut self, rows: usize) -> Result<(), RunFailure> {
        let At::Range {
            number,
            rows: so_far,
            max_rows,
            parts,
        } = self.at
        else {
            return Ok(());
        };
        let rows = so_far.saturating_add(rows as u64);
        self.at = At::Range {
            parts,
            number,
            rows,
            max_rows,
        };
        match max_rows {
            Some(max) if rows > max => {
                let value = format!("{} rows", grouped(max));
                Err(RunFailure::at_limit(
                    self.step(),
                    format!(
                        "range {number} of {} returned more than {value}, its max_rows_per_part; \
                         narrow its ranges with a smaller every, filter in its query, or raise \
                         max_rows_per_part",
                        parts.of
                    ),
                    LimitHit {
                        key: "max_rows_per_part".to_owned(),
                        value,
                        raise_in: format!("the [[source]] {:?}", self.source.id),
                        set_in: Some(LimitSetIn::File),
                    },
                ))
            }
            _ => Ok(()),
        }
    }

    /// Runs `statement` and appends its rows to the table, a page at a time, watching the
    /// source's caps.
    async fn fill<O: SourceOpener>(
        &mut self,
        opener: &O,
        statement: &SourceStatement<'_>,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let step = self.step();
        let at = self.at;
        let (reader, opening_time) = waiting(Box::pin(Shaped::open(opener, statement)), |so_far| {
            let busy = self.clock.lap(so_far);
            (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
        })
        .await;
        self.clock.waited(opening_time);
        let mut reader = reader.map_err(|error| error.at(step))?;
        let opening = reader.opening();
        self.spent.connect += opening.connect;
        self.spent.start += opening.start;
        let filled = self.fill_from(&mut reader, workspace, events).await;
        if let Some(scanned) = reader.scanned() {
            self.scanned = Some(self.scanned.unwrap_or(0).saturating_add(scanned));
        }
        reader.close().await;
        filled
    }

    /// Reads `reader`'s pages into the table: the reader and the loader take turns at the
    /// source's task, joined by a queue of [`QUEUE_PAGES`], so the next page is received while
    /// DuckDB appends the last one.
    async fn fill_from<R: SourceReader>(
        &mut self,
        reader: &mut R,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let mut table = self.take_table(reader.columns(), workspace).await?;
        let (pages, queued) = mpsc::channel(QUEUE_PAGES);
        let ((read, read_ended), (appended, appended_ended)) = tokio::join!(
            self.read_into(reader, pages, events),
            append_queued(&mut table, queued),
        );
        // What DuckDB still had to append once the last page was read held the source too.
        self.spent.load += appended_ended.saturating_duration_since(read_ended);
        self.table = Some(table);
        // A loader that failed stopped the reader: its error is the one to tell.
        appended.map_err(|error| self.duckdb_failure(&error))?;
        read
    }

    /// Reads `reader`'s pages into `pages`, under the source's caps, until it has no more,
    /// fails, or the loader stops; and when it ended.
    async fn read_into<R: SourceReader>(
        &mut self,
        reader: &mut R,
        pages: mpsc::Sender<Vec<Row>>,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> (Result<(), RunFailure>, Instant) {
        let step = self.step();
        let at = self.at;
        let mut rows_seen = false;
        let outcome = loop {
            let (page, waited) = waiting(reader.next_page(), |so_far| {
                let busy = self.clock.lap(so_far);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            })
            .await;
            self.read.waited(waited, !rows_seen);
            self.clock.waited(waited);
            let page = match page {
                Ok(Some(page)) => page,
                Ok(None) => break Ok(()),
                Err(error) => break Err(error.at(step.clone())),
            };
            if page.is_empty() {
                continue;
            }
            rows_seen = true;
            self.read
                .first_row
                .get_or_insert_with(|| self.opened.elapsed());
            self.read.add(&page);
            if let Err(failure) = self.read.check(self.source) {
                break Err(failure);
            }
            if let Err(failure) = self.count_range(page.len()) {
                break Err(failure);
            }
            let (sent, room) = waiting(pages.send(page), |so_far| {
                let busy = self.clock.lap_loading(so_far);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            })
            .await;
            self.spent.load += room;
            self.clock.loaded(room);
            if sent.is_err() {
                // The loader failed, and says why.
                break Ok(());
            }
            if self.clock.due() {
                let busy = self.clock.lap(Duration::ZERO);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            }
        };
        (outcome, Instant::now())
    }

    /// The table for `columns`: made for the first statement, whose columns a later batch's
    /// must be. The caller puts it back once its rows are in.
    async fn take_table(
        &mut self,
        columns: &[ExportColumn],
        workspace: &Workspace,
    ) -> Result<Table, RunFailure> {
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        if let Some(table) = self.table.take() {
            let first: Vec<&str> = table.typer.names().collect();
            if first == names {
                return Ok(table);
            }
            let (statement, number) = match self.at {
                At::Range { number, .. } => ("range", number),
                At::Whole | At::Batch(_) => ("batch", self.at.number()),
            };
            return Err(RunFailure::new(
                self.step(),
                format!(
                    "{statement} {number} returned the columns {}, unlike the first {statement}'s {}",
                    names.join(", "),
                    first.join(", ")
                ),
            ));
        }
        let drivers = DriverTypes::of(self.source.data_source.config.driver());
        let typer = ColumnTyper::new(columns, self.source.columns.as_deref(), drivers);
        let load_columns: Vec<LoadColumn> = names
            .into_iter()
            .zip(typer.known())
            .map(|(name, known)| LoadColumn {
                name,
                known: *known,
            })
            .collect();
        let loader = workspace
            .loader(&self.source.id, &load_columns)
            .await
            .map_err(|error| self.duckdb_failure(&error))?;
        Ok(Table {
            loader,
            typer,
            column_bytes: vec![0; columns.len()],
        })
    }

    /// For a lookup that found no value: a source with declared columns sends nothing and
    /// gets an empty table of them; one without runs once with a list of one `NULL`, which
    /// matches nothing, for its result's columns.
    async fn load_nothing<O: SourceOpener>(
        &mut self,
        statements: &Statements<'_>,
        deadline: Instant,
        opener: &O,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        match &self.source.columns {
            Some(declared) => {
                let columns: Vec<ExportColumn> = declared
                    .iter()
                    .map(|column| ExportColumn {
                        name: column.name.clone(),
                        type_name: None,
                        kind: column.column_type.kind(),
                        excel_format: None,
                    })
                    .collect();
                let table = self.take_table(&columns, workspace).await?;
                self.table = Some(table);
                Ok(())
            }
            None => {
                let statement = statements.batch(&[Literal::Null], deadline);
                self.at = At::Whole;
                self.fill(opener, &statement, workspace, events).await
            }
        }
    }

    /// Types the table's columns, makes it, and says so; `batches` is how many statements
    /// a lookup sent.
    async fn finish(
        self,
        sent: Sent,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<LoadedTable, RunFailure> {
        let step = self.step();
        let Some(Table {
            loader,
            typer,
            column_bytes,
        }) = self.table
        else {
            return Err(RunFailure::new(step, "the source sent no statement"));
        };
        let (types, warnings) = typer.types();
        let declared = typer.is_declared();
        let columns: Vec<(String, DuckDbType)> = typer
            .names()
            .map(str::to_owned)
            .zip(types.iter().copied())
            .collect();
        let (typed, typing) = timed(loader.finish(&types)).await;
        typed.map_err(|error| duckdb_failure(&error, self.limits).at(step))?;
        let spent = &self.spent;
        let phases = SourcePhases {
            connect_ms: millis(spent.connect),
            server_ms: millis(spent.start + self.read.waited_first),
            transfer_ms: millis(self.read.waited_rest),
            load_ms: millis(spent.load),
            type_ms: millis(spent.typing + typing),
        };
        (events.borrow_mut())(RunEvent::SourceDone {
            id: self.source.id.clone(),
            rows: self.read.rows,
            bytes: self.read.bytes,
            first_row_ms: self.read.first_row.map(millis),
            elapsed_ms: millis(self.opened.elapsed()),
            table: columns
                .iter()
                .zip(&column_bytes)
                .map(|((name, column_type), bytes)| LoadedColumn {
                    name: name.clone(),
                    type_name: column_type.to_string(),
                    declared,
                    bytes: Some(*bytes),
                })
                .collect(),
            warnings,
            batches: match sent {
                Sent::Batches(batches) => Some(batches),
                Sent::Whole | Sent::Parts(_) => None,
            },
            parts: match sent {
                Sent::Parts(parts) => Some(parts),
                Sent::Whole | Sent::Batches(_) => None,
            },
            phases: Some(phases),
            scanned_bytes: self.scanned,
        });
        Ok(LoadedTable {
            id: self.source.id.clone(),
            columns,
        })
    }
}

/// What DuckDB's error says, naming the limit when it hit one of the combine step's.
fn duckdb_failure(error: &DriverError, limits: &CombineLimits) -> SourceError {
    let message = describe_error(error);
    // DuckDB's own advice follows, to `SET memory_limit` and the like, which a report can't:
    // the advice below says what can.
    let message = message
        .split_once("\n\nPossible solutions")
        .map_or(message.as_str(), |(cause, _)| cause)
        .trim_end()
        .to_owned();
    let (key, value, advice) = match WorkspaceLimit::of(error) {
        Some(WorkspaceLimit::Memory) => (
            "memory_limit",
            limits.memory_limit.to_string(),
            "combine fewer rows, or raise memory_limit",
        ),
        Some(WorkspaceLimit::TempDirectory) => (
            "max_temp_size",
            limits.max_temp_size.to_string(),
            "combine fewer rows, or raise max_temp_size",
        ),
        None => return SourceError::new(message),
    };
    SourceError {
        message: format!("DuckDB ran past {value}, its {key}, and stopped: {message}; {advice}"),
        limit: Some(Box::new(LimitHit {
            key: key.to_owned(),
            value,
            raise_in: "[combine]".to_owned(),
            set_in: None,
        })),
    }
}

/// Which query of the run's DuckDB a [`CombineReader`] reads, which its timeout's message
/// names.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DuckDbQuery {
    Combine,
    /// A chart's own query, under the combine step's limits.
    Chart,
}

/// The rows of a query of the run's DuckDB, the combine step's or a chart's, read a page at a
/// time until its timeout.
pub(crate) struct CombineReader<'a> {
    cursor: Option<Box<dyn ResultCursor>>,
    columns: Vec<ExportColumn>,
    cancel: Arc<dyn CancelHandle>,
    deadline: Instant,
    query: DuckDbQuery,
    limits: &'a CombineLimits,
}

impl<'a> CombineReader<'a> {
    /// Starts `query` with `values` bound, in the locked-down workspace. It has the combine
    /// step's `timeout`, counted from now.
    pub(crate) async fn start(
        workspace: &mut Workspace,
        query: &str,
        values: &RunValues,
        query_of: DuckDbQuery,
        limits: &'a CombineLimits,
    ) -> Result<Self, SourceError> {
        let deadline = Instant::now() + limits.timeout;
        let prepared = dbrover_sql::bind(query, Dialect::DuckDb, &values.parameters)
            .map_err(|missing| SourceError::new(missing.to_string()))?;
        let connection = workspace.connection();
        let cancel = connection.cancel_handle();
        let started = async {
            connection
                .execute_bound(&prepared.sql, &prepared.values)
                .await
                .map_err(|error| duckdb_failure(&error, limits))
        };
        let cursor = watch(started, deadline, &cancel, query_of, limits).await?;
        let columns = cursor
            .columns()
            .iter()
            .map(|column| ExportColumn::new(column, Dialect::DuckDb))
            .collect();
        Ok(Self {
            cursor: Some(cursor),
            columns,
            cancel,
            deadline,
            query: query_of,
            limits,
        })
    }
}

impl CombineReader<'_> {
    /// Ends the statement: the rows a statement that changes or writes them says it did.
    pub(crate) async fn finish(self) -> Result<Option<u64>, SourceError> {
        let Some(cursor) = self.cursor else {
            return Ok(None);
        };
        cursor
            .finish()
            .await
            .map_err(|error| duckdb_failure(&error, self.limits))
    }
}

impl SourceReader for CombineReader<'_> {
    fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        let Some(cursor) = self.cursor.as_mut().filter(|cursor| !cursor.is_exhausted()) else {
            return Ok(None);
        };
        let limits = self.limits;
        let page = async {
            cursor
                .next_page(PAGE_ROWS)
                .await
                .map_err(|error| duckdb_failure(&error, limits))
        };
        let page = watch(page, self.deadline, &self.cancel, self.query, self.limits).await?;
        Ok((!page.is_empty()).then_some(page))
    }

    async fn close(self) {
        if let Some(cursor) = self.cursor
            && let Err(error) = cursor.finish().await
        {
            tracing::debug!("the combine query ended with: {}", describe_error(&error));
        }
    }
}

/// Runs `work` until `deadline`, then interrupts the query and fails with the combine step's
/// timeout as the limit hit.
async fn watch<T>(
    work: impl Future<Output = Result<T, SourceError>>,
    deadline: Instant,
    cancel: &Arc<dyn CancelHandle>,
    query: DuckDbQuery,
    limits: &CombineLimits,
) -> Result<T, SourceError> {
    let mut work = std::pin::pin!(work);
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        outcome = &mut work => return outcome,
        () = tokio::time::sleep(remaining) => {}
    }
    if let Err(error) = cancel.cancel().await {
        tracing::warn!(
            "could not interrupt the combine query: {}",
            describe_error(&error)
        );
    }
    // Whatever it returns now, it ended because of the interrupt.
    let _ = tokio::time::timeout(CANCEL_GRACE, work).await;
    let value = friendly(limits.timeout);
    let message = match query {
        DuckDbQuery::Combine => format!(
            "the combine query ran past {value}, its timeout, and was interrupted; combine fewer \
             rows, or raise timeout"
        ),
        DuckDbQuery::Chart => format!(
            "the chart's query ran past {value}, the combine step's timeout, and was \
             interrupted; read fewer rows in it, or raise timeout in [combine]"
        ),
    };
    Err(SourceError {
        message,
        limit: Some(Box::new(LimitHit {
            key: "timeout".to_owned(),
            value,
            raise_in: "[combine]".to_owned(),
            set_in: None,
        })),
    })
}

/// `10m`, `1h 30m`, as a report file writes a duration.
fn friendly(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}
