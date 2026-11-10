//! What a plan reads of each table: which columns, under which filters, and how many rows its
//! scans passed on, from the scans' details as DuckDB writes them (`Projections`, `Filters`).
//! The report's combine step is a DuckDB query over its sources' tables, so this is what the
//! Profile's funnel and the plan-based hints read (`docs/reports-at-scale.md`, RS2c and RS2e).

use crate::{Plan, Step};

/// The detail listing the columns a DuckDB scan reads, a line each.
const PROJECTIONS: &str = "Projections";
/// The detail listing the filters a DuckDB scan applies as it reads, a line each.
const FILTERS: &str = "Filters";

/// One scan of a table in a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRead {
    /// As the plan names it, without its schema: `orders` for `main.orders`.
    pub table: String,
    /// The columns the scan reads, in the plan's order; empty when the plan doesn't say.
    pub columns: Vec<String>,
    /// The filters applied as the table is read, as the plan writes them: `region='EU'`.
    pub filters: Vec<String>,
    /// The rows the scan passed on, once the statement ran.
    pub rows: Option<f64>,
}

impl Plan {
    /// Every scan of a table, in reading order. A step is a scan when it names a table (its
    /// target) and has no step under it.
    pub fn table_reads(&self) -> Vec<TableRead> {
        let mut reads = Vec::new();
        add_reads(&self.root, &mut reads);
        reads
    }
}

fn add_reads(step: &Step, reads: &mut Vec<TableRead>) {
    if let Some(read) = table_read(step) {
        reads.push(read);
    }
    for child in &step.children {
        add_reads(child, reads);
    }
}

/// `step` as a scan of a table, if it is one.
pub(crate) fn table_read(step: &Step) -> Option<TableRead> {
    if !step.children.is_empty() {
        return None;
    }
    let target = step.target.as_deref()?;
    let table = target.rsplit('.').next().unwrap_or(target).to_owned();
    Some(TableRead {
        table,
        columns: lines(step, PROJECTIONS),
        filters: lines(step, FILTERS),
        rows: step.actual.and_then(|actual| actual.rows),
    })
}

fn lines(step: &Step, label: &str) -> Vec<String> {
    step.details
        .iter()
        .filter(|detail| detail.label == label)
        .flat_map(|detail| detail.value.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::PlanFormat;

    #[test]
    fn lists_each_scan_with_its_columns_filters_and_rows() {
        let text = std::fs::read_to_string(format!(
            "{}/fixtures/duckdb/explain.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let plan = PlanFormat::DuckDbJson.parse(&text).unwrap();
        assert_eq!(
            plan.table_reads(),
            [TableRead {
                table: "orders".to_owned(),
                columns: vec!["region".to_owned(), "amount".to_owned()],
                filters: vec!["region='EU'".to_owned()],
                rows: None,
            }]
        );
        let profiled = std::fs::read_to_string(format!(
            "{}/fixtures/duckdb/profile.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let plan = PlanFormat::DuckDbJson.parse(&profiled).unwrap();
        let reads = plan.table_reads();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].rows, Some(1000.0));
    }
}
