//! Running user statements. A DuckDB statement borrows its connection and every fetch blocks,
//! so each statement runs on its own thread from tokio's blocking pool, which fetches rows only
//! when the cursor asks for a page. The thread holds the connection's lock until the cursor is
//! finished, which is what keeps the connection busy meanwhile.
//!
//! Results are read as DuckDB streams them, one Arrow chunk (2,048 rows) at a time, through
//! `Statement::step`. Not through the iterator `stream_arrow` returns, which panics when a
//! fetch fails partway through a result, as a cast error in a later chunk does. A page is a
//! chunk's rows as text; a batch is a chunk as DuckDB handed it over, but for the columns of
//! types a batch doesn't carry, which are text (ADR 0007).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dbrover_core::{
    BatchCursor, CancelHandle, Cell, ColumnInfo, DriverError, Rejection, ResultCursor, Row,
};
use dbrover_sql::{Dialect, first_word};
use duckdb::InterruptHandle;
use duckdb::arrow::array::{Array, AsArray, StructArray};
use duckdb::arrow::datatypes::{DataType, Int64Type, Schema, SchemaRef};
use duckdb::arrow::record_batch::RecordBatch;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::errors::{interrupted, rejection, statement_error};
use crate::{connection_error, poisoned, values};

/// First words of statements whose results are rows to show, even when shaped like a status
/// (a query naming its one column `Count`, say).
const QUERY_WORDS: &[&str] = &[
    "call",
    "describe",
    "explain",
    "from",
    "pivot",
    "pragma",
    "select",
    "show",
    "summarize",
    "table",
    "unpivot",
    "values",
    "with",
];

/// How DuckDB answers a statement that has no rows to show: one `Success` column, without
/// rows (`BEGIN`, `SET`, `DROP`, `ATTACH`), or one `Count` column, holding the number of rows
/// changed or copied (`INSERT`, `UPDATE`, `DELETE`, `COPY`), or no row at all (`CREATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Success,
    Count,
}

impl Status {
    /// The status `sql`'s result stands for, if it is shaped like one and `sql` isn't a query.
    fn of(sql: &str, schema: &Schema) -> Option<Self> {
        let [field] = schema.fields().as_ref() else {
            return None;
        };
        let status = match (field.name().as_str(), field.data_type()) {
            ("Success", DataType::Boolean) => Self::Success,
            ("Count", DataType::Int64) => Self::Count,
            _ => return None,
        };
        let query = first_word(sql, Dialect::DuckDb)
            .is_some_and(|word| QUERY_WORDS.contains(&word.as_str()));
        (!query).then_some(status)
    }
}

/// What the statement thread reports once the statement is running (and, for a change, has
/// counted its rows).
struct Started {
    columns: Vec<ColumnInfo>,
    /// The schema of its rows read as batches.
    batch_schema: SchemaRef,
    rows_changed: Option<u64>,
}

/// What the cursor asks the statement thread for: up to so many rows as text, or as a batch.
#[derive(Debug, Clone, Copy)]
enum Demand {
    Rows(usize),
    Batch(usize),
}

/// What the statement thread fetched for a demand.
enum Fetched {
    Rows(Vec<Row>),
    /// `None` once the result has no more rows.
    Batch(Option<RecordBatch>),
}

struct Page {
    fetched: Fetched,
    exhausted: bool,
}

/// Runs `sql` with `values` bound to its `$1`, `$2`, … placeholders.
pub(crate) async fn execute(
    connection: &Arc<Mutex<duckdb::Connection>>,
    cancel: &DuckDbCancel,
    sql: &str,
    values: &[Cell],
) -> Result<Box<dyn ResultCursor>, DriverError> {
    // A cancel applies to the statement running when it arrives; a new statement starts
    // uncancelled. This runs before the statement thread exists, so a cancel can't slip in
    // between the reset and the statement's start.
    cancel.requested.store(false, Ordering::SeqCst);
    let cancelled = Arc::clone(&cancel.requested);
    let (started_sender, started) = oneshot::channel();
    // Capacity 1: the thread fetches at most one page ahead of what was asked for.
    let (demand, demand_receiver) = mpsc::channel(1);
    let (page_sender, pages) = mpsc::channel(1);
    let connection = Arc::clone(connection);
    let sql = sql.to_owned();
    let values = values.to_vec();
    let thread = tokio::task::spawn_blocking(move || {
        let channels = Channels {
            started: started_sender,
            demand: demand_receiver,
            pages: page_sender,
        };
        run_statement(&connection, &sql, &values, &cancelled, channels);
    });

    let started = started
        .await
        .map_err(|_| connection_error("the statement thread stopped unexpectedly"))??;
    let exhausted = started.columns.is_empty();
    Ok(Box::new(DuckDbCursor {
        columns: started.columns,
        batch_schema: started.batch_schema,
        rows_changed: started.rows_changed,
        exhausted,
        demand: Some(demand),
        pages,
        thread,
    }))
}

