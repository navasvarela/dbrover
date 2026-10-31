//! MySQL's and MariaDB's `EXPLAIN FORMAT=JSON`, in either of the two formats they write:
//!
//! - **Iterators** (MySQL 8.3 and later with `explain_json_format_version = 2`): a tree of the
//!   steps MySQL runs, the same tree as `EXPLAIN FORMAT=TREE`, with `EXPLAIN ANALYZE`'s actual
//!   figures when analysed. Read by [`iterators`].
//! - **Query blocks** (MySQL's first format, the only one before 8.3, and MariaDB's): each
//!   `SELECT` a block holding its operations (ordering, grouping, a nested loop of tables)
//!   nested inside each other, with MariaDB's `ANALYZE FORMAT=JSON` figures when analysed.
//!   Read by [`blocks`].
//!
//! Which one a plan is in is told from its root: an `operation`, or a `query_block`.

mod blocks;
mod iterators;

use serde_json::Value;

use crate::{Plan, PlanError};

pub(crate) fn parse(text: &str) -> Result<Plan, PlanError> {
    let value: Value = serde_json::from_str(text)?;
    let root = value
        .as_object()
        .ok_or_else(|| PlanError::Shape("expected an object".to_owned()))?;
    if root.contains_key("operation") {
        Ok(iterators::plan(root))
    } else if let Some(Value::Object(block)) = root.get("query_block") {
        Ok(blocks::plan(root, block))
    } else {
        Err(PlanError::Shape(
            "expected a \"query_block\" or an \"operation\" at the top".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::{Heat, Plan, PlanFormat};

    pub(super) fn fixture(name: &str) -> Plan {
        let path = format!("{}/fixtures/mysql/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        PlanFormat::MysqlJson.parse(&text).unwrap()
    }

    pub(super) fn titles(plan: &Plan) -> Vec<(usize, String)> {
        plan.outline()
            .iter()
            .map(|line| (line.depth, line.step.title()))
            .collect()
    }

    pub(super) fn costliest(plan: &Plan) -> String {
        let outline = plan.outline();
        outline
            .iter()
            .find(|line| line.weight.heat == Heat::Costliest)
            .map(|line| line.step.title())
            .unwrap_or_default()
    }

    #[test]
    fn refuses_json_in_neither_format() {
        let cases = [
            ("[]", "expected an object"),
            (r#"{"steps": []}"#, "\"query_block\" or an \"operation\""),
            ("-> Table scan on t", "isn't JSON"),
        ];
        for (text, expected) in cases {
            let error = PlanFormat::MysqlJson.parse(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}: {error}");
        }
    }
}
