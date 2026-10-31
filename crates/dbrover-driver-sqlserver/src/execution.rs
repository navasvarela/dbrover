//! Running user statements, and cancelling them.
//!
//! `tiberius` borrows the client for as long as a result is open, so each statement runs on
//! its own task that owns the client's lock and reads rows only when the cursor asks for a
//! page, as the MySQL driver's statement task does. The task keeps the lock until the cursor
//! is finished, which is what keeps the connection busy meanwhile.
//!
//! Cancelling uses TDS's own attention message, which any login may send, rather than `KILL`
//! from a second connection, which needs a permission most logins lack. Sending it needs the
//! client itself, which the running statement's result stream borrows, so a cancel is a
//! signal to the statement task: the task stops reading, drops the stream, and sends the
//! attention (`Client::cancel_query`), which also reads the server's acknowledgement and
//! leaves the connection ready for the next statement.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use dbrover_core::{CancelHandle, Cell, ColumnInfo, DriverError, Rejection, ResultCursor, Row};
use dbrover_sql::{Dialect, first_word};
use futures_util::TryStreamExt;
use tiberius::{Query, QueryItem, QueryStream};
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::errors::{rejection, statement_error};
use crate::{SQL_SERVER_QUOTING, SqlClient, column, connection_error, values};

/// How long a read already under way may go on after a cancel, before the statement has
/// answered, before the read is dropped. Nothing of the reply has been read yet, so dropping
/// the read loses nothing, and the attention goes out at once. 100 ms was chosen as long
/// against a network round trip and short against a person waiting.
const GRACE_BEFORE_ROWS: Duration = Duration::from_millis(100);

/// How long a read already under way may go on after a cancel, once rows are arriving.
///
/// Dropping the result stream halfway through a row loses the half already read, and the
/// attention's acknowledgement can then not be found in what follows: tiberius reads the
/// rest of the row as tokens ("invalid token type 61") and the connection is written off.
/// While rows arrive, a read is waiting halfway through one far more often than not: SQL
/// Server sends a 4 KB packet once it is full, so most packet boundaries split a row, and a
/// result that comes slowly (a sparse filter, a loaded server) leaves the client holding
/// half a row until the next packet. With 100 ms here, cancelling such a result lost the
/// connection 7 runs in 8, and CI's cancel test failed now and then on a busy runner.
///
/// So the read is given until the next packet arrives, which completes the row and stops
/// the stream between two rows. 10 s bounds the wait for a result so sparse that a packet
/// takes longer to fill; dropping the read then leaves the connection out of step, as
/// before, rather than leave ⌘. hanging.
const GRACE_WITHIN_ROWS: Duration = Duration::from_secs(10);

/// Where a cancel reaches the running statement: the sender of a fresh channel per
/// statement, so a cancel never outlives the statement it was meant for.
#[derive(Default)]
pub(crate) struct CancelSlot {
    running: std::sync::Mutex<Option<watch::Sender<bool>>>,
    /// Set when a cancel's acknowledgement never arrived: the server's reply to the next
    /// statement could then be taken for the reply to the one after it, so the connection
    /// refuses all further use rather than ever show a statement's result for another's.
    out_of_step: AtomicBool,
}

impl CancelSlot {
    /// Fails if a lost acknowledgement has left the connection out of step with the server.
    pub(crate) fn ensure_in_step(&self) -> Result<(), DriverError> {
        if self.out_of_step.load(Ordering::Acquire) {
            return Err(connection_error(
                "the connection was left out of step with the server by a cancel whose \
                 acknowledgement never arrived; reconnect to go on",
            ));
        }
        Ok(())
    }

    fn arm(&self) -> Cancellation {
        let (sender, receiver) = watch::channel(false);
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        Cancellation(receiver)
    }

