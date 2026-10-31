//! Running a console's statements: one page of rows per statement, and the last statement's
//! cursor kept open ("parked") so the UI can fetch more.
//!
//! Only the last statement stays open because a cursor holds the connection: earlier
//! statements' unread rows are discarded so the next statement can run. Their results are
//! marked truncated.

use std::time::{Duration, Instant};

use dbrover_core::{CancelHandle, DriverError, ResultCursor, describe_error};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource, redact_secrets};
use jiff::Timestamp;

use dbrover_sql::{ExplainDepth, ExplainSetting, controls_transactions};

use super::guard::{self, ReadOnly};
use super::parameters::Sendable;
use super::{Disconnected, Script, SessionContext, Worker, cancel_running_statement, transaction};
use crate::{
    Event, RunId, RunOrigin, RunOutcome, RunPurpose, StatementFailure, StatementSummary,
    TransactionMode,
};

/// Rows per page. Plenty to look at, few enough to arrive at once; other database tools
/// default to between 200 and 1,000.
pub(super) const PAGE_SIZE: usize = 500;
/// How much of a statement the log console shows.
const LOG_PREVIEW_CHARS: usize = 80;

/// Which statement of which run an event or history entry is about.
#[derive(Debug, Clone)]
struct StatementRef {
    run: RunId,
    index: usize,
    /// The statement as sent, and as the history records it.
    sent: Sendable,
    started_at: Timestamp,
    origin: RunOrigin,
}

/// A statement with rows still unread, holding its connection until finished.
pub(super) struct Parked {
    statement: StatementRef,
    cursor: Box<dyn ResultCursor>,
    /// Time until the first page arrived, reported as the statement's duration.
    elapsed: Duration,
}

impl Parked {
    pub(super) fn run(&self) -> RunId {
        self.statement.run
    }
}

enum StatementEnd {
    Completed,
    /// Boxed: a parked statement is far bigger than the other ends.
    Parked(Box<Parked>),
    Failed,
}

pub(super) async fn run_script(
    worker: &mut Worker<'_>,
    script: &Script,
) -> Result<Option<Parked>, Disconnected> {
    // An estimate runs nothing, so it needs no transaction; Explain analyse runs the
    // statements, in the console's transaction mode as a run would.
    let (depth, transactions) = match script.purpose {
        RunPurpose::Run => return run_statements(worker, script, script.transactions).await,
        RunPurpose::Explain => (ExplainDepth::Estimated, TransactionMode::Auto),
        RunPurpose::ExplainAnalyse => (ExplainDepth::Analysed, script.transactions),
    };
    let server = worker.connection.server_info();
    let setting = worker
        .context
        .dialect()
        .and_then(|dialect| dialect.explain_setting(depth, Some(server)));
    match setting {
        Some(setting) => explain_with_setting(worker, script, setting, transactions).await,
        None => run_statements(worker, script, script.transactions).await,
    }
}

/// Explains the script's statements with the session setting switched on, in a batch of its
/// own before them, and switches it off afterwards whatever happened, so the console's next
/// statements run as they did before.
async fn explain_with_setting(
    worker: &mut Worker<'_>,
    script: &Script,
    setting: ExplainSetting,
    transactions: TransactionMode,
) -> Result<Option<Parked>, Disconnected> {
    let switched_on = StatementRef {
        run: script.run,
        index: 0,
        sent: Sendable::plain(setting.on),
        started_at: Timestamp::now(),
        origin: script.origin.clone(),
    };
    if let Err(error) = transaction::run_counting(worker, setting.on, None).await? {
        report_failure(worker, &switched_on, Duration::ZERO, &error);
        worker.context.send(Event::RunFinished {
            run: script.run,
            outcome: RunOutcome::Failed,
            elapsed: Duration::ZERO,
        });
        return Ok(None);
    }
    let parked = run_statements(worker, script, transactions).await?;
    // The setting can only be switched off once nothing holds the connection. A plan is one
    // row, so this only ever drops the plans of a batch's further statements.
    if let Some(parked) = parked {
        discard(worker.context, parked, worker.cancel_handle).await;
    }
    if let Err(error) = transaction::run_counting(worker, setting.off, None).await? {
        tracing::warn!(
            data_source = %worker.context.name(),
            "could not switch {} off again: {}",
            setting.on,
            describe_error(&error)
        );
    }
    Ok(None)
}

