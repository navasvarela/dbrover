//! Asking the database whether it would accept a console's statements, without running them.

use dbrover_core::{CostEstimate, TransactionStatus, describe_error};
use dbrover_sql::is_checkable;

use super::{Disconnected, SessionOwner, Worker};
use crate::{Event, StatementCheck};

/// Prepares each statement on the console's connection and reports what the database said.
///
/// Skips every statement while unread rows hold the connection (preparing would wait for them,
/// or discard them) or a transaction is open (a rejected statement would abort it on
/// PostgreSQL), and statements that aren't data queries or changes, which may run something
/// while being prepared. A validation never changes what the user is doing on the connection.
pub(super) async fn validate(
    worker: &mut Worker<'_>,
    statements: Vec<String>,
) -> Result<(), Disconnected> {
    let SessionOwner::Console(console) = worker.context.owner else {
        return Ok(());
    };
    let dialect = worker.context.dialect();
    let busy = worker.parked.is_some() || worker.transaction != TransactionStatus::Idle;
    let mut results = Vec::with_capacity(statements.len());
    for sql in statements {
        let checkable = dialect.is_some_and(|dialect| is_checkable(&sql, dialect));
        let check = if busy || !checkable {
            StatementCheck::Skipped
        } else {
            let work = worker.connection.validate(&sql);
            match worker
                .signals
                .guard(work, None, worker.cancel_handle)
                .await?
            {
                Ok(None) => StatementCheck::Accepted {
                    estimate: estimate(worker, &sql).await?,
                },
                Ok(Some(rejection)) => StatementCheck::Rejected(rejection),
                Err(error) => {
                    tracing::warn!(
                        data_source = %worker.context.name(),
                        "could not validate a statement: {}",
                        describe_error(&error)
                    );
                    StatementCheck::Skipped
                }
            }
        };
        results.push((sql, check));
    }
    worker
        .context
        .send(Event::StatementsValidated { console, results });
    Ok(())
}

/// What an accepted statement would scan, on a data source with a cost guard: its engine's
/// estimate in bytes, which BigQuery's driver answers from the dry run its check just made, so
/// it is shown before the statement runs, limit or not (`docs/cloud-data.md`, C4). `None`
/// where there is none, or it couldn't be had.
async fn estimate(
    worker: &mut Worker<'_>,
    sql: &str,
) -> Result<Option<CostEstimate>, Disconnected> {
    let guarded = worker
        .context
        .data_source
        .config
        .cost_guard()
        .is_some_and(|guard| guard.is_on());
    if !guarded {
        return Ok(None);
    }
    let work = worker.connection.estimate_cost(sql);
    let estimate = worker
        .signals
        .guard(work, None, worker.cancel_handle)
        .await?;
    Ok(estimate
        .ok()
        .flatten()
        .filter(|estimate| matches!(estimate, CostEstimate::Bytes(_))))
}
