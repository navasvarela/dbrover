//! DuckDB's plans, in the two JSON shapes it writes them:
//!
//! - `EXPLAIN (FORMAT JSON)`: an array holding the root operator, each operator an object with
//!   its `name`, its `children` and its `extra_info`, where DuckDB puts everything else it says
//!   about the operator, `Estimated Cardinality` among it;
//! - the profiler's tree after a statement ran, as DuckDB's JSON profile writes it: a root for
//!   the query (its `latency`, `rows_returned`) whose `children` are operators, each with its
//!   `operator_name`, `operator_type`, `operator_timing` in seconds, `operator_cardinality`
//!   and the same `extra_info`.
//!
//! An `extra_info` value of several lines is written as a list of them. The profiler names
//! some keys as DuckDB does internally, `__estimated_cardinality__` for `Estimated
//! Cardinality`; the reader names them as `EXPLAIN` does. A scan names its table with its
//! catalog and schema: `memory.main.orders`.
//!
//! The profiler times each operator on its own, where every other engine's plan includes the
//! children's time in a step's; the reader adds the children's in, so DuckDB's plans are weighed
//! as the others are ([`crate::weigh`]).

use serde_json::Value;

use crate::json::{Object, number, text};
use crate::{Actual, Detail, Estimate, Plan, PlanError, Step, Timing};

/// The `extra_info` key of the operator's estimate of its rows.
const ESTIMATED_ROWS: &str = "Estimated Cardinality";
/// The `extra_info` key naming the table a scan reads, which becomes the step's target.
const TABLE: &str = "Table";

const MILLISECONDS_IN_A_SECOND: f64 = 1_000.0;

pub(crate) fn parse(text: &str) -> Result<Plan, PlanError> {
    let value: Value = serde_json::from_str(text)?;
    let shape = |message: &str| PlanError::Shape(message.to_owned());
    let root = match &value {
        Value::Array(operators) => operators
            .first()
            .and_then(Value::as_object)
            .ok_or_else(|| shape("the plan's array holds no operator"))?,
        Value::Object(object) if is_operator(object) => object,
        Value::Object(query) => {
            let operator = children(query)
                .first()
                .copied()
                .ok_or_else(|| shape("the profile holds no operator"))?;
            return Ok(Plan {
                root: step(operator)?,
                details: query_details(query),
            });
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            return Err(shape("the plan is neither an array nor an object"));
        }
    };
    Ok(Plan {
        root: step(root)?,
        details: Vec::new(),
    })
}

fn is_operator(object: &Object) -> bool {
    ["name", "operator_name", "operator_type"]
        .iter()
        .any(|key| object.contains_key(*key))
}

fn children(object: &Object) -> Vec<&Object> {
    object
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

/// The profile's facts about the whole query.
fn query_details(query: &Object) -> Vec<Detail> {
    let mut details = Vec::new();
    if let Some(latency) = number(query, "latency") {
        details.push(Detail::new(
            "Latency",
            format!("{:.3} ms", latency * MILLISECONDS_IN_A_SECOND),
        ));
    }
    if let Some(rows) = number(query, "rows_returned") {
        details.push(Detail::new("Rows returned", format!("{rows}")));
    }
    details
}

fn step(operator: &Object) -> Result<Step, PlanError> {
    let operation = ["operator_name", "name", "operator_type"]
        .iter()
        .find_map(|key| text(operator, key))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| PlanError::Shape("an operator has no name".to_owned()))?;
    let children = children(operator)
        .into_iter()
        .map(step)
        .collect::<Result<Vec<_>, _>>()?;
    let extra: Vec<(String, &Value)> = operator
        .get("extra_info")
        .and_then(Value::as_object)
        .map(|extra| {
            extra
                .iter()
                .map(|(key, value)| (key_name(key), value))
                .collect()
        })
        .unwrap_or_default();
    let value_of = |name: &str| {
        extra
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
    };
    let estimate = Estimate {
        rows: value_of(ESTIMATED_ROWS).and_then(estimated_rows),
        ..Estimate::default()
    };
    let target = value_of(TABLE).map(shown);
    let details = extra
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), TABLE | ESTIMATED_ROWS))
        .map(|(key, value)| Detail::new(key, shown(value)))
        .collect();
    let actual = actual(operator, &children);
    Ok(Step {
        operation,
        target,
        heading: None,
        estimate,
        actual,
        details,
        children,
    })
}