async fn run_statements(
    worker: &mut Worker<'_>,
    script: &Script,
    transactions: TransactionMode,
) -> Result<Option<Parked>, Disconnected> {
    let run = script.run;
    let started = Instant::now();
    let last = script.statements.len().saturating_sub(1);
    let mut outcome = RunOutcome::Completed;
    let mut parked = None;
    for (index, sql) in script.statements.iter().enumerate() {
        // A cancel that arrives between statements stops the script before the next one.
        if worker.signals.is_cancelled(run) {
            outcome = RunOutcome::Cancelled;
            break;
        }
        let dialect = worker.context.dialect();
        let sendable = match Sendable::new(sql, dialect, &script.parameters, script.purpose) {
            Ok(sendable) => sendable,
            Err(missing) => {
                // Nothing was sent, so nothing goes in the history.
                let statement = StatementRef {
                    run,
                    index,
                    sent: Sendable::plain(sql),
                    started_at: Timestamp::now(),
                    origin: script.origin.clone(),
                };
                report_refusal(worker.context, &statement, missing.to_string());
                outcome = RunOutcome::Failed;
                break;
            }
        };
        let checked = sendable.checked.clone();
        let statement = StatementRef {
            run,
            index,
            sent: sendable,
            started_at: Timestamp::now(),
            origin: script.origin.clone(),
        };
        if let Some(read_only) = worker.context.read_only {
            let language = worker.context.language();
            if let Err(refusal) = ReadOnly::check(&checked, language, worker.context.name()) {
                report_refusal(worker.context, &statement, refusal);
                outcome = RunOutcome::Failed;
                break;
            }
            if let Err(error) = guard::open(worker, read_only).await? {
                report_failure(worker, &statement, Duration::ZERO, &error);
                guard::close(worker).await?;
                outcome = RunOutcome::Failed;
                break;
            }
        } else if let Err(error) =
            transaction::begin_if_needed(worker, transactions, run, &checked).await?
        {
            report_failure(worker, &statement, Duration::ZERO, &error);
            outcome = RunOutcome::Failed;
            break;
        }
        tracing::info!(data_source = %worker.context.name(), "running {}", preview(&redact_secrets(sql)));
        let end = run_statement(worker, statement, index == last).await?;
        // Rows still being paged keep the read-only transaction open until they are done.
        if !matches!(end, StatementEnd::Parked(_)) {
            guard::close(worker).await?;
        }
        // A parked statement still holds the connection, so it can't be asked. Only the last
        // statement parks, and one that returns rows hardly ever controls transactions.
        let failed = matches!(end, StatementEnd::Failed);
        let controls = dialect.is_some_and(|dialect| controls_transactions(&checked, dialect));
        if (failed || controls) && !matches!(end, StatementEnd::Parked(_)) {
            transaction::refresh(worker).await?;
        }
        match end {
            StatementEnd::Completed => {}
            StatementEnd::Parked(statement) => parked = Some(*statement),
            StatementEnd::Failed if worker.signals.is_cancelled(run) => {
                outcome = RunOutcome::Cancelled;
                break;
            }
            StatementEnd::Failed => {
                outcome = RunOutcome::Failed;
                break;
            }
        }
    }
    worker.context.send(Event::RunFinished {
        run,
        outcome,
        elapsed: started.elapsed(),
    });
    Ok(parked)
}

