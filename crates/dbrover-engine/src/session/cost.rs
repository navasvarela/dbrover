//! A data source's cost guard (`dbrover_core::CostGuard`): before a statement runs on an
//! engine that bills each read, the driver estimates what it would cost, and the guard holds
//! it back unless the run allows it (`docs/nosql.md`, N5; `docs/cloud-data.md`, C0e). Every
//! statement a run, an export or Run to file sends goes through [`held_cost`].

use dbrover_core::{CostEstimate, DriverError, ReadPace};

use super::{Disconnected, Worker};

/// What `statement`, read at `pace`, would cost, when the data source's guard holds it back
/// for that: `None` when it costs nothing worth asking about, or nothing asks.
pub(super) async fn held_cost(
    worker: &mut Worker<'_>,
    statement: &str,
    pace: ReadPace,
) -> Result<Result<Option<CostEstimate>, DriverError>, Disconnected> {
    let Some(guard) = worker
        .context
        .data_source
        .config
        .cost_guard()
        .filter(|guard| guard.is_on())
    else {
        return Ok(Ok(None));
    };
    let asked = worker.connection.estimate_cost(statement);
    let estimated = worker
        .signals
        .guard(asked, None, worker.cancel_handle)
        .await?;
    Ok(estimated.map(|estimate| estimate.filter(|estimate| guard.holds(estimate, pace))))
}

/// Why a statement the guard holds was refused where nobody can be asked, as when exporting.
pub(super) fn refused(estimate: &CostEstimate, data_source: &str) -> String {
    let (asks, change) = match estimate {
        CostEstimate::WholeTable(_) => ("reading a whole table", "turn that off"),
        CostEstimate::Bytes(_) => ("scanning that much", "raise its limit"),
    };
    format!(
        "Refused: {} \u{201c}{data_source}\u{201d} asks before {asks}; {change} in the data \
         source to export it.",
        estimate.describe()
    )
}
