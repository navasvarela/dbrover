//! ClickHouse's `EXPLAIN json = 1, indexes = 1`: an array holding one object whose `Plan` is the
//! root step, each step with its `Node Type`, a `Description` and its children under `Plans`.
//! A step reading a MergeTree table names the table in its description and lists the indexes
//! it used under `Indexes`, each with the parts and granules it kept of those it had, which
//! say how much of the table the query reads. ClickHouse estimates no cost or rows here.

use serde_json::Value;

use crate::json::{Object, number, shown, text};
use crate::{Detail, Estimate, Plan, PlanError, Step};

/// The keys a step is built from, which aren't repeated among its details. `Node Id` names a
/// step for ClickHouse's own tools.
const SHOWN_ELSEWHERE: &[&str] = &["Node Type", "Node Id", "Description", "Plans", "Indexes"];

/// Node types that read a table, whose description is the table's name.
const READS: &[&str] = &["ReadFromMergeTree", "ReadFromStorage", "ReadFromRemote"];

pub(crate) fn parse(text: &str) -> Result<Plan, PlanError> {
    let value: Value = serde_json::from_str(text)?;
    let root = value
        .as_array()
        .and_then(|plans| plans.first())
        .and_then(Value::as_object)
        .and_then(|explained| explained.get("Plan"))
        .and_then(Value::as_object)
        .ok_or_else(|| PlanError::Shape("no Plan in the first element".to_owned()))?;
    Ok(Plan {
        root: step(root)?,
        details: Vec::new(),
    })
}

fn step(node: &Object) -> Result<Step, PlanError> {
    let operation = text(node, "Node Type")
        .ok_or_else(|| PlanError::Shape("a step has no Node Type".to_owned()))?;
    let description = text(node, "Description").filter(|description| !description.is_empty());
    let reads = READS.contains(&operation.as_str());
    let (target, mut details) = match description {
        Some(table) if reads => (Some(table), Vec::new()),
        Some(description) => (None, vec![Detail::new("Description", description)]),
        None => (None, Vec::new()),
    };
    details.extend(
        node.iter()
            .filter(|(key, _)| !SHOWN_ELSEWHERE.contains(&key.as_str()))
            .map(|(key, value)| Detail::new(key, shown(value))),
    );
    details.extend(indexes(node));
    let children = node
        .get("Plans")
        .and_then(Value::as_array)
        .map(|plans| {
            plans
                .iter()
                .filter_map(Value::as_object)
                .map(step)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Step {
        operation,
        target,
        heading: None,
        estimate: Estimate::default(),
        actual: None,
        details,
        children,
    })
}

/// An index a read used, as a fact: `PrimaryKey: 44 of 46 granules, 44 of 46 parts; keys
/// region, ts; and((region in ['EU', 'EU']), …)`.
fn indexes(node: &Object) -> Vec<Detail> {
    let Some(indexes) = node.get("Indexes").and_then(Value::as_array) else {
        return Vec::new();
    };
    indexes
        .iter()
        .filter_map(Value::as_object)
        .map(|index| {
            let kind = text(index, "Type").unwrap_or_else(|| "Index".to_owned());
            let mut parts = Vec::new();
            for (unit, selected, initial) in [
                ("granules", "Selected Granules", "Initial Granules"),
                ("parts", "Selected Parts", "Initial Parts"),
            ] {
                if let (Some(selected), Some(initial)) =
                    (number(index, selected), number(index, initial))
                {
                    parts.push(format!("{selected} of {initial} {unit}"));
                }
            }
            if let Some(keys) = index.get("Keys") {
                parts.push(format!("keys {}", shown(keys)));
            }
            if let Some(condition) = text(index, "Condition") {
                parts.push(condition);
            }
            Detail::new(kind, parts.join("; "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::PlanFormat;

    #[test]
    fn reads_a_plan_with_the_granules_each_index_kept() {
        let path = format!(
            "{}/fixtures/clickhouse/explain.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let plan = PlanFormat::ClickHouseJson.parse(&text).unwrap();
        assert_eq!(plan.root.operation, "Expression");
        assert_eq!(
            plan.root.details,
            [Detail::new("Description", "(Project names + Projection)")]
        );
        let read = plan
            .root
            .find(&|step| step.operation == "ReadFromMergeTree")
            .unwrap();
        assert_eq!(read.title(), "ReadFromMergeTree dbrover.events");
        assert_eq!(
            read.details[2],
            Detail::new(
                "PrimaryKey",
                "44 of 44 granules; 44 of 44 parts; keys region, ts; \
                 and((region in ['EU', 'EU']), (ts in [1772323200, +Inf)))"
            )
        );
        assert_eq!(read.details[0].label, "MinMax");
        assert!(
            read.details[0].value.starts_with("44 of 46 granules"),
            "{}",
            read.details[0].value
        );
    }

    #[test]
    fn refuses_what_isnt_a_plan() {
        for text in ["[]", "{}", r#"[{"Plan": {"Plans": []}}]"#] {
            assert!(PlanFormat::ClickHouseJson.parse(text).is_err(), "{text}");
        }
    }
}