async fn run_statement(
    worker: &mut Worker<'_>,
    statement: StatementRef,
    is_last: bool,
) -> Result<StatementEnd, Disconnected> {
    let run = statement.run;
    let started = Instant::now();
    let execute = statement.sent.execute(worker.connection);
    let mut cursor = match worker
        .signals
        .guard(execute, Some(run), worker.cancel_handle)
        .await?
    {
        Ok(cursor) => cursor,
        Err(error) => {
            report_failure(worker, &statement, started.elapsed(), &error);
            return Ok(StatementEnd::Failed);
        }
    };
    if cursor.columns().is_empty() {
        return finish(worker, statement, cursor, started, None).await;
    }

    worker.context.send(Event::StatementStarted {
        run,
        statement: statement.index,
        columns: cursor.columns().to_vec(),
        shape: cursor.shape(),
    });
    let page = cursor.next_page(PAGE_SIZE);
    let rows = match worker
        .signals
        .guard(page, Some(run), worker.cancel_handle)
        .await?
    {
        Ok(rows) => rows,
        Err(error) => {
            // The statement already failed; whatever finishing reports adds nothing.
            let _ = cursor.finish().await;
            report_failure(worker, &statement, started.elapsed(), &error);
            return Ok(StatementEnd::Failed);
        }
    };
    let elapsed = started.elapsed();
    let more = !cursor.is_exhausted();
    worker.context.send(Event::RowsFetched {
        run,
        statement: statement.index,
        rows,
        more: more && is_last,
    });
    if more && is_last {
        return Ok(StatementEnd::Parked(Box::new(Parked {
            statement,
            cursor,
            elapsed,
        })));
    }
    finish(worker, statement, cursor, started, Some(elapsed)).await
}

/// Ends a statement and reports it. If rows were left unread, the server is asked to stop
/// producing them first, and the error that cancel causes is expected rather than reported.
async fn finish(
    worker: &mut Worker<'_>,
    statement: StatementRef,
    cursor: Box<dyn ResultCursor>,
    started: Instant,
    first_page: Option<Duration>,
) -> Result<StatementEnd, Disconnected> {
    let truncated = !cursor.is_exhausted();
    if truncated {
        cancel_running_statement(worker.cancel_handle).await;
    }
    let finished = worker
        .signals
        .guard(cursor.finish(), Some(statement.run), worker.cancel_handle)
        .await?;
    let elapsed = first_page.unwrap_or_else(|| started.elapsed());
    let rows_changed = match finished {
        Ok(rows_changed) => rows_changed,
        Err(_) if truncated => None,
        Err(error) => {
            report_failure(worker, &statement, elapsed, &error);
            return Ok(StatementEnd::Failed);
        }
    };
    report_success(
        worker.context,
        &statement,
        StatementSummary {
            rows_changed,
            elapsed,
            truncated,
        },
    );
    Ok(StatementEnd::Completed)
}

pub(super) async fn fetch_more(
    worker: &mut Worker<'_>,
    parked: Parked,
) -> Result<Option<Parked>, Disconnected> {
    let Parked {
        statement,
        mut cursor,
        elapsed,
    } = parked;
    let run = statement.run;
    let page = cursor.next_page(PAGE_SIZE);
    let rows = match worker
        .signals
        .guard(page, Some(run), worker.cancel_handle)
        .await?
    {
        Ok(rows) => rows,
        Err(error) => {
            let _ = cursor.finish().await;
            report_failure(worker, &statement, elapsed, &error);
            return Ok(None);
        }
    };
    let more = !cursor.is_exhausted();
    worker.context.send(Event::RowsFetched {
        run,
        statement: statement.index,
        rows,
        more,
    });
    if more {
        return Ok(Some(Parked {
            statement,
            cursor,
            elapsed,
        }));
    }
    let finished = worker
        .signals
        .guard(cursor.finish(), Some(run), worker.cancel_handle)
        .await?;
    match finished {
        Ok(rows_changed) => report_success(
            worker.context,
            &statement,
            StatementSummary {
                rows_changed,
                elapsed,
                truncated: false,
            },
        ),
        Err(error) => report_failure(worker, &statement, elapsed, &error),
    }
    Ok(None)
}

