//! A read-only data source's console: each statement is checked before it is sent, and runs
//! in the database's read-only mode, as `crate::read_only` describes.
//!
//! Where that mode is a transaction, one is opened before each statement and rolled back once
//! the statement is done: at once for most, or when the last of its rows is read (or dropped)
//! for one whose rows are still being paged through. The console's own transaction status
//! never sees it, so the console shows no open transaction and nothing asks to roll one back.

use dbrover_core::{DriverError, TimeLimit, describe_error};
use dbrover_language::QueryLanguage;

use super::{Disconnected, Worker};
use crate::RunId;
use crate::read_only::{self, ReadOnlyMode, Refusal};

/// How a read-only data source's sessions refuse changes.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReadOnly {
    /// `None` for an engine without a read-only mode (SQL Server): only the check applies.
    pub(super) mode: Option<ReadOnlyMode>,
}

impl ReadOnly {
    /// Refuses `sql` unless it only reads, saying why in terms of `data_source`.
    pub(super) fn check(
        sql: &str,
        language: QueryLanguage,
        data_source: &str,
    ) -> Result<(), String> {
        read_only::check(sql, language).map_err(|refusal| refused(&refusal, data_source))
    }
}

fn refused(refusal: &Refusal, data_source: &str) -> String {
    format!(
        "Refused: {}, and \u{201c}{data_source}\u{201d} is read-only.",
        refusal.reason()
    )
}

/// Opens the read-only transaction for the next statement, if the engine uses one.
pub(super) async fn open(
    worker: &mut Worker<'_>,
    read_only: ReadOnly,
) -> Result<Result<(), DriverError>, Disconnected> {
    open_for_run(worker, read_only, None, None).await
}

/// [`open`] for `run`, telling the transaction to stop its statement after `limit` where the
/// engine can be told (PostgreSQL's `statement_timeout`), so a statement stops even if a cancel
/// can't get through. A cancel of `run` that arrives while the transaction opens is then seen
/// by [`super::Signals::is_cancelled`] rather than lost before the statement starts.
pub(super) async fn open_for_run(
    worker: &mut Worker<'_>,
    read_only: ReadOnly,
    run: Option<RunId>,
    limit: Option<TimeLimit>,
) -> Result<Result<(), DriverError>, Disconnected> {
    let Some(mode @ ReadOnlyMode::Transaction { .. }) = read_only.mode else {
        return Ok(Ok(()));
    };
    let begin = read_only::begin(
        &mut *worker.connection,
        mode,
        limit.map(TimeLimit::duration),
    );
    let begun = worker
        .signals
        .guard(begin, run, worker.cancel_handle)
        .await?;
    if begun.is_ok() {
        worker.read_only_open = Some(mode);
    }
    Ok(begun)
}

/// Rolls back the read-only transaction, if one is open. A failed rollback is only logged:
/// the next statement's `BEGIN` reports whatever state the connection is in.
pub(super) async fn close(worker: &mut Worker<'_>) -> Result<(), Disconnected> {
    let Some(mode) = worker.read_only_open.take() else {
        return Ok(());
    };
    let rollback = read_only::rollback(&mut *worker.connection, mode);
    if let Err(error) = worker
        .signals
        .guard(rollback, None, worker.cancel_handle)
        .await?
    {
        tracing::warn!(
            data_source = %worker.context.name(),
            "could not roll back the read-only transaction: {}",
            describe_error(&error)
        );
    }
    Ok(())
}
