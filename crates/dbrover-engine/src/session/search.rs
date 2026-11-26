//! A data search's plan and run (W16, `docs/data-search.md`), on a session of the search's
//! own whose data source is always opened read-only.
//!
//! Each table's statement runs in the engine's read-only mode, under the table's time limit,
//! and reads at most the rows per table and one more. A table that runs out of time, or that
//! the server refuses, is given up on and the search goes on; a lost connection or Cancel ends
//! it. The search is recorded in the history once, when it ends.

use std::time::Instant;

use dbrover_core::{
    CaseMatch, Connection, DriverError, MatchKind, ResultCursor, Row, SafetyLevel, describe_error,
};
use dbrover_sql::{SearchSupport, search_statement};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource};
use jiff::Timestamp;

use super::guard::{self, ReadOnly};
use super::{Disconnected, Worker, cancel_running_statement};
use crate::data_search::{
    PlannedTable, SearchEnd, SearchPlan, SearchQuestion, SearchRequest, TableOutcome, TableSearched,
};
use crate::read_only::ReadOnlyMode;
use crate::{Event, RunId};

/// How much longer than a table's limit DbRover waits before cancelling its statement, where
/// the read-only transaction tells the server the limit itself (PostgreSQL's
/// `statement_timeout`). Cancelling at the same moment as the server raced it: a cancel that
/// arrived after the server had stopped the statement cancelled the `ROLLBACK` that followed,
/// and the next statement found its transaction aborted (seen in `w16_exit`, one run in three).
/// The server's limit is the one that acts; this cancel is for a server that didn't.
const SERVER_LIMIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// A plan, kept by the search's session until it is run.
#[derive(Debug)]
pub(super) struct Planned {
    run: RunId,
    request: SearchRequest,
    plan: SearchPlan,
}

/// Lists the scope's tables, writes their statements, and orders them smallest first, so most
/// matches show early and the large tables, likeliest to time out, come last.
pub(super) async fn plan(
    worker: &mut Worker<'_>,
    run: RunId,
    request: SearchRequest,
    safety: SafetyLevel,
) -> Result<(), Disconnected> {
    worker.search = None;
    let result = list(worker, run, &request).await?.map(|tables| {
        let searched = tables
            .iter()
            .filter(|table| table.statement.is_some())
            .count();
        let question = (safety == SafetyLevel::Careful && searched > 1)
            .then_some(SearchQuestion::Careful { tables: searched });
        SearchPlan { tables, question }
    });
    if let Ok(plan) = &result {
        worker.search = Some(Planned {
            run,
            request,
            plan: plan.clone(),
        });
    }
    worker.context.send(Event::DataSearchPlanned {
        data_source: worker.context.data_source.id,
        run,
        result,
    });
    Ok(())
}

async fn list(
    worker: &mut Worker<'_>,
    run: RunId,
    request: &SearchRequest,
) -> Result<Result<Vec<PlannedTable>, String>, Disconnected> {
    let Some(dialect) = worker.context.dialect() else {
        return Ok(Err("This database's data can't be searched.".to_owned()));
    };
    let service = worker.connection.server_info().service;
    if dialect.data_search(service) == SearchSupport::Unsupported {
        return Ok(Err("This database's data can't be searched yet.".to_owned()));
    }
    let cancel_handle = worker.cancel_handle;
    let listed = worker
        .signals
        .guard(
            worker.connection.searchable_tables(&request.tables),
            Some(run),
            cancel_handle,
        )
        .await?;
    let mut tables = match listed {
        Ok(tables) => tables,
        Err(error) => return Ok(Err(describe_error(&error))),
    };
    tables.sort_by(|a, b| {
        (
            a.row_estimate.is_none(),
            a.row_estimate,
            &a.name.schema,
            &a.name.name,
        )
            .cmp(&(
                b.row_estimate.is_none(),
                b.row_estimate,
                &b.name.schema,
                &b.name.name,
            ))
    });
    Ok(Ok(tables
        .into_iter()
        .map(|table| PlannedTable {
            statement: search_statement(dialect, &table, &request.search, request.limits.rows),
            table,
        })
        .collect()))
}

