//! MySQL's iterator plans (`explain_json_format_version = 2`): each step an object with its
//! `operation`, written as `EXPLAIN FORMAT=TREE` writes it (`Table scan on c`, `Filter: (…)`),
//! its estimates and, under `EXPLAIN ANALYZE`, its actual figures, with the steps it reads
//! from under `inputs` and its subqueries under keys such as `inputs_from_select_list`.

use serde_json::Value;

use crate::json::{Object, label, number, shown, text};
use crate::{Actual, Detail, Estimate, Plan, Step, Timing};

/// The keys a step is built from, which aren't repeated among its details. `access_type` is
/// left out too: it names the iterator's kind, which the operation already says in words.
const SHOWN_ELSEWHERE: &[&str] = &[
    "operation",
    "heading",
    "access_type",
    "query",
    "estimated_rows",
    "estimated_total_cost",
    "estimated_first_row_cost",
    "actual_rows",
    "actual_loops",
    "actual_first_row_ms",
    "actual_last_row_ms",
];

pub(super) fn plan(root: &Object) -> Plan {
    Plan {
        root: step(root),
        details: text(root, "query")
            .map(|query| Detail::new("Query", query))
            .into_iter()
            .collect(),
    }
}

fn step(node: &Object) -> Step {
    let (steps, facts): (Vec<_>, Vec<_>) = node
        .iter()
        .filter(|(key, _)| !SHOWN_ELSEWHERE.contains(&key.as_str()))
        .partition(|(_, value)| holds_steps(value));
    Step {
        operation: text(node, "operation").unwrap_or_default(),
        target: None,
        heading: text(node, "heading"),
        estimate: Estimate {
            startup_cost: number(node, "estimated_first_row_cost"),
            total_cost: number(node, "estimated_total_cost"),
            rows: number(node, "estimated_rows"),
        },
        actual: number(node, "actual_loops").map(|loops| Actual {
            rows: number(node, "actual_rows"),
            loops,
            time: number(node, "actual_last_row_ms").map(|last_row_ms| Timing {
                first_row_ms: number(node, "actual_first_row_ms"),
                last_row_ms,
            }),
        }),
        details: facts
            .into_iter()
            .map(|(key, value)| Detail::new(label(key), shown(value)))
            .collect(),
        children: steps
            .into_iter()
            .filter_map(|(_, value)| value.as_array())
            .flatten()
            .filter_map(Value::as_object)
            .map(step)
            .collect(),
    }
}

/// Whether the value is a list of steps: `inputs`, or the subqueries a step evaluates.
fn holds_steps(value: &Value) -> bool {
    match value {
        Value::Array(items) => {
            !items.is_empty()
                && items.iter().all(|item| {
                    item.as_object()
                        .is_some_and(|item| item.contains_key("operation"))
                })
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::super::tests::{costliest, fixture, titles};
    use crate::{Detail, Measure};

    #[test]
    fn reads_an_estimated_join_as_its_iterators() {
        let plan = fixture("join_v2");
        assert!(!plan.is_analysed());
        let titles = titles(&plan);
        assert_eq!(titles[0], (0, "Limit: 10 row(s)".to_owned()));
        assert!(
            titles.contains(&(4, "Nested loop inner join".to_owned())),
            "{titles:?}"
        );
        assert!(
            titles.contains(&(6, "Table scan on c".to_owned())),
            "{titles:?}"
        );
        assert!(plan.details[0].value.starts_with("/* select#1 */ select"));
        // The aggregate and the sort above the join have no cost of their own in MySQL's plan.
        assert_eq!(costliest(&plan), "Nested loop inner join");
    }

    #[test]
    fn reads_an_analysed_join_with_what_each_iterator_did() {
        let plan = fixture("join_v2_analysed");
        assert!(plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Time);
        let outline = plan.outline();
        let scan = outline
            .iter()
            .find(|line| line.step.title() == "Table scan on c")
            .unwrap();
        let actual = scan.step.actual.unwrap();
        assert_eq!((actual.rows, actual.loops), (Some(1000.0), 1.0));
        assert!(actual.time.unwrap().first_row_ms.is_some());
        assert!(
            scan.step
                .details
                .contains(&Detail::new("Table name", "w10_customers"))
        );
        let text = plan.text();
        assert!(
            text.contains("->  Table scan on c  (cost=101.25 rows=1000) (actual time="),
            "{text}"
        );
    }

    #[test]
    fn reads_subqueries_in_the_select_list_with_their_headings() {
        let plan = fixture("shapes_v2_analysed");
        let outline = plan.outline();
        let subquery = outline
            .iter()
            .find(|line| line.step.operation == "Aggregate: max(o2.total)")
            .unwrap();
        assert_eq!(
            subquery.step.heading.as_deref(),
            Some("Select #2 (subquery in projection; dependent)")
        );
        assert_eq!(subquery.step.actual.map(|actual| actual.loops), Some(546.0));
        assert_eq!(plan.root.operation, "Append");
    }
}
