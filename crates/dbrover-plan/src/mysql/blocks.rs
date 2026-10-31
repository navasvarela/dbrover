//! MySQL's first JSON format and MariaDB's: a `query_block` per `SELECT`, holding what is done
//! to its rows (`ordering_operation`, `grouping_operation`, `filesort`, `temporary_table`, …)
//! nested inside each other, down to a `nested_loop` of the `table`s it reads. Subqueries,
//! unions and materialized tables hold query blocks of their own.
//!
//! Each such key becomes a step, named as MySQL's tree format names the same work where it can
//! (`Sort`, `Table scan on c`, `Index lookup on o using customer_id`). MySQL's figures are
//! estimates only; MariaDB's `ANALYZE FORMAT=JSON` adds what happened (`r_loops`, `r_rows`, the
//! `r_…_time_ms` times), in totals over every loop.

use serde_json::Value;

use crate::json::{Object, label, number, shown, text};
use crate::{Actual, Detail, Estimate, Plan, Step, Timing};

/// The keys whose object is a step of its own, besides `table`.
const STEP_KEYS: &[&str] = &[
    "query_block",
    "table",
    "ordering_operation",
    "grouping_operation",
    "duplicates_removal",
    "filesort",
    "temporary_table",
    "union_result",
    "materialized_from_subquery",
    "materialized",
    "subquery_cache",
    "buffer_result",
    "windowing",
    "read_sorted_file",
];

/// The keys a step's figures and name are read from, which aren't repeated among its details.
const SHOWN_ELSEWHERE: &[&str] = &[
    "select_id",
    "table_name",
    "access_type",
    "key",
    "rows",
    "rows_examined_per_scan",
    "cost",
    "r_loops",
    "r_rows",
    "r_total_time_ms",
    "r_table_time_ms",
    "r_other_time_ms",
];

/// The costs MySQL keeps in `cost_info` that a step's estimate is built from.
const ESTIMATED_COSTS: &[&str] = &["query_cost", "read_cost", "eval_cost"];

pub(super) fn plan(root: &Object, block: &Object) -> Plan {
    Plan {
        root: step("query_block", block),
        details: root
            .iter()
            .filter(|(key, _)| key.as_str() != "query_block")
            .map(|(key, value)| Detail::new(label(key), shown(value)))
            .collect(),
    }
}

fn step(key: &str, node: &Object) -> Step {
    let (operation, target) = name(key, node);
    let mut step = Step {
        operation,
        target,
        heading: None,
        estimate: estimate(node),
        actual: actual(node),
        details: Vec::new(),
        children: Vec::new(),
    };
    for (key, value) in node {
        match value {
            _ if SHOWN_ELSEWHERE.contains(&key.as_str()) => {}
            Value::Object(costs) if key == "cost_info" => step.details.extend(
                costs
                    .iter()
                    .filter(|(cost, _)| !ESTIMATED_COSTS.contains(&cost.as_str()))
                    .map(|(cost, value)| Detail::new(label(cost), shown(value))),
            ),
            Value::Object(child) if STEP_KEYS.contains(&key.as_str()) => {
                step.children.push(self::step(key, child));
            }
            Value::Array(items) if key == "nested_loop" => {
                step.children.push(nested_loop(items));
            }
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                let heading = (key != "query_specifications").then(|| label(key));
                step.children
                    .extend(items.iter().filter_map(Value::as_object).map(|item| Step {
                        heading: heading.clone(),
                        ..item_step(key, item)
                    }));
            }
            value => step.details.push(Detail::new(label(key), shown(value))),
        }
    }
    step
}

/// A join of tables, read one after another, each row of one looking up the next.
fn nested_loop(items: &[Value]) -> Step {
    Step {
        operation: "Nested loop".to_owned(),
        target: None,
        heading: None,
        estimate: Estimate::default(),
        actual: None,
        details: Vec::new(),
        children: items
            .iter()
            .filter_map(Value::as_object)
            .map(|item| item_step("nested_loop", item))
            .collect(),
    }
}

/// An item of a list: usually an object wrapping one step (`{"table": {…}}`, or a subquery's
/// `{"dependent": true, "query_block": {…}}`), whose other values become the step's details.
fn item_step(list: &str, item: &Object) -> Step {
    let wrapped = item.iter().find_map(|(key, value)| match value {
        Value::Object(inner) if STEP_KEYS.contains(&key.as_str()) => Some((key, inner)),
        _ => None,
    });
    let others_plain = item.values().filter(|value| value.is_object()).count() == 1;
    match wrapped {
        Some((key, inner)) if others_plain => {
            let mut step = step(key, inner);
            step.details.extend(
                item.iter()
                    .filter(|(other, _)| *other != key)
                    .map(|(other, value)| Detail::new(label(other), shown(value))),
            );
            step
        }
        _ => step(list, item),
    }
}

/// The step's name and target, as MySQL's tree format would write them.
fn name(key: &str, node: &Object) -> (String, Option<String>) {
    let operation = match key {
        "table" => return table_name(node),
        "query_block" => match node.get("select_id") {
            Some(id) => format!("Select #{}", shown(id)),
            None => "Select".to_owned(),
        },
        "ordering_operation" | "filesort" => "Sort".to_owned(),
        "grouping_operation" => "Group".to_owned(),
        "duplicates_removal" => "Remove duplicates".to_owned(),
        "temporary_table" => "Temporary table".to_owned(),
        "union_result" => "Union".to_owned(),
        "materialized_from_subquery" | "materialized" => "Materialize".to_owned(),
        "windowing" => "Window".to_owned(),
        other => label(other),
    };
    (operation, None)
}

