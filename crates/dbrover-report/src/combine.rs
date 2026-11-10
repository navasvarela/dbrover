//! A run with a combine step (`docs/reporting.md`, "RP2 — Several sources"):
//!
//! 1. open the run's DuckDB, with its temporary folder in the run's staging folder;
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
    DuckDbType, TableLoader, Workspace, WorkspaceLimit, WorkspaceLimits, WorkspacePeaks,
};
use dbrover_export::ExportColumn;
use dbrover_language::Literal;
use dbrover_sql::Dialect;
use futures_util::future::try_join_all;

use crate::chart::ChartRun;
use crate::declared::Shaped;
use crate::live::{ProgressClock, waiting};
use crate::run::{Count, Feed, Outputs, Streamed, millis, timed};
use crate::statement::{Statements, lookup_literal};
use crate::typing::ColumnTyper;
use crate::{
    AttachedSource, Batch, Busy, Chart, CombineLimits, Combined, LimitHit, LoadedColumn,
    PlannedLookup, PlannedSource, QuerySource, RunEvent, RunFailure, RunSummary, RunValues,
    SourceError, SourceOpener, SourcePhases, SourceReader, SourceStatement, Step,
};

/// Rows asked of the combine query at a time, as a source's are.
const PAGE_ROWS: usize = 1_000;

