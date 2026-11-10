//! The Profile's data funnel (`docs/reports-at-scale.md`, "A timeline" and RS2c): for each
//! source of a kept run, what it returned to DbRover, what the combine step used of it (the
//! rows its scans passed on, from DuckDB's profile), and what the run wrote. The widest drop
//! is where a source's query should filter.
//!
//! A pure function of a [`ReportRun`], so it is tested without drawing anything.

use dbrover_plan::{Plan, PlanFormat};
use dbrover_store::{ReportRun, StepMetrics};

/// One source's line of the funnel.
#[derive(Debug, Clone, PartialEq)]
pub struct FunnelLine {
    pub source: String,
    pub returned_rows: u64,
    pub returned_bytes: u64,
    /// The rows the combine's scans of the source's table passed on; `None` when the run kept
    /// no plan, or the plan reads no table of that name.
    pub used_rows: Option<u64>,
}

/// The funnel of `run`: a line a source, in the order they finished, and the most rows an
/// output wrote; `None` when the run read no source.
#[derive(Debug, Clone, PartialEq)]
pub struct Funnel {
    pub lines: Vec<FunnelLine>,
    pub written_rows: Option<u64>,
}

pub fn funnel(run: &ReportRun) -> Option<Funnel> {
    let plan = combine_plan(run);
    let lines: Vec<FunnelLine> = run
        .steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Source(source) => Some(FunnelLine {
                source: source.id.clone(),
                returned_rows: source.rows,
                returned_bytes: source.bytes,
                used_rows: plan.as_ref().and_then(|plan| used_rows(plan, &source.id)),
            }),
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let written_rows = run
        .steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Output(output) => Some(output.rows),
            _ => None,
        })
        .max();
    Some(Funnel {
        lines,
        written_rows,
    })
}

/// The combine step's plan, as the run kept it, if it kept one DbRover can read.
pub fn combine_plan(run: &ReportRun) -> Option<Plan> {
    let text = run.steps.iter().find_map(|step| match step {
        StepMetrics::Combine(combine) => combine.plan.as_deref(),
        _ => None,
    })?;
    match PlanFormat::DuckDbJson.parse(text) {
        Ok(plan) => Some(plan),
        Err(error) => {
            tracing::debug!("the run's combine plan can't be read: {error}");
            None
        }
    }
}

/// The rows the plan's scans of `table` passed on, all scans of it together. A source's
/// table is named after the source, which DuckDB matches without regard to case.
fn used_rows(plan: &Plan, table: &str) -> Option<u64> {
    let rows: Vec<f64> = plan
        .table_reads()
        .into_iter()
        .filter(|read| read.table.eq_ignore_ascii_case(table))
        .filter_map(|read| read.rows)
        .collect();
    if rows.is_empty() {
        return None;
    }
    // Rows are whole numbers that DuckDB counts in 64 bits.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = rows.iter().sum::<f64>().round() as u64;
    Some(total)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use dbrover_store::{
        CombineMetrics, OutputMetrics, ReportName, RunOrigin, RunOutcome, RunProfile, SourceMetrics,
    };

    use super::*;

    /// A profile of `SELECT … FROM tx JOIN prices …` whose scan of `tx` passed on 120 rows and
    /// of `prices` 4.
    const PLAN: &str = r#"{"latency": 0.01, "children": [{"operator_name": "HASH_JOIN",
        "operator_timing": 0.001, "operator_cardinality": 120, "extra_info": {},
        "children": [
          {"operator_name": "TABLE_SCAN", "operator_timing": 0.002, "operator_cardinality": 120,
           "extra_info": {"Table": "tx", "Projections": ["wallet", "amount"]}, "children": []},
          {"operator_name": "TABLE_SCAN", "operator_timing": 0.001, "operator_cardinality": 4,
           "extra_info": {"Table": "main.prices", "Projections": "asset"}, "children": []}
        ]}]}"#;

    fn source(id: &str, rows: u64) -> StepMetrics {
        StepMetrics::Source(SourceMetrics {
            id: id.to_owned(),
            data_source: "db".to_owned(),
            rows,
            bytes: rows * 10,
            first_row_ms: None,
            elapsed_ms: 1,
            batches: None,
            started_ms: None,
            engine: None,
            waits_for: None,
            phases: None,
            limits: None,
            columns: Vec::new(),
        })
    }

    fn run(steps: Vec<StepMetrics>) -> ReportRun {
        ReportRun {
            id: None,
            report: ReportName::new("r").unwrap(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_secs(1),
            outcome: RunOutcome::Finished,
            params: BTreeMap::new(),
            steps,
            origin: RunOrigin::App,
            version: None,
            profile: RunProfile::default(),
        }
    }

    fn combine(plan: Option<&str>) -> StepMetrics {
        StepMetrics::Combine(CombineMetrics {
            rows: 3,
            bytes: 30,
            first_row_ms: None,
            elapsed_ms: 1,
            peak_memory: None,
            peak_temp_size: None,
            started_ms: None,
            query_ms: None,
            limits: None,
            plan: plan.map(str::to_owned),
        })
    }

    fn output(rows: u64) -> StepMetrics {
        StepMetrics::Output(OutputMetrics {
            number: 1,
            file: "out.csv".to_owned(),
            rows,
            size: 1,
            elapsed_ms: 1,
            started_ms: None,
            ended_ms: None,
        })
    }

    #[test]
    fn says_what_each_source_returned_what_the_combine_used_and_what_was_written() {
        let funnel = funnel(&run(vec![
            source("TX", 5_000),
            source("prices", 4),
            source("unused", 7),
            combine(Some(PLAN)),
            output(3),
        ]));
        let funnel = funnel.unwrap();
        let used: Vec<(&str, Option<u64>)> = funnel
            .lines
            .iter()
            .map(|line| (line.source.as_str(), line.used_rows))
            .collect();
        assert_eq!(
            used,
            [("TX", Some(120)), ("prices", Some(4)), ("unused", None)]
        );
        assert_eq!(funnel.lines[0].returned_bytes, 50_000);
        assert_eq!(funnel.written_rows, Some(3));
    }

    #[test]
    fn leaves_the_combines_share_out_without_a_plan_it_can_read() {
        for plan in [None, Some("not a plan")] {
            let funnel = funnel(&run(vec![source("tx", 5), combine(plan)]));
            let funnel = funnel.unwrap();
            assert_eq!(funnel.lines[0].used_rows, None);
            assert_eq!(funnel.written_rows, None);
        }
        assert_eq!(super::funnel(&run(Vec::new())), None);
    }
}
