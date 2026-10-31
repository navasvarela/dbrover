//! PostgreSQL's `EXPLAIN (FORMAT JSON)`: an array with an object per planned statement, whose
//! `Plan` is the root step, each step holding its children under `Plans`. Steps are named as
//! PostgreSQL's text format names them, which spells out what the JSON splits into several
//! keys (`HashAggregate` for an `Aggregate` whose strategy is `Hashed`).

use serde_json::{Map, Value};

use crate::{Actual, Detail, Estimate, Plan, PlanError, Step, Timing};

/// The keys a step is built from, which aren't repeated among its details. `Plan Width`,
/// `Parent Relationship` and `Inner Unique` are left out too: PostgreSQL's own text plan only
/// shows them with `VERBOSE`, or as the indentation the tree already shows.
const SHOWN_ELSEWHERE: &[&str] = &[
    "Node Type",
    "Strategy",
    "Partial Mode",
    "Parallel Aware",
    "Async Capable",
    "Join Type",
    "Operation",
    "Scan Direction",
    "Relation Name",
    "Schema",
    "Alias",
    "Index Name",
    "CTE Name",
    "Function Name",
    "Subplan Name",
    "Startup Cost",
    "Total Cost",
    "Plan Rows",
    "Plan Width",
    "Actual Startup Time",
    "Actual Total Time",
    "Actual Rows",
    "Actual Loops",
    "Parent Relationship",
    "Inner Unique",
    "Plans",
];

/// Facts PostgreSQL reports in kilobytes without saying so.
const KILOBYTES: &[&str] = &[
    "Sort Space Used",
    "Peak Memory Usage",
    "Disk Usage",
    "Memory Usage",
    "Original Peak Memory Usage",
];

/// Plan-wide facts PostgreSQL reports in milliseconds without saying so.
const MILLISECONDS: &[&str] = &["Planning Time", "Execution Time", "Serialization Time"];

pub(crate) fn parse(text: &str) -> Result<Plan, PlanError> {
    let value: Value = serde_json::from_str(text)?;
    let statement = value
        .as_array()
        .and_then(|statements| statements.first())
        .and_then(Value::as_object)
        .ok_or_else(|| shape("expected an array holding the statement's plan"))?;
    let root = statement
        .get("Plan")
        .and_then(Value::as_object)
        .ok_or_else(|| shape("the statement has no \"Plan\""))?;
    Ok(Plan {
        root: step(root)?,
        details: plan_details(statement),
    })
}

fn shape(message: &str) -> PlanError {
    PlanError::Shape(message.to_owned())
}

fn step(node: &Map<String, Value>) -> Result<Step, PlanError> {
    let children = match node.get("Plans") {
        Some(Value::Array(plans)) => plans
            .iter()
            .map(|plan| {
                plan.as_object()
                    .ok_or_else(|| shape("a step in \"Plans\" isn't an object"))
                    .and_then(step)
            })
            .collect::<Result<_, _>>()?,
        _ => Vec::new(),
    };
    Ok(Step {
        operation: operation(node)?,
        target: target(node),
        heading: text(node, "Subplan Name"),
        estimate: Estimate {
            startup_cost: number(node, "Startup Cost"),
            total_cost: number(node, "Total Cost"),
            rows: number(node, "Plan Rows"),
        },
        actual: actual(node),
        details: node
            .iter()
            .filter(|(key, _)| !SHOWN_ELSEWHERE.contains(&key.as_str()))
            .map(|(key, value)| Detail::new(key, value_text(key, value)))
            .collect(),
        children,
    })
}