/// Runs the plan `run` names, table by table, then records the search.
pub(super) async fn run(worker: &mut Worker<'_>, run: RunId) -> Result<(), Disconnected> {
    let data_source = worker.context.data_source.id;
    let Some(Planned { request, plan, .. }) =
        worker.search.take().filter(|planned| planned.run == run)
    else {
        worker.context.send(Event::DataSearchEnded {
            data_source,
            run,
            end: SearchEnd::Failed("The search wasn't planned; search again.".to_owned()),
        });
        return Ok(());
    };
    let started_at = Timestamp::now();
    let started = Instant::now();
    let mut tally = Tally::default();
    let mut end = SearchEnd::Finished;
    for (index, planned) in plan.tables.iter().enumerate() {
        let Some(statement) = &planned.statement else {
            continue;
        };
        if worker.signals.is_cancelled(run) {
            end = SearchEnd::Cancelled;
            break;
        }
        worker.context.send(Event::DataSearchTableStarted {
            data_source,
            run,
            index,
        });
        let table_started = Instant::now();
        let outcome = search_table(worker, run, &statement.sql, &request).await?;
        if worker.signals.is_cancelled(run) {
            end = SearchEnd::Cancelled;
            break;
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(lost) => {
                end = SearchEnd::Failed(lost);
                break;
            }
        };
        tally.add(&outcome, &statement.sql);
        worker.context.send(Event::DataSearchTableSearched {
            data_source,
            run,
            searched: TableSearched {
                index,
                outcome,
                took: table_started.elapsed(),
            },
        });
    }
    worker.context.record(HistoryEntry {
        data_source,
        executed_at: started_at,
        sql: tally.statements(&request, &plan),
        duration: Some(started.elapsed()),
        rows: Some(tally.rows),
        outcome: match end {
            SearchEnd::Finished => HistoryOutcome::Ok,
            SearchEnd::Cancelled => HistoryOutcome::Cancelled,
            SearchEnd::Failed(_) => HistoryOutcome::Error,
        },
        source: HistorySource::Search {
            searched: tally.summary(&request, &plan),
        },
    });
    worker.context.send(Event::DataSearchEnded {
        data_source,
        run,
        end,
    });
    Ok(())
}

/// One table's statement, in the read-only mode, under the time limit. `Err` when the
/// connection was lost, which ends the search.
async fn search_table(
    worker: &mut Worker<'_>,
    run: RunId,
    sql: &str,
    request: &SearchRequest,
) -> Result<Result<TableOutcome, String>, Disconnected> {
    let context = worker.context;
    if let Err(refusal) = ReadOnly::check(sql, context.language(), context.name()) {
        return Ok(Ok(TableOutcome::Failed(refusal)));
    }
    let limit = request.limits.per_table;
    if let Some(read_only) = context.read_only
        && let Err(error) = guard::open_for_run(worker, read_only, Some(run), limit).await?
    {
        return Ok(classify(Err(error), false));
    }
    // Cancelled while the transaction opened: the guard below would never hear of it, since
    // the change it waits for has been seen. The caller sees the cancel and stops.
    if worker.signals.is_cancelled(run) {
        guard::close(worker).await?;
        return Ok(Ok(TableOutcome::NoMatch));
    }
    let asked = usize::try_from(request.limits.rows.asked()).unwrap_or(usize::MAX);
    let cancel_handle = worker.cancel_handle;
    let timer = limit.map(|limit| match context.read_only {
        Some(ReadOnly {
            mode:
                Some(ReadOnlyMode::Transaction {
                    sets_timeout: true, ..
                }),
        }) => limit.duration() + SERVER_LIMIT_GRACE,
        _ => limit.duration(),
    });
    let read = read_rows(&mut *worker.connection, sql, asked);
    let timed = async {
        tokio::pin!(read);
        let Some(timer) = timer else {
            return (read.await, false);
        };
        tokio::select! {
            biased;
            result = &mut read => (result, false),
            () = tokio::time::sleep(timer) => {
                cancel_running_statement(cancel_handle).await;
                (read.await, true)
            }
        }
    };
    let read_started = Instant::now();
    let (result, timed_out) = worker
        .signals
        .guard(timed, Some(run), cancel_handle)
        .await?;
    guard::close(worker).await?;
    // PostgreSQL's own `statement_timeout`, set to the same limit, may stop the statement just
    // before the timer here does: a failure at the limit is the limit's.
    let past_limit = |limit: dbrover_core::TimeLimit| {
        timed_out || (result.is_err() && read_started.elapsed() >= limit.duration())
    };
    let outcome = match limit {
        Some(limit) if past_limit(limit) => Ok(TableOutcome::TimedOut(limit)),
        _ => classify(result, true),
    };
    Ok(outcome.map(|outcome| match outcome {
        TableOutcome::Matched { mut rows, .. } if rows.len() >= asked => {
            rows.truncate(asked.saturating_sub(1));
            TableOutcome::Matched { rows, more: true }
        }
        outcome => outcome,
    }))
}