/// A table's step, named by how it is read: `Table scan on c`, `Index lookup on o using
/// customer_id`. A table entry with only a message (`No tables used`) is named by it.
fn table_name(table: &Object) -> (String, Option<String>) {
    let Some(name) = text(table, "table_name") else {
        return (
            text(table, "message").unwrap_or_else(|| "Table".to_owned()),
            None,
        );
    };
    let access = text(table, "access_type").unwrap_or_default();
    let operation = match access.as_str() {
        "ALL" => "Table scan",
        "index" => "Index scan",
        "range" => "Index range scan",
        "ref" => "Index lookup",
        "ref_or_null" => "Index lookup or null",
        "eq_ref" => "Single-row index lookup",
        "const" | "system" => "Constant row",
        "fulltext" => "Full-text index search",
        "index_merge" => "Index merge",
        "unique_subquery" | "index_subquery" => "Subquery index lookup",
        _ => "Table",
    };
    let target = match text(table, "key") {
        Some(key) => format!("on {name} using {key}"),
        None => format!("on {name}"),
    };
    (operation.to_owned(), Some(target))
}

/// MySQL's estimate is in `cost_info`: a block's whole cost, or a table's read and evaluation
/// costs; MariaDB's is a step's `cost`. A table's rows are those it reads each time it is read.
fn estimate(node: &Object) -> Estimate {
    let costs = node.get("cost_info").and_then(Value::as_object);
    let mysql = costs.and_then(|costs| {
        number(costs, "query_cost")
            .or_else(|| Some(number(costs, "read_cost")? + number(costs, "eval_cost")?))
    });
    Estimate {
        startup_cost: None,
        total_cost: mysql.or_else(|| number(node, "cost")),
        rows: number(node, "rows_examined_per_scan").or_else(|| number(node, "rows")),
    }
}

/// MariaDB's actual figures: how many times the step ran, its rows each time (a table's only),
/// and the time it took in all, which is spread over its loops to compare with PostgreSQL's
/// time per loop.
fn actual(node: &Object) -> Option<Actual> {
    let loops = number(node, "r_loops")?;
    let total_ms = number(node, "r_total_time_ms").or_else(|| {
        Some(number(node, "r_table_time_ms")? + number(node, "r_other_time_ms").unwrap_or(0.0))
    });
    Some(Actual {
        rows: number(node, "r_rows"),
        loops,
        time: total_ms.filter(|_| loops > 0.0).map(|total_ms| Timing {
            first_row_ms: None,
            last_row_ms: total_ms / loops,
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::super::tests::{costliest, fixture, titles};
    use crate::{Detail, Measure};

    #[test]
    fn reads_mysqls_first_format_as_blocks_of_operations() {
        let plan = fixture("join_v1");
        assert!(!plan.is_analysed());
        assert_eq!(
            titles(&plan),
            [
                (0, "Select #1".to_owned()),
                (1, "Sort".to_owned()),
                (2, "Group".to_owned()),
                (3, "Nested loop".to_owned()),
                (4, "Table scan on c".to_owned()),
                (4, "Index lookup on o using customer_id".to_owned()),
            ]
        );
        let scan = &plan.root.children[0].children[0].children[0].children[0];
        assert_eq!(scan.estimate.total_cost, Some(101.25));
        assert_eq!(scan.estimate.rows, Some(1000.0));
        assert!(scan.details.contains(&Detail::new(
            "Attached condition",
            "(`dbrover`.`c`.`country` = 'ES')"
        )));
        assert!(scan.details.contains(&Detail::new("Prefix cost", "101.25")));
        assert_eq!(costliest(&plan), "Index lookup on o using customer_id");
    }

    #[test]
    fn reads_subqueries_and_unions_in_the_first_format() {
        let plan = fixture("shapes_v1");
        let titles = titles(&plan);
        // A union's block has no number of its own; its parts do.
        assert_eq!(titles[0], (0, "Select".to_owned()));
        assert_eq!(titles[1], (1, "Union".to_owned()));
        assert_eq!(titles[2], (2, "Select #1".to_owned()));
        assert!(
            titles.iter().any(|(_, title)| title == "Select #2"),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|(_, title)| title == "Materialize"),
            "{titles:?}"
        );
    }

    #[test]
    fn reads_mariadbs_analysed_plan_with_its_times_spread_over_loops() {
        let plan = fixture("join_mariadb_analysed");
        assert!(plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Time);
        let outline = plan.outline();
        let tables: Vec<(String, Option<f64>)> = outline
            .iter()
            .filter(|line| line.step.target.is_some())
            .map(|line| {
                (
                    line.step.title(),
                    line.step.actual.and_then(|actual| actual.rows),
                )
            })
            .collect();
        assert_eq!(
            tables,
            [
                ("Table scan on c".to_owned(), Some(1000.0)),
                ("Index lookup on o using customer_id".to_owned(), Some(20.0)),
            ]
        );
        assert_eq!(plan.details[0].label, "Query optimization");
        let text = plan.text();
        assert!(text.contains("(actual time="), "{text}");
    }

    #[test]
    fn reads_mariadbs_subquery_cache_and_materialized_tables() {
        let plan = fixture("shapes_mariadb_analysed");
        let outline = plan.outline();
        let cached = outline
            .iter()
            .find(|line| line.step.operation == "Subquery cache")
            .unwrap();
        assert_eq!(cached.step.heading.as_deref(), Some("Subqueries"));
        let lookup = outline
            .iter()
            .find(|line| line.step.title() == "Index lookup on o2 using customer_id")
            .unwrap();
        assert_eq!(lookup.step.actual.map(|actual| actual.loops), Some(546.0));
        assert!(
            outline
                .iter()
                .any(|line| line.step.operation == "Materialize")
        );
        // The union's second part reads no table.
        assert!(
            outline
                .iter()
                .any(|line| line.step.operation == "No tables used")
        );
    }
}