/// How the statement thread talks to its cursor.
struct Channels {
    started: oneshot::Sender<Result<Started, DriverError>>,
    demand: mpsc::Receiver<Demand>,
    pages: mpsc::Sender<Result<Page, DriverError>>,
}

/// The statement thread: prepares and starts the statement, then fetches one page per demand
/// until the result is exhausted, the statement is cancelled, or the cursor goes away.
fn run_statement(
    connection: &Mutex<duckdb::Connection>,
    sql: &str,
    values: &[Cell],
    cancelled: &AtomicBool,
    channels: Channels,
) {
    let Channels {
        started,
        mut demand,
        pages,
    } = channels;
    let Ok(connection) = connection.lock() else {
        let _ = started.send(Err(poisoned()));
        return;
    };
    let mut statement = match connection.prepare(sql) {
        Ok(statement) => statement,
        Err(error) => {
            // The receiver only disappears if the cursor's caller gave up; nothing to do.
            let _ = started.send(Err(statement_error(error, sql)));
            return;
        }
    };
    // Only to start the statement streaming: its iterator is dropped unused (see the module).
    let values = duckdb::params_from_iter(values.iter().map(Option::as_deref));
    if let Err(error) = statement.stream_arrow(values).map(drop) {
        let _ = started.send(Err(statement_error(error, sql)));
        return;
    }
    let schema = statement.schema();
    if let Some(status) = Status::of(sql, &schema) {
        // A streaming statement runs as its result is fetched, so this is also what runs it.
        let ran = count(&statement)
            .map(|count| Started {
                columns: Vec::new(),
                batch_schema: Arc::new(Schema::empty()),
                rows_changed: count.filter(|_| status == Status::Count),
            })
            .map_err(|error| statement_error(error, sql));
        let _ = started.send(ran);
        return;
    }
    let columns = values::column_info(&schema);
    let batch_schema = values::batch_schema(&schema);
    let reported = started.send(Ok(Started {
        columns,
        batch_schema: Arc::clone(&batch_schema),
        rows_changed: None,
    }));
    if reported.is_ok() {
        let fetch = Fetch {
            statement: &statement,
            sql,
            cancelled,
            batch_schema,
        };
        fetch.pages(&mut demand, &pages);
    }
}

/// Steps a status result to its end, which is what runs its statement, and returns the
/// count in its first row, if it has one.
fn count(statement: &duckdb::Statement<'_>) -> duckdb::Result<Option<u64>> {
    let mut count = None;
    while let Some(chunk) = statement.step()? {
        count = count.or_else(|| first_count(&chunk));
    }
    Ok(count)
}

fn first_count(chunk: &StructArray) -> Option<u64> {
    let column = chunk.columns().first()?;
    if column.data_type() != &DataType::Int64 || column.is_empty() || column.is_null(0) {
        return None;
    }
    u64::try_from(column.as_primitive::<Int64Type>().value(0)).ok()
}

/// Reads a running statement's rows a page or a batch at a time.
struct Fetch<'a> {
    statement: &'a duckdb::Statement<'a>,
    sql: &'a str,
    cancelled: &'a AtomicBool,
    batch_schema: SchemaRef,
}

impl Fetch<'_> {
    fn pages(
        &self,
        demand: &mut mpsc::Receiver<Demand>,
        pages: &mpsc::Sender<Result<Page, DriverError>>,
    ) {
        // The chunk being read, and the next of its rows to read.
        let mut chunk: Option<(StructArray, usize)> = None;
        while let Some(wanted) = demand.blocking_recv() {
            let page = match wanted {
                Demand::Rows(max_rows) => self.page(&mut chunk, max_rows),
                Demand::Batch(max_rows) => self.batch(&mut chunk, max_rows),
            };
            let ended = !matches!(
                page,
                Ok(Page {
                    exhausted: false,
                    ..
                })
            );
            if pages.blocking_send(page).is_err() || ended {
                return;
            }
        }
    }

    /// The chunk to read next: what is left of `chunk`, or DuckDB's next; `None` once the
    /// result is exhausted.
    fn next_chunk(
        &self,
        chunk: &mut Option<(StructArray, usize)>,
    ) -> Result<Option<(StructArray, usize)>, DriverError> {
        // DuckDB's interrupt stops a fetch in progress; this catches a cancel that arrived
        // between fetches, or while rows were being sent.
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(interrupted());
        }
        match chunk.take() {
            Some((current, next_row)) if next_row < current.len() => Ok(Some((current, next_row))),
            _ => match self.statement.step() {
                Ok(Some(next)) => Ok(Some((next, 0))),
                Ok(None) => Ok(None),
                Err(error) => Err(statement_error(error, self.sql)),
            },
        }
    }

    fn page(
        &self,
        chunk: &mut Option<(StructArray, usize)>,
        max_rows: usize,
    ) -> Result<Page, DriverError> {
        let mut rows = Vec::new();
        while rows.len() < max_rows {
            let Some((current, next_row)) = self.next_chunk(chunk)? else {
                return Ok(Page {
                    fetched: Fetched::Rows(rows),
                    exhausted: true,
                });
            };
            let wanted = (max_rows - rows.len()).min(current.len() - next_row);
            rows.extend(values::rows(&current, next_row..next_row + wanted));
            *chunk = Some((current, next_row + wanted));
        }
        Ok(Page {
            fetched: Fetched::Rows(rows),
            exhausted: false,
        })
    }

    /// Up to `max_rows` rows of one chunk, as a batch: never more than a chunk, so DuckDB's
    /// arrays are handed on without being copied into larger ones.
    fn batch(
        &self,
        chunk: &mut Option<(StructArray, usize)>,
        max_rows: usize,
    ) -> Result<Page, DriverError> {
        let (current, next_row) = loop {
            match self.next_chunk(chunk)? {
                None => {
                    return Ok(Page {
                        fetched: Fetched::Batch(None),
                        exhausted: true,
                    });
                }
                Some((current, _)) if current.is_empty() => {}
                Some(found) => break found,
            }
        };
        let wanted = max_rows.max(1).min(current.len() - next_row);
        let batch = values::batch(&current, &self.batch_schema, next_row..next_row + wanted)
            .map_err(connection_error)?;
        *chunk = Some((current, next_row + wanted));
        Ok(Page {
            fetched: Fetched::Batch(Some(batch)),
            exhausted: false,
        })
    }
}

