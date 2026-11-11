//! **Explain report** (`docs/reports-at-scale.md`, "Each source's plan and estimated cost",
//! RS2d): what the request to explain needs from the history, and the explanation the worker
//! sends back as the tab and `dbrover report explain` write it out.

use std::fmt::Write as _;

use dbrover_plan::{Plan, PlanFormat};
use dbrover_report::{
    CombineExplanation, ExplainedPlan, Explanation, KnownTable, LoadedColumn, SourceExplanation,
};
use dbrover_store::{ReportRun, RunOutcome, StepMetrics};

use super::hints::Hint;
use super::plan_hints::{SourceFacts, plan_hints};

/// The tables the newest finished run of `runs` loaded, which the combine step is planned over
/// for a source whose file declares no columns.
pub fn known_tables(runs: &[ReportRun]) -> Vec<KnownTable> {
    let Some(run) = runs.iter().find(|run| run.outcome == RunOutcome::Finished) else {
        return Vec::new();
    };
    run.steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Source(source) if !source.columns.is_empty() => Some(KnownTable {
                id: source.id.clone(),
                columns: source
                    .columns
                    .iter()
                    .map(|column| LoadedColumn {
                        name: column.name.clone(),
                        type_name: column.type_name.clone(),
                        declared: false,
                        bytes: column.bytes,
                    })
                    .collect(),
            }),
            _ => None,
        })
        .collect()
}

/// A plan the engine wrote in a format DbRover reads, read.
pub fn drawn(plan: &ExplainedPlan) -> Option<Result<Plan, String>> {
    let ExplainedPlan::Drawn { format, text } = plan else {
        return None;
    };
    let Some(format) = PlanFormat::named(format) else {
        return Some(Err(format!(
            "{format:?} isn't a plan format this DbRover reads"
        )));
    };
    Some(format.parse(text).map_err(|error| error.to_string()))
}

/// The combine step's plan, read.
pub fn combine_plan(explanation: &Explanation) -> Option<Result<Plan, String>> {
    match explanation.combine.as_ref()? {
        CombineExplanation::Planned { text } => Some(
            PlanFormat::DuckDbJson
                .parse(text)
                .map_err(|error| error.to_string()),
        ),
        CombineExplanation::Unknown { .. } | CombineExplanation::Failed { .. } => None,
    }
}

