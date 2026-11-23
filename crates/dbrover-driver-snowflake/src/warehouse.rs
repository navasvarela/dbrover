//! The data source's warehouse as Test connection names it: its size, whether it is suspended,
//! and the statement timeout it keeps itself (`docs/cloud-data.md`, C5f). Both come from `SHOW`
//! commands, which read a warehouse's state without resuming it.

use std::sync::Arc;

use dbrover_core::{DriverError, Jobs, TimeLimit};

use crate::catalog::{like_exactly, literal, quoted, show};

/// A warehouse as `SHOW WAREHOUSES` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Warehouse {
    pub name: String,
    /// `X-Small`, `Large`, …
    pub size: String,
    /// `STARTED`, `SUSPENDED` or `RESIZING`.
    pub state: String,
}

impl Warehouse {
    /// `warehouse ANALYTICS_WH (X-Small, suspended; stops statements after 1 h)`.
    pub(crate) fn describe(&self, own_limit: Option<TimeLimit>) -> String {
        let mut said = vec![self.size.clone(), self.state.to_lowercase()];
        said.retain(|part| !part.is_empty());
        let mut text = format!("warehouse {} ({}", self.name, said.join(", "));
        if let Some(limit) = own_limit {
            text.push_str(&format!("; stops statements after {limit}"));
        }
        text.push(')');
        text
    }
}

/// The warehouse named `name`, or `None` where the role sees none of that name. `SHOW
/// WAREHOUSES LIKE` ignores case, so an exact match wins over one that differs only in case.
pub(crate) async fn find(jobs: &Arc<Jobs>, name: &str) -> Result<Option<Warehouse>, DriverError> {
    let statement = format!("SHOW WAREHOUSES LIKE {}", literal(&like_exactly(name)));
    let shown = show(jobs, &statement).await?;
    let found = shown
        .rows
        .iter()
        .find(|row| shown.get(row, "name") == name)
        .or_else(|| {
            shown
                .rows
                .iter()
                .find(|row| shown.get(row, "name").eq_ignore_ascii_case(name))
        });
    Ok(found.map(|row| Warehouse {
        name: shown.get(row, "name").to_owned(),
        size: shown.get(row, "size").to_owned(),
        state: shown.get(row, "state").to_owned(),
    }))
}

/// The `STATEMENT_TIMEOUT_IN_SECONDS` set on the warehouse itself, which stops a statement even
/// when the data source's own limit is longer; `None` where none is set there.
pub(crate) async fn own_limit(
    jobs: &Arc<Jobs>,
    name: &str,
) -> Result<Option<TimeLimit>, DriverError> {
    let statement = format!(
        "SHOW PARAMETERS LIKE 'STATEMENT_TIMEOUT_IN_SECONDS' IN WAREHOUSE {}",
        quoted(name)
    );
    let shown = show(jobs, &statement).await?;
    Ok(shown
        .rows
        .iter()
        .find(|row| shown.get(row, "level").eq_ignore_ascii_case("WAREHOUSE"))
        .and_then(|row| shown.get(row, "value").parse().ok())
        .and_then(TimeLimit::from_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warehouse_says_its_size_state_and_own_limit() {
        let warehouse = Warehouse {
            name: "ANALYTICS_WH".to_owned(),
            size: "X-Small".to_owned(),
            state: "SUSPENDED".to_owned(),
        };
        assert_eq!(
            warehouse.describe(None),
            "warehouse ANALYTICS_WH (X-Small, suspended)"
        );
        assert_eq!(
            warehouse.describe(TimeLimit::from_seconds(3600)),
            "warehouse ANALYTICS_WH (X-Small, suspended; stops statements after 1 h)"
        );
    }
}