/// What the profiler measured of the operator, its children's time added to its own.
fn actual(operator: &Object, children: &[Step]) -> Option<Actual> {
    let rows = number(operator, "operator_cardinality");
    let own_seconds = number(operator, "operator_timing");
    if rows.is_none() && own_seconds.is_none() {
        return None;
    }
    let time = own_seconds.map(|seconds| {
        let below: f64 = children
            .iter()
            .filter_map(|child| child.actual.and_then(|actual| actual.total_ms()))
            .sum();
        Timing {
            first_row_ms: None,
            last_row_ms: seconds * MILLISECONDS_IN_A_SECOND + below,
        }
    });
    Some(Actual {
        rows,
        loops: 1.0,
        time,
    })
}

/// An `extra_info` key as `EXPLAIN` writes it: the profiler's `__estimated_cardinality__` as
/// `Estimated Cardinality`; any other key as it is.
fn key_name(key: &str) -> String {
    let Some(inner) = key
        .strip_prefix("__")
        .and_then(|rest| rest.strip_suffix("__"))
    else {
        return key.to_owned();
    };
    inner
        .split('_')
        .map(|word| {
            let mut characters = word.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// DuckDB writes its estimate as a number in text, `~` before it where it is a guess.
fn estimated_rows(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().trim_start_matches('~').parse().ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// An `extra_info` value as a fact shows it: a value of several lines keeps them, a line each,
/// since an item such as a filter can hold commas.
fn shown(value: &Value) -> String {
    match value {
        Value::Array(lines) => lines.iter().map(shown).collect::<Vec<_>>().join("\n"),
        Value::String(text) => text.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Heat, Measure, PlanFormat};

    fn fixture(name: &str) -> Plan {
        let path = format!("{}/fixtures/duckdb/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        PlanFormat::DuckDbJson.parse(&text).unwrap()
    }

    #[test]
    fn reads_an_explained_plan_with_its_estimates_and_tables() {
        let plan = fixture("explain");
        assert!(!plan.is_analysed());
        assert_eq!(plan.root.operation, "PROJECTION");
        let scan = plan
            .root
            .find(&|step| step.target.as_deref() == Some("orders"));
        let scan = scan.unwrap();
        assert_eq!(scan.operation, "SEQ_SCAN");
        assert_eq!(scan.estimate.rows, Some(1000.0));
        assert!(
            scan.details
                .contains(&Detail::new("Projections", "region\namount")),
            "{:?}",
            scan.details
        );
    }

    #[test]
    fn reads_a_profile_with_each_operators_time_including_its_childrens() {
        let plan = fixture("profile");
        assert!(plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Time);
        assert_eq!(plan.root.operation, "PROJECTION");
        let actual = plan.root.actual.unwrap();
        assert_eq!(actual.rows, Some(2.0));
        // 1 ms of its own, 2 ms for the aggregate's, 5 ms for the scan's.
        let total = actual.total_ms().unwrap();
        assert!((total - 8.0).abs() < 1e-9, "{total}");
        let outline = plan.outline();
        let costliest = outline
            .iter()
            .find(|line| line.weight.heat == Heat::Costliest)
            .unwrap();
        assert_eq!(costliest.step.title(), "TABLE_SCAN orders");
        assert_eq!(plan.details[0].label, "Latency");
    }

    #[test]
    fn names_the_profilers_keys_as_explain_does() {
        assert_eq!(
            key_name("__estimated_cardinality__"),
            "Estimated Cardinality"
        );
        assert_eq!(key_name("__projections__"), "Projections");
        assert_eq!(key_name("Join Type"), "Join Type");
    }

    #[test]
    fn refuses_what_isnt_a_plan() {
        for text in ["[]", "{}", "3", r#"[{"children": []}]"#] {
            assert!(PlanFormat::DuckDbJson.parse(text).is_err(), "{text}");
        }
    }
}