/// The hints the combine step's explained plan gives, with what the newest finished run of
/// `runs` knows of each source: its rows, and its columns' bytes. A source no run has read
/// gets every hint its plan shows.
pub fn hints(explanation: &Explanation, runs: &[ReportRun]) -> Vec<Hint> {
    let Some(Ok(plan)) = combine_plan(explanation) else {
        return Vec::new();
    };
    let last = runs.iter().find(|run| run.outcome == RunOutcome::Finished);
    let facts: Vec<SourceFacts> = explanation
        .sources
        .iter()
        .map(|explained| {
            let ran = last.and_then(|run| {
                run.steps.iter().find_map(|step| match step {
                    StepMetrics::Source(source) if source.id == explained.id => Some(source),
                    _ => None,
                })
            });
            SourceFacts {
                id: explained.id.clone(),
                rows: ran.map(|source| source.rows),
                columns: ran
                    .map(|source| {
                        source
                            .columns
                            .iter()
                            .map(|column| (column.name.clone(), column.bytes))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();
    plan_hints(&plan, &facts)
}

/// The explanation as text for a person: each source's statement, its plan and its estimate,
/// then the combine step's plan.
pub fn text(explanation: &Explanation, hints: &[Hint]) -> String {
    let mut out = String::new();
    for source in &explanation.sources {
        source_text(&mut out, source);
        out.push('\n');
    }
    match &explanation.combine {
        None => {}
        Some(CombineExplanation::Planned { .. }) => {
            out.push_str("The combine step, planned by DuckDB over empty tables:\n");
            match combine_plan(explanation) {
                Some(Ok(plan)) => indented(&mut out, &plan.text()),
                Some(Err(reason)) => indented(&mut out, &format!("can't be read: {reason}")),
                None => {}
            }
        }
        Some(CombineExplanation::Unknown { reason }) => {
            let _ = writeln!(out, "The combine step isn't explained. {reason}");
        }
        Some(CombineExplanation::Failed { message }) => {
            let _ = writeln!(out, "DuckDB couldn't plan the combine step: {message}");
        }
    }
    if !hints.is_empty() {
        out.push_str("\nHints:\n");
        for hint in hints {
            let _ = writeln!(out, "  {} ({})", hint.text, hint.id);
        }
    }
    out
}

fn source_text(out: &mut String, source: &SourceExplanation) {
    let _ = writeln!(out, "Source {} on {}:", source.id, source.data_source);
    indented(out, &source.explained.statement);
    for note in &source.notes {
        let _ = writeln!(out, "  {note}");
    }
    match &source.explained.plan {
        plan @ ExplainedPlan::Drawn { .. } => match drawn(plan) {
            Some(Ok(plan)) => {
                out.push_str("  Plan:\n");
                indented(out, &plan.text());
            }
            Some(Err(reason)) => {
                let _ = writeln!(out, "  The plan can't be read: {reason}");
            }
            None => {}
        },
        ExplainedPlan::Text { text } => {
            out.push_str("  Plan:\n");
            indented(out, text);
        }
        ExplainedPlan::Unexplained { reason } => {
            let _ = writeln!(out, "  Not explained: {reason}");
        }
        ExplainedPlan::Failed { message } => {
            let _ = writeln!(out, "  Couldn't be explained: {message}");
        }
    }
    if let Some(estimate) = &source.explained.estimate {
        let _ = writeln!(out, "  Cost: {}", estimate.says);
        if let Some(limit) = estimate.limit_bytes {
            let _ = writeln!(
                out,
                "  Its data source asks above {}{}",
                dbrover_core::ByteCount(limit),
                if estimate.refused {
                    ": a run would refuse this source."
                } else {
                    "."
                }
            );
        } else if estimate.refused {
            out.push_str("  Its data source's cost guard would refuse it in a run.\n");
        }
    }
}

/// `text`, each line four spaces in.
fn indented(out: &mut String, text: &str) {
    for line in text.lines() {
        let _ = writeln!(out, "    {line}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use dbrover_report::{Estimate, SourceExplained};
    use dbrover_store::{KeptColumn, ReportName, RunOrigin, RunProfile, SourceMetrics};

    use super::*;

    fn run(outcome: RunOutcome, columns: Vec<KeptColumn>) -> ReportRun {
        ReportRun {
            id: None,
            report: ReportName::new("r").unwrap(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_secs(1),
            outcome,
            params: BTreeMap::new(),
            steps: vec![StepMetrics::Source(SourceMetrics {
                id: "tx".to_owned(),
                data_source: "db".to_owned(),
                rows: 1,
                bytes: 1,
                first_row_ms: None,
                elapsed_ms: 1,
                batches: None,
                started_ms: None,
                engine: None,
                waits_for: None,
                phases: None,
                limits: None,
                columns,
                scanned_bytes: None,
                parts: Vec::new(),
            })],
            origin: RunOrigin::App,
            version: None,
            profile: RunProfile::default(),
        }
    }

    #[test]
    fn knows_the_tables_of_the_newest_finished_run() {
        let column = |name: &str| KeptColumn {
            name: name.to_owned(),
            type_name: "BIGINT".to_owned(),
            bytes: Some(4),
        };
        let runs = [
            run(RunOutcome::Cancelled, vec![column("newer")]),
            run(RunOutcome::Finished, vec![column("id")]),
        ];
        let known = known_tables(&runs);
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].id, "tx");
        assert_eq!(known[0].columns[0].name, "id");
        assert_eq!(known_tables(&runs[..1]), []);
    }

    #[test]
    fn writes_each_sources_plan_and_estimate_and_why_the_combine_isnt_explained() {
        let explanation = Explanation {
            sources: vec![SourceExplanation {
                id: "events".to_owned(),
                data_source: "warehouse".to_owned(),
                explained: SourceExplained {
                    statement: "SELECT * FROM events".to_owned(),
                    plan: ExplainedPlan::Unexplained {
                        reason: "BigQuery has no EXPLAIN.".to_owned(),
                    },
                    estimate: Some(Estimate {
                        says: "This scans 2.0 GB, billed whatever it returns.".to_owned(),
                        bytes: Some(2_000_000_000),
                        at_least: false,
                        limit_bytes: Some(1_000_000_000),
                        refused: true,
                    }),
                },
                notes: Vec::new(),
            }],
            combine: Some(CombineExplanation::Unknown {
                reason: "Run it once.".to_owned(),
            }),
        };
        assert_eq!(
            text(&explanation, &[]),
            "Source events on warehouse:\n    SELECT * FROM events\n  Not explained: BigQuery \
             has no EXPLAIN.\n  Cost: This scans 2.0 GB, billed whatever it returns.\n  Its \
             data source asks above 1.0 GB: a run would refuse this source.\n\nThe combine \
             step isn't explained. Run it once.\n"
        );
    }
}