    fn disarm(&self) {
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[async_trait]
impl CancelHandle for CancelSlot {
    /// Signals the statement task, which sends the attention itself: returns at once, and
    /// does nothing when no statement is running.
    async fn cancel(&self) -> Result<(), DriverError> {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = running.as_ref() {
            sender.send_replace(true);
        }
        Ok(())
    }
}

/// The statement task's end of a [`CancelSlot`].
struct Cancellation(watch::Receiver<bool>);

impl Cancellation {
    /// Completes once a cancel is requested; never if the slot is disarmed first.
    async fn requested(&mut self) {
        if self.0.wait_for(|cancelled| *cancelled).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Why a statement stopped early, when it was cancelled.
#[derive(Debug, thiserror::Error)]
enum CancelError {
    #[error("the statement was cancelled")]
    Cancelled,
    #[error("the statement was cancelled, but the server's acknowledgement was lost")]
    Unacknowledged(#[source] tiberius::error::Error),
}

/// The column SQL Server names the actual plan it sends under `STATISTICS XML`.
const SHOWPLAN_COLUMN: &str = "Microsoft SQL Server 2005 XML Showplan";

/// What a statement's reply is read for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The rows of its first result; a batch's further results are read and dropped.
    Rows,
    /// The actual plan SQL Server sends after the statement's own results while the session's
    /// `STATISTICS XML` is on. The statement's results are read and dropped, and the plan is
    /// the one row of one value it answers with, as `SHOWPLAN_XML`'s estimated plan is.
    ActualPlan,
}

/// Whether `sql` switches the session's `STATISTICS XML` on (`Some(true)`) or off. Only the
/// statement as DbRover's Explain analyse sends it is recognised; one that switches other
/// statistics with it is not.
pub(crate) fn statistics_xml_switch(sql: &str) -> Option<bool> {
    let words: Vec<String> = sql
        .split_whitespace()
        .map(|word| word.trim_end_matches(';').to_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    match words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["set", "statistics", "xml", "on"] => Some(true),
        ["set", "statistics", "xml", "off"] => Some(false),
        _ => None,
    }
}

/// What the statement task reports once the server has answered: the result's columns, or,
/// for a statement without rows, how many it changed.
struct Started {
    columns: Vec<ColumnInfo>,
    rows_changed: Option<u64>,
}

struct Page {
    rows: Vec<Row>,
    exhausted: bool,
}

/// How the statement task talks to its cursor.
struct Channels {
    started: oneshot::Sender<Result<Started, DriverError>>,
    demand: mpsc::Receiver<usize>,
    pages: mpsc::Sender<Result<Page, DriverError>>,
}

/// Runs `sql`, with `values` bound to its `@P1`, `@P2`, … placeholders if there are any, and
/// reads its reply for `answer`.
pub(crate) async fn execute(
    client: &Arc<Mutex<SqlClient>>,
    cancel: &Arc<CancelSlot>,
    sql: &str,
    values: &[Cell],
    answer: Answer,
) -> Result<Box<dyn ResultCursor>, DriverError> {
    let client = Arc::clone(client).lock_owned().await;
    // Armed before the statement is sent, so a cancel that arrives as soon as it is running
    // isn't lost.
    let cancellation = cancel.arm();
    let (started_sender, started) = oneshot::channel();
    // Capacity 1: the task reads at most one page ahead of what was asked for.
    let (demand, demand_receiver) = mpsc::channel(1);
    let (page_sender, pages) = mpsc::channel(1);
    let channels = Channels {
        started: started_sender,
        demand: demand_receiver,
        pages: page_sender,
    };
    let task = tokio::spawn(run_statement(
        client,
        Statement {
            sql: sql.to_owned(),
            values: values.to_vec(),
            answer,
        },
        channels,
        Arc::clone(cancel),
        cancellation,
    ));

    let started = started
        .await
        .map_err(|_| connection_error("the statement task stopped unexpectedly"))??;
    let exhausted = started.columns.is_empty();
    Ok(Box::new(SqlServerCursor {
        columns: started.columns,
        rows_changed: started.rows_changed,
        exhausted,
        demand: Some(demand),
        pages,
        task,
    }))
}

/// Where the error a cancel causes is reported: to whoever is waiting on the statement at
/// that point.
enum Report {
    Started(oneshot::Sender<Result<Started, DriverError>>),
    Page(mpsc::Sender<Result<Page, DriverError>>),
    /// The cursor's [`finish`](ResultCursor::finish), through the task's result.
    Finish,
}

enum Outcome {
    /// The statement ran to its end, or failed; what is left to report is the task's result.
    Ended(Result<(), DriverError>),
    Cancelled(Report),
}

/// The statement task: runs `sql`, then reads one page per demand until the result is
/// exhausted, fails, is cancelled, or the cursor goes away. Whatever is left is read and
/// discarded, so the connection is ready for the next statement, and how that went is the
/// task's result, which [`SqlServerCursor::finish`] reports.
async fn run_statement(
    mut client: OwnedMutexGuard<SqlClient>,
    statement: Statement,
    channels: Channels,
    slot: Arc<CancelSlot>,
    mut cancellation: Cancellation,
) -> Result<(), DriverError> {
    let outcome = read_statement(&mut client, statement, channels, &mut cancellation).await;
    slot.disarm();
    let report = match outcome {
        Outcome::Ended(result) => return result,
        Outcome::Cancelled(report) => report,
    };
    let source = match client.cancel_query().await {
        Ok(()) => CancelError::Cancelled,
        Err(error) => {
            slot.out_of_step.store(true, Ordering::Release);
            CancelError::Unacknowledged(error)
        }
    };
    let error = DriverError::Statement {
        source: Box::new(source),
        position: None,
        sqlstate: None,
    };
    // The receivers only disappear if the cursor's caller gave up; nothing to do then.
    match report {
        Report::Started(started) => {
            let _ = started.send(Err(error));
            Ok(())
        }
        Report::Page(pages) => {
            let _ = pages.send(Err(error)).await;
            Ok(())
        }
        Report::Finish => Err(error),
    }
}

/// A statement to run, the values for its placeholders, and what its reply is read for.
struct Statement {
    sql: String,
    values: Vec<Cell>,
    answer: Answer,
}

impl Statement {
    /// Sends the statement: as a batch of its own, or, with values, through `sp_executesql`,
    /// whose scope ends with the statement (a `#temp` table it creates, a `USE` or a `SET`
    /// it runs, don't outlast it).
    async fn send<'a>(&self, client: &'a mut SqlClient) -> tiberius::Result<QueryStream<'a>> {
        if self.values.is_empty() {
            return client.simple_query(self.sql.as_str()).await;
        }
        let mut query = Query::new(self.sql.as_str());
        for value in &self.values {
            query.bind(value.as_deref());
        }
        query.query(client).await
    }
}

async fn read_statement(
    client: &mut SqlClient,
    statement: Statement,
    channels: Channels,
    cancellation: &mut Cancellation,
) -> Outcome {
    let sql = statement.sql.as_str();
    // Sending waits for the statement's first result set, or the end of its reply: a
    // statement that takes long before it answers is cancelled here.
    let mut stream =
        match until_cancelled(statement.send(client), GRACE_BEFORE_ROWS, cancellation).await {
            Next::Cancelled => return Outcome::Cancelled(Report::Started(channels.started)),
            Next::Done(Ok(stream)) => stream,
            Next::Done(Err(error)) => {
                let _ = channels.started.send(Err(statement_error(error, sql)));
                return Outcome::Ended(Ok(()));
            }
        };
    if statement.answer == Answer::ActualPlan {
        return read_actual_plan(stream, sql, channels, cancellation).await;
    }
    let Channels {
        started,
        demand,
        pages,
    } = channels;
    let channels = PageChannels { demand, pages };
    let columns = match until_cancelled(stream.columns(), GRACE_BEFORE_ROWS, cancellation).await {
        Next::Cancelled => return Outcome::Cancelled(Report::Started(started)),
        Next::Done(Err(error)) => {
            let _ = started.send(Err(statement_error(error, sql)));
            return Outcome::Ended(Ok(()));
        }
        Next::Done(Ok(columns)) => columns.map(<[tiberius::Column]>::to_vec),
    };
    let Some(columns) = columns else {
        // Without a result set, the reply has been read to its end, so the client is free to
        // ask how many rows the statement changed. A `SET` changes none, and the one that
        // matters most, `SET SHOWPLAN_XML ON`, would have the question explained rather than
        // answered.
        drop(stream);
        let is_set = first_word(sql, Dialect::SqlServer).as_deref() == Some("set");
        let counted = if is_set {
            Ok(None)
        } else {
            rows_changed(client).await
        };
        let finished = counted.map(|rows_changed| Started {
            columns: Vec::new(),
            rows_changed,
        });
        let _ = started.send(finished);
        return Outcome::Ended(Ok(()));
    };
    let column_info = columns.iter().map(values::column_info).collect();
    let cursor_waits = started
        .send(Ok(Started {
            columns: column_info,
            rows_changed: None,
        }))
        .is_ok();
    if cursor_waits && let Some(ended) = serve_pages(&mut stream, sql, channels, cancellation).await
    {
        return ended;
    }
    discard_rest(&mut stream, sql, cancellation).await
}

/// Reads a statement's whole reply under `STATISTICS XML`, keeping the first actual plan in
/// it, and answers with that plan as one row, or with no rows if there was none (a `SET`).
async fn read_actual_plan(
    mut stream: QueryStream<'_>,
    sql: &str,
    channels: Channels,
    cancellation: &mut Cancellation,
) -> Outcome {
    let Channels {
        started,
        mut demand,
        pages,
    } = channels;
    let mut plan: Option<(Vec<ColumnInfo>, Row)> = None;
    let mut plan_columns: Option<Vec<ColumnInfo>> = None;
    loop {
        match until_cancelled(stream.try_next(), GRACE_WITHIN_ROWS, cancellation).await {
            Next::Cancelled => return Outcome::Cancelled(Report::Started(started)),
            Next::Done(Ok(Some(QueryItem::Metadata(metadata)))) => {
                let is_plan = metadata
                    .columns()
                    .first()
                    .is_some_and(|column| column.name() == SHOWPLAN_COLUMN);
                plan_columns = (is_plan && plan.is_none())
                    .then(|| metadata.columns().iter().map(values::column_info).collect());
            }
            Next::Done(Ok(Some(QueryItem::Row(row)))) => {
                if let Some(columns) = plan_columns.take() {
                    plan = Some((columns, values::cells(row)));
                }
            }
            Next::Done(Ok(None)) => break,
            Next::Done(Err(error)) => {
                let _ = started.send(Err(statement_error(error, sql)));
                return Outcome::Ended(Ok(()));
            }
        }
    }
    drop(stream);
    let Some((columns, row)) = plan else {
        let _ = started.send(Ok(Started {
            columns: Vec::new(),
            rows_changed: None,
        }));
        return Outcome::Ended(Ok(()));
    };
    let cursor_waits = started
        .send(Ok(Started {
            columns,
            rows_changed: None,
        }))
        .is_ok();
    if cursor_waits && demand.recv().await.is_some() {
        let _ = pages
            .send(Ok(Page {
                rows: vec![row],
                exhausted: true,
            }))
            .await;
    }
    Outcome::Ended(Ok(()))
}

/// The cursor's half of [`Channels`], once the statement has started.
struct PageChannels {
    demand: mpsc::Receiver<usize>,
    pages: mpsc::Sender<Result<Page, DriverError>>,
}

/// Reads one page per demand, until the first result set is exhausted or the cursor stops
/// asking. Returns how the statement ended if it ended here, by failing or being cancelled.
async fn serve_pages(
    stream: &mut QueryStream<'_>,
    sql: &str,
    channels: PageChannels,
    cancellation: &mut Cancellation,
) -> Option<Outcome> {
    let PageChannels { mut demand, pages } = channels;
    // `columns` only peeked at the first result set's metadata, which the stream yields
    // before its rows.
    let mut in_first_result = false;
    loop {
        let max_rows = tokio::select! {
            biased;
            () = cancellation.requested() => return Some(Outcome::Cancelled(Report::Finish)),
            demand = demand.recv() => demand?,
        };
        let mut page = Vec::new();
        let mut exhausted = false;
        while page.len() < max_rows {
            match until_cancelled(stream.try_next(), GRACE_WITHIN_ROWS, cancellation).await {
                Next::Cancelled => return Some(Outcome::Cancelled(Report::Page(pages))),
                Next::Done(Ok(Some(QueryItem::Row(row)))) => page.push(values::cells(row)),
                Next::Done(Ok(Some(QueryItem::Metadata(_)))) if !in_first_result => {
                    in_first_result = true;
                }
                // The first result set ended: a batch's further results aren't shown.
                Next::Done(Ok(Some(QueryItem::Metadata(_)) | None)) => {
                    exhausted = true;
                    break;
                }
                Next::Done(Err(error)) => {
                    let _ = pages.send(Err(statement_error(error, sql))).await;
                    return Some(Outcome::Ended(Ok(())));
                }
            }
        }
        let sent = pages
            .send(Ok(Page {
                rows: page,
                exhausted,
            }))
            .await;
        if sent.is_err() || exhausted {
            return None;
        }
    }
}

/// Reads and drops the rows the cursor never asked for, and any further result sets, so the
/// connection is ready for the next statement.
async fn discard_rest(
    stream: &mut QueryStream<'_>,
    sql: &str,
    cancellation: &mut Cancellation,
) -> Outcome {
    loop {
        match until_cancelled(stream.try_next(), GRACE_WITHIN_ROWS, cancellation).await {
            Next::Cancelled => return Outcome::Cancelled(Report::Finish),
            Next::Done(Ok(Some(_))) => {}
            Next::Done(Ok(None)) => return Outcome::Ended(Ok(())),
            Next::Done(Err(error)) => return Outcome::Ended(Err(statement_error(error, sql))),
        }
    }
}

enum Next<T> {
    Done(T),
    Cancelled,
}

/// Waits for `work`, unless a cancel is requested first: then it is given `grace` to finish
/// what it is reading, and dropped either way.
async fn until_cancelled<F: Future>(
    work: F,
    grace: Duration,
    cancellation: &mut Cancellation,
) -> Next<F::Output> {
    tokio::pin!(work);
    // The cancel first: rows already buffered keep `work` ready, which would otherwise win
    // every time.
    tokio::select! {
        biased;
        () = cancellation.requested() => {}
        output = &mut work => return Next::Done(output),
    }
    // Whatever arrives in the grace period is discarded: the point is only to stop between
    // tokens.
    let _ = tokio::time::timeout(grace, work).await;
    Next::Cancelled
}

/// How many rows the batch just run changed (or selected, for statements that only assign
/// variables). `@@ROWCOUNT` survives into the next batch, which TDS's own row counts would
/// make unnecessary, but `tiberius` keeps those to itself for a batch read as a stream.
async fn rows_changed(client: &mut SqlClient) -> Result<Option<u64>, DriverError> {
    let row = client
        .simple_query("SELECT CAST(@@ROWCOUNT AS bigint)")
        .await
        .map_err(connection_error)?
        .into_row()
        .await
        .map_err(connection_error)?;
    Ok(row
        .and_then(|row| column::<i64>(&row, 0))
        .and_then(|count| u64::try_from(count).ok()))
}

/// Runs `sql` for its effect alone, reading and discarding whatever it returns.
pub(crate) async fn run_quietly(client: &mut SqlClient, sql: &str) -> Result<(), DriverError> {
    client
        .simple_query(sql)
        .await
        .map_err(connection_error)?
        .into_results()
        .await
        .map_err(connection_error)?;
    Ok(())
}

/// Has the server describe the first result `sql` would return, which compiles it and
/// resolves its names without running it, with the session's temporary tables in view.
///
/// Not `SET NOEXEC ON`, which compiles as well but defers resolving the names of tables that
/// don't exist (yet), so a misspelt table would pass.
pub(crate) async fn validate(
    client: &mut SqlClient,
    sql: &str,
) -> Result<Option<Rejection>, DriverError> {
    let probe = format!(
        "EXEC sp_describe_first_result_set @tsql = {}",
        SQL_SERVER_QUOTING.string_literal(sql)
    );
    let described = match client.simple_query(probe).await {
        Ok(stream) => stream.into_results().await,
        Err(error) => Err(error),
    };
    match described {
        Ok(_) => Ok(None),
        Err(tiberius::error::Error::Server(error)) => Ok(Some(rejection(&error, sql))),
        Err(error) => Err(connection_error(error)),
    }
}

struct SqlServerCursor {
    columns: Vec<ColumnInfo>,
    rows_changed: Option<u64>,
    exhausted: bool,
    /// Dropping it tells the statement task to discard the rest and release the connection.
    demand: Option<mpsc::Sender<usize>>,
    pages: mpsc::Receiver<Result<Page, DriverError>>,
    task: JoinHandle<Result<(), DriverError>>,
}

#[async_trait]
impl ResultCursor for SqlServerCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let Some(demand) = self.demand.as_ref().filter(|_| !self.exhausted) else {
            return Ok(Vec::new());
        };
        if demand.send(max_rows).await.is_err() {
            self.exhausted = true;
            return Ok(Vec::new());
        }
        match self.pages.recv().await {
            Some(Ok(page)) => {
                self.exhausted = page.exhausted;
                Ok(page.rows)
            }
            Some(Err(error)) => {
                self.exhausted = true;
                Err(error)
            }
            None => {
                self.exhausted = true;
                Ok(Vec::new())
            }
        }
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Stops the statement task and waits for it, so the connection is free again. Reports
    /// an error the server sent while the rest of the result was discarded, a cancel's
    /// included.
    async fn finish(mut self: Box<Self>) -> Result<Option<u64>, DriverError> {
        self.demand = None;
        self.pages.close();
        (&mut self.task).await.map_err(connection_error)??;
        Ok(self.rows_changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_all_use_once_out_of_step() {
        let slot = CancelSlot::default();
        assert!(slot.ensure_in_step().is_ok());

        slot.out_of_step.store(true, Ordering::Release);

        assert!(matches!(
            slot.ensure_in_step(),
            Err(DriverError::Connection(_))
        ));
    }

    #[test]
    fn recognises_the_statements_that_switch_statistics_xml() {
        let cases = [
            ("SET STATISTICS XML ON", Some(true)),
            ("set  statistics xml off;", Some(false)),
            ("SET STATISTICS XML, IO ON", None),
            ("SET SHOWPLAN_XML ON", None),
            ("SELECT 'SET STATISTICS XML ON'", None),
        ];
        for (sql, expected) in cases {
            assert_eq!(statistics_xml_switch(sql), expected, "{sql}");
        }
    }
}