/// Prepares `sql` without running it: DuckDB parses it and binds its names, as it would to
/// run it.
/// What preparing `sql` finds wrong with it, if anything.
///
/// A statement naming a remote address (`s3://…`, `https://…`) isn't prepared: DuckDB binds a
/// file's columns by reading it, so checking it as it is typed would send a request to the
/// bucket at every pause, and could start an extension's download. Its errors show when it
/// runs.
pub(crate) fn validate(
    connection: &duckdb::Connection,
    sql: &str,
) -> Result<Option<Rejection>, DriverError> {
    if names_remote_address(sql) {
        return Ok(None);
    }
    match connection.prepare(sql) {
        Ok(_) => Ok(None),
        Err(error) => rejection(error, sql).map(Some),
    }
}

fn names_remote_address(sql: &str) -> bool {
    sql.contains("://")
}

struct DuckDbCursor {
    columns: Vec<ColumnInfo>,
    batch_schema: SchemaRef,
    rows_changed: Option<u64>,
    exhausted: bool,
    /// Dropping it tells the statement thread to stop and release the connection.
    demand: Option<mpsc::Sender<Demand>>,
    pages: mpsc::Receiver<Result<Page, DriverError>>,
    thread: JoinHandle<()>,
}

impl DuckDbCursor {
    /// What the statement thread fetches for `wanted`; `None` once the result is exhausted.
    async fn fetch(&mut self, wanted: Demand) -> Result<Option<Fetched>, DriverError> {
        let Some(demand) = self.demand.as_ref().filter(|_| !self.exhausted) else {
            return Ok(None);
        };
        if demand.send(wanted).await.is_err() {
            self.exhausted = true;
            return Ok(None);
        }
        match self.pages.recv().await {
            Some(Ok(page)) => {
                self.exhausted = page.exhausted;
                Ok(Some(page.fetched))
            }
            Some(Err(error)) => {
                self.exhausted = true;
                Err(error)
            }
            None => {
                self.exhausted = true;
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl ResultCursor for DuckDbCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        match self.fetch(Demand::Rows(max_rows)).await? {
            Some(Fetched::Rows(rows)) => Ok(rows),
            Some(Fetched::Batch(_)) | None => Ok(Vec::new()),
        }
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// DuckDB hands every result over as Arrow, so every cursor has batches.
    fn batches(&mut self) -> Option<&mut dyn BatchCursor> {
        Some(self)
    }

    /// Stops the statement thread and waits for it, so the connection is free again.
    async fn finish(mut self: Box<Self>) -> Result<Option<u64>, DriverError> {
        self.demand = None;
        self.pages.close();
        (&mut self.thread).await.map_err(connection_error)?;
        Ok(self.rows_changed)
    }
}

#[async_trait]
impl BatchCursor for DuckDbCursor {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.batch_schema)
    }

    async fn next_batch(&mut self, max_rows: usize) -> Result<Option<RecordBatch>, DriverError> {
        match self.fetch(Demand::Batch(max_rows)).await? {
            Some(Fetched::Batch(batch)) => Ok(batch),
            Some(Fetched::Rows(_)) | None => Ok(None),
        }
    }
}

/// Stops the running statement: DuckDB's interrupt for a fetch in progress, plus a flag the
/// statement thread checks between fetches, for a cancel that lands between them.
pub(crate) struct DuckDbCancel {
    interrupt: Arc<InterruptHandle>,
    requested: Arc<AtomicBool>,
}

impl DuckDbCancel {
    pub(crate) fn new(interrupt: Arc<InterruptHandle>) -> Self {
        Self {
            interrupt,
            requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl CancelHandle for DuckDbCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.requested.store(true, Ordering::SeqCst);
        self.interrupt.interrupt();
        Ok(())
    }
}
