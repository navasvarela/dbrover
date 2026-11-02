//! A run with a combine step (`docs/reporting.md`, "RP2 — Several sources"):
//!
//! 1. open the run's DuckDB, with its temporary folder in the run's staging folder;
//! 2. attach each DuckDB file, and load every other source at the same time, each under its
//!    caps, typing its columns once its last row is in;
//! 3. lock the database down, and run the combine query under its timeout, its rows going to
//!    the outputs as a source's do without a combine step.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbrover_core::{CancelHandle, Connection, DriverError, ResultCursor, Row, describe_error};
use dbrover_driver_duckdb::{TableLoader, Workspace, WorkspaceLimit, WorkspaceLimits};
use dbrover_export::ExportColumn;
use dbrover_sql::Dialect;
use futures_util::future::try_join_all;

use crate::run::{Count, Feed, Outputs, PROGRESS_EVERY, millis};
use crate::typing::ColumnTyper;
use crate::{
    AttachedSource, CombineLimits, Combined, LimitHit, LoadedColumn, PlannedSource, QuerySource,
    RunEvent, RunFailure, RunSummary, RunValues, SourceError, SourceOpener, SourceReader, Step,
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

impl<O: SourceOpener> Combining<'_, O> {
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
        {
            let events = RefCell::new(&mut *events);
            let loads = self
                .combined
                .sources
                .iter()
                .filter_map(|source| match source {
                    PlannedSource::Query(source) => Some(self.load(source, workspace, &events)),
                    PlannedSource::Attached(_) => None,
                });
            try_join_all(loads).await?;
        }
        workspace
            .lock()
            .await
            .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;

        let started = Instant::now();
        let query = &self.combined.combine.query;
        let mut reader = CombineReader::start(workspace, query, self.outputs.values, self.limits())
            .await
            .map_err(|error| error.at(Step::Combine))?;
        let written = self
            .outputs
            .write(&mut reader, &Feed::Combine, started, events)
            .await;
        reader.close().await;
        written
    }

    /// Reads `source` into its table under its caps, then types its columns.
    async fn load(
        &self,
        source: &QuerySource,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let loading = Loading {
            source,
            limits: self.limits(),
            opened: Instant::now(),
        };
        let mut reader = self
            .opener
            .open(source, &self.outputs.values.parameters)
            .await
            .map_err(|error| error.at(loading.step()))?;
        let filled = loading.fill(&mut reader, workspace, events).await;
        reader.close().await;
        let Filled {
            loader,
            typer,
            read,
        } = filled?;
        let (types, warnings) = typer.types();
        let table = typer
            .names()
            .zip(&types)
            .map(|(name, column_type)| LoadedColumn {
                name: name.to_owned(),
                type_name: column_type.to_string(),
            })
            .collect();
        loader
            .finish(&types)
            .await
            .map_err(|error| loading.duckdb_failure(&error))?;
        (events.borrow_mut())(RunEvent::SourceDone {
            id: source.id.clone(),
            rows: read.rows,
            bytes: read.bytes,
            first_row_ms: read.first_row.map(millis),
            elapsed_ms: millis(loading.opened.elapsed()),
            table,
            warnings,
        });
        Ok(())
    }
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

/// A source whose rows are all in its table, as text.
struct Filled {
    loader: TableLoader,
    typer: ColumnTyper,
    read: Count,
}

/// A source being loaded into its table.
struct Loading<'a> {
    source: &'a QuerySource,
    limits: &'a CombineLimits,
    /// When the source started to be opened, for the time to its first row.
    opened: Instant,
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

    /// Appends `reader`'s rows to a new table, a page at a time, watching the source's caps.
    async fn fill<R: SourceReader>(
        &self,
        reader: &mut R,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<Filled, RunFailure> {
        let names: Vec<String> = reader
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let mut loader = workspace
            .loader(&self.source.id, &names)
            .await
            .map_err(|error| self.duckdb_failure(&error))?;
        let mut typer = ColumnTyper::new(reader.columns());
        let mut read = Count::default();
        let mut last_progress = Instant::now();
        while let Some(page) = reader
            .next_page()
            .await
            .map_err(|error| error.at(self.step()))?
        {
            if page.is_empty() {
                continue;
            }
            read.first_row.get_or_insert_with(|| self.opened.elapsed());
            read.add(&page);
            read.check(self.source)?;
            typer.see(&page);
            loader
                .append(page)
                .await
                .map_err(|error| self.duckdb_failure(&error))?;
            if last_progress.elapsed() >= PROGRESS_EVERY {
                last_progress = Instant::now();
                (events.borrow_mut())(RunEvent::Progress {
                    step: self.step(),
                    rows: read.rows,
                    bytes: read.bytes,
                    elapsed_ms: millis(self.opened.elapsed()),
                });
            }
        }
        Ok(Filled {
            loader,
            typer,
            read,
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
        })),
    }
}

/// The combine query's rows, read a page at a time until its timeout.
struct CombineReader<'a> {
    cursor: Option<Box<dyn ResultCursor>>,
    columns: Vec<ExportColumn>,
    cancel: Arc<dyn CancelHandle>,
    deadline: Instant,
    limits: &'a CombineLimits,
}

impl<'a> CombineReader<'a> {
    /// Starts `query` with `values` bound, in the locked-down workspace.
    async fn start(
        workspace: &mut Workspace,
        query: &str,
        values: &RunValues,
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
        let cursor = watch(started, deadline, &cancel, limits).await?;
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
        let page = watch(page, self.deadline, &self.cancel, self.limits).await?;
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
    Err(SourceError {
        message: format!(
            "the combine query ran past {value}, its timeout, and was interrupted; combine fewer \
             rows, or raise timeout"
        ),
        limit: Some(Box::new(LimitHit {
            key: "timeout".to_owned(),
            value,
            raise_in: "[combine]".to_owned(),
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