/// How long an interrupted query gets to stop before the run gives up on it.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// The folder in the run's staging folder that DuckDB spills to.
const SPILL_FOLDER: &str = "duckdb";

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
        let mut workspace = Workspace::open(&WorkspaceLimits {
            memory_limit: limits.memory_limit.0,
            threads: limits.threads(),
            temp_directory: self.outputs.folders.staging.join(SPILL_FOLDER),
            max_temp_size: limits.max_temp_size.0,
        })
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
        workspace
            .lock()
            .await
            .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;

        let started = Instant::now();
        let query = self.combined.combine.query.sql();
        let mut clock = ProgressClock::new();
        let (reader, starting) = waiting(
            Box::pin(CombineReader::start(
                workspace,
                query,
                self.outputs.values,
                DuckDbQuery::Combine,
                self.limits(),
            )),
            |so_far| {
                events(progress(
                    &Step::Combine,
                    &Count::default(),
                    started,
                    None,
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
        let streamed = streamed?;
        let peaks = match workspace.profile().await {
            Ok(profile) => profile.peaks,
            Err(error) => {
                tracing::warn!(
                    "could not read the run's DuckDB's peaks: {}",
                    describe_error(&error)
                );
                WorkspacePeaks::default()
            }
        };
        events(RunEvent::CombineDone {
            rows: streamed.read.rows,
            bytes: streamed.read.bytes,
            first_row_ms: streamed.read.first_row.map(millis),
            elapsed_ms: millis(started.elapsed()),
            peak_memory: peaks.memory,
            peak_temp_size: peaks.temp_size,
            query_ms: Some(millis(
                starting + streamed.read.waited_first + streamed.read.waited_rest,
            )),
        });
        let mut streamed = streamed;
        if let Err(failure) = self.query_charts(workspace, &mut streamed).await {
            streamed.abandon();
            return Err(failure);
        }
        streamed.finish(events)
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
        };
        let statements = Statements::new(self.outputs.plan, self.outputs.values, source);
        let deadline = opened + source.limits.timeout;
        let batches = match &source.lookup {
            None => {
                let statement = statements.whole(opened);
                loading
                    .fill(self.opener, &statement, workspace, None, events)
                    .await?;
                None
            }
            Some(lookup) => {
                let (values, reading) = timed(lookup_values(lookup, workspace, loaded)).await;
                loading.spent.load += reading;
                let values = values.map_err(|error| error.at(loading.step()))?;
                let batches: Vec<&[Literal]> = values.chunks(lookup.batch_size.get()).collect();
                let of = batches.len() as u64;
                for (index, batch) in batches.into_iter().enumerate() {
                    let statement = statements.batch(batch, deadline);
                    let batch = Batch {
                        number: index as u64 + 1,
                        of,
                    };
                    loading
                        .fill(self.opener, &statement, workspace, Some(batch), events)
                        .await?;
                }
                if of == 0 {
                    loading
                        .load_nothing(&statements, deadline, self.opener, workspace, events)
                        .await?;
                }
                Some(of)
            }
        };
        loading.finish(batches, events).await
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

/// A source's table being loaded, as text, and the types its values say.
struct Table {
    loader: TableLoader,
    typer: ColumnTyper,
}

/// The time a source being loaded spent outside waiting for its rows, which its [`Count`]
/// holds.
#[derive(Default)]
struct Spent {
    connect: Duration,
    /// Each statement's start, up to its cursor; its wait for the first page is in `Count`.
    start: Duration,
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
}

/// A source's progress: what it has read since `opened`, and what it was busy with.
fn progress(
    step: &Step,
    read: &Count,
    opened: Instant,
    batch: Option<Batch>,
    busy: Busy,
) -> RunEvent {
    RunEvent::Progress {
        step: step.clone(),
        rows: read.rows,
        bytes: read.bytes,
        elapsed_ms: millis(opened.elapsed()),
        batch,
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

    /// Runs `statement` and appends its rows to the table, a page at a time, watching the
    /// source's caps. `batch` is which of the lookup's batches it reads.
    async fn fill<O: SourceOpener>(
        &mut self,
        opener: &O,
        statement: &SourceStatement<'_>,
        workspace: &Workspace,
        batch: Option<Batch>,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let step = self.step();
        let (reader, opening_time) = waiting(Box::pin(Shaped::open(opener, statement)), |so_far| {
            let busy = self.clock.lap(so_far);
            (events.borrow_mut())(progress(&step, &self.read, self.opened, batch, busy));
        })
        .await;
        self.clock.waited(opening_time);
        let mut reader = reader.map_err(|error| error.at(step))?;
        let opening = reader.opening();
        self.spent.connect += opening.connect;
        self.spent.start += opening.start;
        let filled = self.fill_from(&mut reader, workspace, batch, events).await;
        reader.close().await;
        filled
    }

    async fn fill_from<R: SourceReader>(
        &mut self,
        reader: &mut R,
        workspace: &Workspace,
        batch: Option<Batch>,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let mut table = self.take_table(reader.columns(), workspace, batch).await?;
        let step = self.step();
        let mut rows_seen = false;
        loop {
            let (page, waited) = waiting(reader.next_page(), |so_far| {
                let busy = self.clock.lap(so_far);
                (events.borrow_mut())(progress(&step, &self.read, self.opened, batch, busy));
            })
            .await;
            self.read.waited(waited, !rows_seen);
            self.clock.waited(waited);
            let Some(page) = page.map_err(|error| error.at(self.step()))? else {
                break;
            };
            if page.is_empty() {
                continue;
            }
            rows_seen = true;
            self.read
                .first_row
                .get_or_insert_with(|| self.opened.elapsed());
            self.read.add(&page);
            self.read.check(self.source)?;
            table.typer.see(&page);
            let (appended, appending) = timed(table.loader.append(page)).await;
            self.spent.load += appending;
            self.clock.loaded(appending);
            if let Err(error) = appended {
                return Err(self.duckdb_failure(&error));
            }
            if self.clock.due() {
                let busy = self.clock.lap(Duration::ZERO);
                (events.borrow_mut())(progress(&step, &self.read, self.opened, batch, busy));
            }
        }
        self.table = Some(table);
        Ok(())
    }

    /// The table for `columns`: made for the first statement, whose columns a later batch's
    /// must be. The caller puts it back once its rows are in.
    async fn take_table(
        &mut self,
        columns: &[ExportColumn],
        workspace: &Workspace,
        batch: Option<Batch>,
    ) -> Result<Table, RunFailure> {
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        if let Some(table) = self.table.take() {
            let first: Vec<&str> = table.typer.names().collect();
            if first == names {
                return Ok(table);
            }
            let number = batch.map_or(1, |batch| batch.number);
            return Err(RunFailure::new(
                self.step(),
                format!(
                    "batch {number} returned the columns {}, unlike the first batch's {}",
                    names.join(", "),
                    first.join(", ")
                ),
            ));
        }
        let loader = workspace
            .loader(&self.source.id, &names)
            .await
            .map_err(|error| self.duckdb_failure(&error))?;
        let typer = ColumnTyper::new(columns, self.source.columns.as_deref());
        Ok(Table { loader, typer })
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
                let table = self.take_table(&columns, workspace, None).await?;
                self.table = Some(table);
                Ok(())
            }
            None => {
                let statement = statements.batch(&[Literal::Null], deadline);
                self.fill(opener, &statement, workspace, None, events).await
            }
        }
    }

    /// Types the table's columns, makes it, and says so; `batches` is how many statements
    /// a lookup sent.
    async fn finish(
        self,
        batches: Option<u64>,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<LoadedTable, RunFailure> {
        let step = self.step();
        let Some(Table { loader, typer }) = self.table else {
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
                .map(|(name, column_type)| LoadedColumn {
                    name: name.clone(),
                    type_name: column_type.to_string(),
                    declared,
                })
                .collect(),
            warnings,
            batches,
            phases: Some(phases),
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
enum DuckDbQuery {
    Combine,
    /// A chart's own query, under the combine step's limits.
    Chart,
}

/// The rows of a query of the run's DuckDB, the combine step's or a chart's, read a page at a
/// time until its timeout.
struct CombineReader<'a> {
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
    async fn start(
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
