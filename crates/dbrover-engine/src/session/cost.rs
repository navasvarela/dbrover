//! A data source's cost guard (`dbrover_core::CostGuard`): before a statement runs on an
//! engine that bills each read, the driver says whether it would read a whole table, and the
//! guard holds it back unless the run allows it (`docs/nosql.md`, N5).

use dbrover_core::{CostGuard, DriverError, FullScan};

use super::{Disconnected, Worker};

/// The whole-table read `statement` would make, when the data source's guard asks before
/// one: what holds the statement back. `None` when it reads by key, or nothing asks.
pub(super) async fn held_scan(
    worker: &mut Worker<'_>,
    statement: &str,
) -> Result<Result<Option<FullScan>, DriverError>, Disconnected> {
    if worker.context.data_source.config.cost_guard() != Some(CostGuard::AskBeforeScans) {
        return Ok(Ok(None));
    }
    let asked = worker.connection.full_scan(statement);
    worker
        .signals
        .guard(asked, None, worker.cancel_handle)
        .await
}

/// Why a statement that would scan was refused where nobody can be asked, as when exporting.
pub(super) fn refused(scan: &FullScan, data_source: &str) -> String {
    format!(
        "Refused: {} \u{201c}{data_source}\u{201d} asks before reading a whole table; turn \
         that off in the data source to export one.",
        scan.describe()
    )
}