/// Drops a parked statement's unread rows, when the next request needs the connection or
/// the user cancels. Not interruptible: it is quick once the server has been told to stop.
pub(super) async fn discard(
    context: &SessionContext,
    parked: Parked,
    cancel_handle: &dyn CancelHandle,
) {
    cancel_running_statement(cancel_handle).await;
    // An error here is the cancellation just requested.
    let _ = parked.cursor.finish().await;
    report_success(
        context,
        &parked.statement,
        StatementSummary {
            rows_changed: None,
            elapsed: parked.elapsed,
            truncated: true,
        },
    );
}

fn report_success(context: &SessionContext, statement: &StatementRef, summary: StatementSummary) {
    tracing::info!(
        data_source = %context.name(),
        elapsed_ms = summary.elapsed.as_millis(),
        rows = summary.rows_changed,
        truncated = summary.truncated,
        "statement {} finished",
        statement.index + 1
    );
    context.record(history_entry(
        context,
        statement,
        summary.elapsed,
        summary.rows_changed,
        HistoryOutcome::Ok,
    ));
    context.send(Event::StatementFinished {
        run: statement.run,
        statement: statement.index,
        result: Ok(summary),
    });
}

/// Reports a statement a read-only data source refused, without sending it. Not recorded in
/// the history, which holds what was sent to the database.
fn report_refusal(context: &SessionContext, statement: &StatementRef, message: String) {
    tracing::warn!(data_source = %context.name(), "statement {}: {message}", statement.index + 1);
    context.send(Event::StatementFinished {
        run: statement.run,
        statement: statement.index,
        result: Err(StatementFailure {
            message,
            position: None,
        }),
    });
}

fn report_failure(
    worker: &Worker<'_>,
    statement: &StatementRef,
    elapsed: Duration,
    error: &DriverError,
) {
    let context = worker.context;
    let (message, position) = match error {
        // The outer "the statement failed" adds nothing in a console that shows the result
        // of each statement; the database's own message is what matters.
        DriverError::Statement {
            source, position, ..
        } => (
            describe_error(source.as_ref()),
            statement.sent.typed_position(*position),
        ),
        other => (describe_error(other), None),
    };
    tracing::warn!(data_source = %context.name(), "statement {} failed: {message}", statement.index + 1);
    let outcome = if worker.signals.is_cancelled(statement.run) {
        HistoryOutcome::Cancelled
    } else {
        HistoryOutcome::Error
    };
    context.record(history_entry(context, statement, elapsed, None, outcome));
    context.send(Event::StatementFinished {
        run: statement.run,
        statement: statement.index,
        result: Err(StatementFailure { message, position }),
    });
}

fn history_entry(
    context: &SessionContext,
    statement: &StatementRef,
    elapsed: Duration,
    rows: Option<u64>,
    outcome: HistoryOutcome,
) -> HistoryEntry {
    HistoryEntry {
        data_source: context.data_source.id,
        executed_at: statement.started_at,
        sql: statement.sent.recorded.clone(),
        duration: Some(elapsed),
        rows,
        outcome,
        source: match &statement.origin {
            RunOrigin::Console => HistorySource::Console,
            RunOrigin::Action(id) => HistorySource::Action {
                id: id.as_str().to_owned(),
            },
        },
    }
}

/// The statement's first line, shortened for the log.
fn preview(sql: &str) -> String {
    let first_line = sql.lines().next().unwrap_or_default();
    let shortened: String = first_line.chars().take(LOG_PREVIEW_CHARS).collect();
    if shortened.len() < sql.len() {
        format!("{shortened}…")
    } else {
        shortened
    }
}