/// The step's name as PostgreSQL's text plan writes it.
fn operation(node: &Map<String, Value>) -> Result<String, PlanError> {
    let node_type = text(node, "Node Type").ok_or_else(|| shape("a step has no \"Node Type\""))?;
    let mut name = match node_type.as_str() {
        "Aggregate" => aggregate(text(node, "Strategy").as_deref()).to_owned(),
        "SetOp" if text(node, "Strategy").as_deref() == Some("Hashed") => "HashSetOp".to_owned(),
        "ModifyTable" => text(node, "Operation").unwrap_or(node_type),
        _ => node_type,
    };
    if let Some(join) = text(node, "Join Type").filter(|join| join != "Inner") {
        // "Hash Join" with a left join is "Hash Left Join"; "Nested Loop" keeps its name.
        let method = name.strip_suffix(" Join").unwrap_or(&name);
        name = format!("{method} {join} Join");
    }
    if text(node, "Scan Direction").as_deref() == Some("Backward") {
        name.push_str(" Backward");
    }
    let prefixes = [
        (flag(node, "Async Capable"), "Async "),
        (flag(node, "Parallel Aware"), "Parallel "),
    ];
    for (set, prefix) in prefixes {
        if set {
            name.insert_str(0, prefix);
        }
    }
    match text(node, "Partial Mode").as_deref() {
        Some(mode @ ("Partial" | "Finalize")) => Ok(format!("{mode} {name}")),
        _ => Ok(name),
    }
}

fn aggregate(strategy: Option<&str>) -> &'static str {
    match strategy {
        Some("Sorted") => "GroupAggregate",
        Some("Hashed") => "HashAggregate",
        Some("Mixed") => "MixedAggregate",
        _ => "Aggregate",
    }
}

/// What the step reads, in the text plan's words: `using customers_pkey on customers c`,
/// `on orders o`, `on generate_series g`.
fn target(node: &Map<String, Value>) -> Option<String> {
    let alias = text(node, "Alias");
    let named = text(node, "Relation Name")
        .or_else(|| text(node, "CTE Name"))
        .or_else(|| text(node, "Function Name"))
        .map(|name| match text(node, "Schema") {
            Some(schema) => format!("{schema}.{name}"),
            None => name,
        });
    let read = match (named, alias) {
        (Some(name), Some(alias)) if alias != name => Some(format!("{name} {alias}")),
        (Some(name), _) => Some(name),
        // A subquery scan has only its alias.
        (None, alias) => alias,
    };
    match (text(node, "Index Name"), read) {
        (Some(index), Some(read)) => Some(format!("using {index} on {read}")),
        (Some(index), None) => Some(format!("on {index}")),
        (None, Some(read)) => Some(format!("on {read}")),
        (None, None) => None,
    }
}

fn actual(node: &Map<String, Value>) -> Option<Actual> {
    let loops = number(node, "Actual Loops")?;
    let time = match (
        number(node, "Actual Startup Time"),
        number(node, "Actual Total Time"),
    ) {
        (Some(first_row_ms), Some(last_row_ms)) => Some(Timing {
            first_row_ms,
            last_row_ms,
        }),
        _ => None,
    };
    Some(Actual {
        rows: number(node, "Actual Rows").unwrap_or(0.0),
        loops,
        time,
    })
}

/// The statement's own facts, beside its `Plan`: the times, each trigger and anything else.
fn plan_details(statement: &Map<String, Value>) -> Vec<Detail> {
    let mut details = Vec::new();
    for (key, value) in statement {
        match (key.as_str(), value) {
            ("Plan", _) => {}
            ("Triggers", Value::Array(triggers)) => {
                details.extend(triggers.iter().filter_map(Value::as_object).map(trigger));
            }
            (key, value) => details.push(Detail::new(key, value_text(key, value))),
        }
    }
    details
}

/// A trigger's line, as the text plan has it: `Trigger for constraint orders_customer_id_fkey:
/// time=1.234 calls=20`.
fn trigger(trigger: &Map<String, Value>) -> Detail {
    let name = text(trigger, "Constraint Name")
        .map(|constraint| format!("for constraint {constraint}"))
        .or_else(|| text(trigger, "Trigger Name"))
        .unwrap_or_default();
    let mut value = text(trigger, "Relation")
        .map(|relation| format!("on {relation}: "))
        .unwrap_or_default();
    if let Some(time) = number(trigger, "Time") {
        value.push_str(&format!("time={time:.3} "));
    }
    if let Some(calls) = number(trigger, "Calls") {
        value.push_str(&format!("calls={calls}"));
    }
    Detail::new(format!("Trigger {name}").trim_end(), value.trim_end())
}

