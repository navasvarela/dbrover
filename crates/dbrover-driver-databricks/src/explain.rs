//! The estimate of a Databricks source in Explain report: the bytes `EXPLAIN COST`'s plan says
//! its relations hold (`docs/cloud-data.md`, C6, "`EXPLAIN COST` in Explain report").

use dbrover_core::{ByteCount, ScanEstimate};

/// What `EXPLAIN COST` is put before a statement with.
pub const EXPLAIN_COST: &str = "EXPLAIN COST ";

/// The units Spark writes a size in, `bytesToString`'s: powers of 1,024.
const UNITS: [(&str, f64); 7] = [
    ("EiB", 1_152_921_504_606_846_976.0),
    ("PiB", 1_125_899_906_842_624.0),
    ("TiB", 1_099_511_627_776.0),
    ("GiB", 1_073_741_824.0),
    ("MiB", 1_048_576.0),
    ("KiB", 1_024.0),
    ("B", 1.0),
];

/// The bytes the relations of `plan`, `EXPLAIN COST`'s text, hold: each `Relation` leaf of the
/// optimized logical plan, `Statistics(sizeInBytes=…)`, added up. At most what the statement
/// reads, since Delta's file skipping and the columns a query names read less. `None` for a
/// plan with no relation sized, as one over no table, or one Databricks has no statistics for.
pub fn estimate(plan: &str) -> Option<ScanEstimate> {
    let optimized = plan
        .split_once("== Optimized Logical Plan ==")
        .map_or(plan, |(_, rest)| rest);
    let optimized = optimized
        .split_once("== Physical Plan ==")
        .map_or(optimized, |(before, _)| before);
    let sizes: Vec<f64> = optimized
        .lines()
        .filter(|line| is_relation(line))
        .filter_map(size_in_bytes)
        .collect();
    if sizes.is_empty() {
        return None;
    }
    let bytes: f64 = sizes.iter().sum();
    // A size Spark writes with one decimal, in bytes far below 2⁶³.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(ScanEstimate::at_most(ByteCount(bytes.round() as u64)))
}

/// Whether `line` of a plan's tree is a relation, a table read: its operator, after the tree's
/// `+-` and `:-` marks, is `Relation` or `LocalRelation`.
fn is_relation(line: &str) -> bool {
    let operator =
        line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '+' | '-' | ':' | '|'));
    operator.starts_with("Relation") || operator.starts_with("LocalRelation")
}

/// `sizeInBytes` of a line's `Statistics(…)`: `20.0 B`, `1.5 MiB`, or a plain number.
fn size_in_bytes(line: &str) -> Option<f64> {
    let (_, rest) = line.split_once("sizeInBytes=")?;
    let written = rest.split([',', ')']).next()?.trim();
    match written.split_once(' ') {
        Some((number, unit)) => {
            let factor = UNITS.iter().find(|(name, _)| *name == unit.trim())?.1;
            Some(number.parse::<f64>().ok()? * factor)
        }
        None => written.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::Precision;

    use super::*;

    const PLAN: &str = "== Optimized Logical Plan ==
Aggregate [customer#12], [customer#12, sum(total#13) AS total#20], Statistics(sizeInBytes=1.2 KiB, rowCount=40)
+- Join Inner, (id#11L = order_id#30L), Statistics(sizeInBytes=3.0 MiB)
   :- Project [id#11L, customer#12, total#13], Statistics(sizeInBytes=2.0 MiB)
   :  +- Relation main.sales.orders[id#11L,customer#12,total#13] parquet, Statistics(sizeInBytes=1.5 GiB, rowCount=10000000)
   +- Relation main.sales.lines[order_id#30L] parquet, Statistics(sizeInBytes=512.0 MiB)

== Physical Plan ==
AdaptiveSparkPlan isFinalPlan=false
+- FileScan parquet main.sales.orders, Statistics(sizeInBytes=9.0 GiB)";

    #[test]
    fn the_estimate_adds_up_the_relations_of_the_optimized_plan() {
        let estimate = estimate(PLAN).expect("an estimate");
        assert_eq!(estimate.precision, Precision::AtMost);
        assert_eq!(estimate.bytes, ByteCount(1_610_612_736 + 536_870_912));
    }

    #[test]
    fn a_plan_without_a_sized_relation_has_no_estimate() {
        assert_eq!(
            estimate("== Optimized Logical Plan ==\nProject [1 AS one#0]\n+- OneRowRelation"),
            None
        );
        assert_eq!(
            size_in_bytes("Statistics(sizeInBytes=20.0 B, rowCount=1)"),
            Some(20.0)
        );
        assert_eq!(
            size_in_bytes("Statistics(sizeInBytes=8.0 EiB)"),
            Some(8.0 * 1_152_921_504_606_846_976.0)
        );
        assert_eq!(size_in_bytes("Statistics(sizeInBytes=12)"), Some(12.0));
    }
}