/// What a table's read says: its rows, or the server's refusal. A lost connection is the
/// search's end, not the table's.
fn classify(result: Result<Vec<Row>, DriverError>, read: bool) -> Result<TableOutcome, String> {
    match result {
        Ok(rows) if rows.is_empty() => Ok(TableOutcome::NoMatch),
        Ok(rows) => Ok(TableOutcome::Matched { rows, more: false }),
        Err(error @ DriverError::Lost(_)) => Err(describe_error(&error)),
        Err(error) => {
            let message = describe_error(&error);
            let message = if read {
                message
            } else {
                format!("The read-only transaction couldn't start: {message}")
            };
            Ok(TableOutcome::Failed(message))
        }
    }
}

/// Runs `sql` and reads up to `asked` rows, closing the cursor after.
async fn read_rows(
    connection: &mut dyn Connection,
    sql: &str,
    asked: usize,
) -> Result<Vec<Row>, DriverError> {
    let mut cursor = connection.execute(sql).await?;
    let mut rows: Vec<Row> = Vec::new();
    while rows.len() < asked && !cursor.is_exhausted() {
        let page = cursor.next_page(asked - rows.len()).await?;
        rows.extend(page);
    }
    finish(cursor).await?;
    Ok(rows)
}

/// Lets the server finish a cursor read to its cap; one cut short is dropped, which every
/// driver closes on its own.
async fn finish(cursor: Box<dyn ResultCursor>) -> Result<(), DriverError> {
    if cursor.is_exhausted() {
        cursor.finish().await.map(drop)
    } else {
        drop(cursor);
        Ok(())
    }
}

/// What a search found, for the history.
#[derive(Debug, Default)]
struct Tally {
    statements: Vec<String>,
    matched: usize,
    timed_out: usize,
    failed: usize,
    searched: usize,
    rows: u64,
}

impl Tally {
    fn add(&mut self, outcome: &TableOutcome, sql: &str) {
        self.searched += 1;
        match outcome {
            TableOutcome::Matched { rows, .. } => {
                self.matched += 1;
                self.rows += u64::try_from(rows.len()).unwrap_or(u64::MAX);
                self.statements.push(sql.to_owned());
            }
            TableOutcome::NoMatch => {}
            TableOutcome::TimedOut(_) => self.timed_out += 1,
            TableOutcome::Failed(_) => self.failed += 1,
        }
    }

    /// The statements of the tables that matched, so **Run again** finds the same rows; a
    /// comment saying what was searched when none did.
    fn statements(&self, request: &SearchRequest, plan: &SearchPlan) -> String {
        if self.statements.is_empty() {
            return format!("-- {}", self.summary(request, plan));
        }
        self.statements
            .iter()
            .map(|sql| format!("{sql};"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// *Searched 3 of 4 tables for "o'brien", contains, ignoring case: 1 matched*, with the
    /// tables that timed out or failed.
    fn summary(&self, request: &SearchRequest, plan: &SearchPlan) -> String {
        let search = &request.search;
        let matching = match search.matching {
            MatchKind::Contains => "contains",
            MatchKind::WholeValue => "whole value",
        };
        let case = match search.case {
            CaseMatch::Ignore => "ignoring case",
            CaseMatch::Match => "matching case",
        };
        let mut summary = format!(
            "Searched {} of {} tables for \u{201c}{}\u{201d}, {matching}, {case}: {} matched",
            self.searched,
            plan.searched(),
            search.value.text(),
            self.matched
        );
        if self.timed_out > 0 {
            summary.push_str(&format!(", {} timed out", self.timed_out));
        }
        if self.failed > 0 {
            summary.push_str(&format!(", {} failed", self.failed));
        }
        summary
    }
}