fn value_text(key: &str, value: &Value) -> String {
    let unit = if KILOBYTES.contains(&key) {
        " kB"
    } else if MILLISECONDS.contains(&key) {
        " ms"
    } else {
        ""
    };
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => format!("{number}{unit}"),
        Value::Array(items) if items.iter().all(|item| !item.is_object()) => items
            .iter()
            .map(|item| value_text(key, item))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn text(node: &Map<String, Value>, key: &str) -> Option<String> {
    node.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(node: &Map<String, Value>, key: &str) -> Option<f64> {
    node.get(key).and_then(Value::as_f64)
}

fn flag(node: &Map<String, Value>, key: &str) -> bool {
    node.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Heat, Measure, PlanFormat};

    fn fixture(name: &str) -> Plan {
        let path = format!(
            "{}/fixtures/postgres/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        PlanFormat::PostgresJson.parse(&text).unwrap()
    }

    fn titles(plan: &Plan) -> Vec<(usize, String)> {
        plan.outline()
            .iter()
            .map(|line| (line.depth, line.step.title()))
            .collect()
    }

    fn costliest(plan: &Plan) -> String {
        let outline = plan.outline();
        let line = outline
            .iter()
            .find(|line| line.weight.heat == Heat::Costliest)
            .unwrap();
        line.step.title()
    }

    #[test]
    fn reads_an_estimated_join_as_the_text_plan_names_its_steps() {
        let plan = fixture("join");
        assert!(!plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Cost);
        assert_eq!(
            titles(&plan),
            [
                (0, "Limit".to_owned()),
                (1, "Sort".to_owned()),
                (2, "HashAggregate".to_owned()),
                (3, "Hash Join".to_owned()),
                (4, "Seq Scan on orders o".to_owned()),
                (4, "Hash".to_owned()),
                (5, "Seq Scan on customers c".to_owned()),
            ]
        );
        let join = &plan.root.children[0].children[0].children[0];
        assert_eq!(
            join.estimate,
            Estimate {
                startup_cost: Some(23.66),
                total_cost: Some(404.38),
                rows: Some(6660.0),
            }
        );
        assert_eq!(
            join.details,
            [Detail::new("Hash Cond", "(o.customer_id = c.id)")]
        );
        assert_eq!(costliest(&plan), "Seq Scan on orders o");
    }

    #[test]
    fn reads_an_analysed_join_with_what_each_step_did() {
        let plan = fixture("join_analysed");
        assert!(plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Time);
        let sort = &plan.root.children[0];
        let actual = sort.actual.unwrap();
        assert_eq!((actual.rows, actual.loops), (10.0, 1.0));
        assert!(actual.time.is_some());
        assert!(
            sort.details
                .contains(&Detail::new("Sort Space Used", "26 kB"))
        );
        assert_eq!(
            plan.details
                .iter()
                .map(|detail| detail.label.as_str())
                .collect::<Vec<_>>(),
            ["Planning Time", "Execution Time"]
        );
        assert!(plan.details[1].value.ends_with(" ms"));
        let text = plan.text();
        assert!(
            text.starts_with("Limit  (cost=449.04..449.07 rows=10) (actual time="),
            "{text}"
        );
        assert!(
            text.contains("\n              Group Key: c.name\n"),
            "{text}"
        );
        assert!(
            text.contains("\n                    ->  Seq Scan on orders o  (cost=0.00..328.00 rows=20000) (actual time="),
            "{text}"
        );
    }

    #[test]
    fn names_joins_subplans_and_steps_that_never_ran() {
        let plan = fixture("shapes_analysed");
        let titles = titles(&plan);
        assert_eq!(titles[0], (0, "Nested Loop Left Join".to_owned()));
        assert!(
            titles.contains(&(
                2,
                "Index Scan using customers_pkey on customers c".to_owned()
            )),
            "{titles:?}"
        );
        assert!(titles.contains(&(2, "Function Scan on generate_series g".to_owned())));
        assert!(titles.contains(&(1, "CTE Scan on per_customer p".to_owned())));
        let outline = plan.outline();
        let cte = outline
            .iter()
            .find(|line| line.step.heading.as_deref() == Some("CTE per_customer"))
            .unwrap();
        assert_eq!(cte.step.operation, "HashAggregate");
        assert_eq!(cte.step.actual.map(|actual| actual.loops), Some(0.0));
        assert_eq!(cte.step.misestimate(), None);
        let text = plan.text();
        assert!(
            text.contains("\n  CTE per_customer\n    ->  HashAggregate"),
            "{text}"
        );
        assert!(text.contains("(never executed)"), "{text}");
    }

    #[test]
    fn reads_a_change_as_its_operation_on_its_table() {
        let plan = fixture("update_analysed");
        assert_eq!(plan.root.title(), "Update on orders");
        let scan = &plan.root.children[0];
        assert_eq!(
            scan.details,
            [
                Detail::new("Filter", "(customer_id = 7)"),
                Detail::new("Rows Removed by Filter", "19980"),
            ]
        );
        assert_eq!(scan.misestimate(), Some(1.0));
    }

    #[test]
    fn names_steps_from_every_key_that_shapes_their_name() {
        let cases = [
            (
                r#"{"Node Type": "Aggregate", "Strategy": "Sorted", "Partial Mode": "Finalize"}"#,
                "Finalize GroupAggregate",
            ),
            (
                r#"{"Node Type": "Seq Scan", "Parallel Aware": true, "Relation Name": "t", "Alias": "t"}"#,
                "Parallel Seq Scan on t",
            ),
            (
                r#"{"Node Type": "Merge Join", "Join Type": "Anti"}"#,
                "Merge Anti Join",
            ),
            (
                r#"{"Node Type": "Index Only Scan", "Scan Direction": "Backward", "Index Name": "i", "Relation Name": "t", "Schema": "s", "Alias": "x"}"#,
                "Index Only Scan Backward using i on s.t x",
            ),
            (
                r#"{"Node Type": "Bitmap Index Scan", "Index Name": "i"}"#,
                "Bitmap Index Scan on i",
            ),
            (
                r#"{"Node Type": "Subquery Scan", "Alias": "sub"}"#,
                "Subquery Scan on sub",
            ),
        ];
        for (node, expected) in cases {
            let node: Map<String, Value> = serde_json::from_str(node).unwrap();
            assert_eq!(step(&node).unwrap().title(), expected, "{node:?}");
        }
    }

    #[test]
    fn refuses_text_that_isnt_a_plan() {
        let cases = [
            ("Seq Scan on t  (cost=0.00..1.00 rows=1)", "isn't JSON"),
            ("[]", "an array holding"),
            (r#"[{"Plan": {"Total Cost": 1}}]"#, "\"Node Type\""),
            (
                r#"[{"Plan": {"Node Type": "Result", "Plans": [1]}}]"#,
                "isn't an object",
            ),
        ];
        for (text, expected) in cases {
            let error = PlanFormat::PostgresJson.parse(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn writes_triggers_as_the_text_plan_does() {
        let text = r#"[{"Plan": {"Node Type": "Result"}, "Triggers": [
            {"Trigger Name": "RI_ConstraintTrigger_c_1", "Constraint Name": "orders_customer_id_fkey",
             "Relation": "orders", "Time": 1.5, "Calls": 20}]}]"#;
        let plan = PlanFormat::PostgresJson.parse(text).unwrap();
        assert_eq!(
            plan.details,
            [Detail::new(
                "Trigger for constraint orders_customer_id_fkey",
                "on orders: time=1.500 calls=20"
            )]
        );
    }
}
