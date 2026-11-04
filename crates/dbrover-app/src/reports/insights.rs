//! What a report's runs say (`docs/reporting.md`, "The report's tab in RP4"): the time each
//! step spent on its own work, the step that dominates a run, and two hints from the newest
//! finished run.

use std::fmt;

use dbrover_core::ByteCount;
use dbrover_report::{HintId, grouped};
use dbrover_store::{ReportRun, RunOutcome, StepMetrics};

/// A source must read at least this many rows for the filter hint: below it, filtering in the
/// source's query saves too little to mention.
const FILTER_HINT_MIN_ROWS: u64 = 10_000;
/// ... and at least this many times the rows the outputs write.
const FILTER_HINT_RATIO: u64 = 10;

/// A step of a report, the same one from one run to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKey {
    Source(String),
    Combine,
    /// Counted from 1. An output's file name can change with the parameters; its place can't.
    Output(usize),
}

impl StepKey {
    pub fn of(step: &StepMetrics) -> Self {
        match step {
            StepMetrics::Source(source) => Self::Source(source.id.clone()),
            StepMetrics::Attached { id, .. } => Self::Source(id.clone()),
            StepMetrics::Combine(_) => Self::Combine,
            StepMetrics::Output(output) => Self::Output(output.number),
        }
    }

    /// Where the step comes in the tab's list: by its kind, then an output by its number.
    fn rank(&self) -> (u8, usize) {
        match self {
            Self::Source(_) => (0, 0),
            Self::Combine => (1, 0),
            Self::Output(number) => (2, *number),
        }
    }
}

impl fmt::Display for StepKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(id) => write!(f, "Source {id}"),
            Self::Combine => f.write_str("Combine"),
            Self::Output(number) => write!(f, "Output {number}"),
        }
    }
}

/// Every step any of `runs` measured, sources in the order the newest run finished them,
/// then the combine step and the outputs.
pub fn step_keys(runs: &[ReportRun]) -> Vec<StepKey> {
    let mut keys: Vec<StepKey> = Vec::new();
    for key in runs
        .iter()
        .flat_map(|run| run.steps.iter().map(StepKey::of))
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    // Stable, so sources keep the order they were found in.
    keys.sort_by_key(StepKey::rank);
    keys
}

/// The time `step` of `run` spent on its own work, in milliseconds: `None` for an attached
/// file, which does nothing until the combine step reads it.
///
/// The step whose rows the outputs write (the combine step, or a report's one source without
/// it) writes each page before it reads the next, so its time holds the outputs'. Their time
/// is taken off it here, so that a slow writer shows as the output, not as its feed.
pub fn own_time(run: &ReportRun, step: &StepMetrics) -> Option<u64> {
    let outputs: u64 = run
        .steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Output(output) => Some(output.elapsed_ms),
            _ => None,
        })
        .sum();
    match step {
        StepMetrics::Attached { .. } => None,
        StepMetrics::Combine(combine) => Some(combine.elapsed_ms.saturating_sub(outputs)),
        StepMetrics::Source(source) if feeds_outputs(run) => {
            Some(source.elapsed_ms.saturating_sub(outputs))
        }
        StepMetrics::Source(source) => Some(source.elapsed_ms),
        StepMetrics::Output(output) => Some(output.elapsed_ms),
    }
}

/// Whether a source's rows go straight to the outputs: a run without a combine step.
fn feeds_outputs(run: &ReportRun) -> bool {
    !run.steps
        .iter()
        .any(|step| matches!(step, StepMetrics::Combine(_)))
}

/// The step that took the longest share of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Dominant {
    pub step: StepKey,
    /// Of the run's time, from 0 to 1. Sources of one round run at the same time, so the
    /// shares of a run's steps can add up to more than 1.
    pub share: f64,
}

/// The step that took the longest share of `run`, if it measured one and took any time.
pub fn dominant(run: &ReportRun) -> Option<Dominant> {
    let total = u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX);
    if total == 0 {
        return None;
    }
    run.steps
        .iter()
        .filter_map(|step| Some((StepKey::of(step), own_time(run, step)?)))
        .max_by_key(|(_, time)| *time)
        .map(|(step, time)| Dominant {
            step,
            share: (time as f64 / total as f64).min(1.0),
        })
}

/// The step that dominates every finished run of `runs`, when they agree, and its share of
/// the newest of them.
pub fn dominant_throughout(runs: &[ReportRun]) -> Option<Dominant> {
    let mut finished = runs
        .iter()
        .filter(|run| run.outcome == RunOutcome::Finished);
    let newest = dominant(finished.next()?)?;
    finished
        .all(|run| dominant(run).is_some_and(|other| other.step == newest.step))
        .then_some(newest)
}

/// Something to change, from what a run measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hint {
    /// A source read far more rows than the outputs wrote.
    Filter {
        source: String,
        read: u64,
        written: u64,
    },
    /// The combine step held this many bytes on disk at its peak.
    Spilled { bytes: u64 },
}

impl Hint {
    /// The id a report's `quiet_hints` silences it by.
    pub fn id(&self) -> HintId {
        match self {
            Self::Filter { .. } => HintId::SourceReadsMore,
            Self::Spilled { .. } => HintId::CombineSpilled,
        }
    }
}

/// The hints the newest finished run of `runs` gives.
pub fn hints(runs: &[ReportRun]) -> Vec<Hint> {
    let Some(run) = runs.iter().find(|run| run.outcome == RunOutcome::Finished) else {
        return Vec::new();
    };
    let written = run
        .steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Output(output) => Some(output.rows),
            _ => None,
        })
        .max();
    run.steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Source(source) => {
                let written = written?;
                let reads_far_more = source.rows >= FILTER_HINT_MIN_ROWS
                    && source.rows >= written.saturating_mul(FILTER_HINT_RATIO);
                reads_far_more.then(|| Hint::Filter {
                    source: source.id.clone(),
                    read: source.rows,
                    written,
                })
            }
            StepMetrics::Combine(combine) => combine
                .peak_temp_size
                .filter(|bytes| *bytes > 0)
                .map(|bytes| Hint::Spilled { bytes }),
            StepMetrics::Attached { .. } | StepMetrics::Output(_) => None,
        })
        .collect()
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Filter {
                source,
                read,
                written,
            } => write!(
                f,
                "Source {source} read {} rows for {} written. If the combine step drops most of \
                 them, filter them in the source's query, where its database can use an index.",
                grouped(*read),
                grouped(*written)
            ),
            Self::Spilled { bytes } => write!(
                f,
                "Combine spilled {} to disk, past its memory_limit. A larger memory_limit in \
                 [combine], or fewer rows from the sources, keeps it in memory.",
                ByteCount(*bytes)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_store::{CombineMetrics, OutputMetrics, ReportName, RunError, SourceMetrics};
    use jiff::Timestamp;

    use super::*;

    fn source(id: &str, rows: u64, elapsed_ms: u64) -> StepMetrics {
        StepMetrics::Source(SourceMetrics {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            rows,
            bytes: rows * 20,
            first_row_ms: Some(1),
            elapsed_ms,
            batches: None,
        })
    }

    fn combine(elapsed_ms: u64, peak_temp_size: Option<u64>) -> StepMetrics {
        StepMetrics::Combine(CombineMetrics {
            rows: 40,
            bytes: 800,
            first_row_ms: Some(1),
            elapsed_ms,
            peak_memory: Some(1 << 30),
            peak_temp_size,
        })
    }

    fn output(number: usize, rows: u64, elapsed_ms: u64) -> StepMetrics {
        StepMetrics::Output(OutputMetrics {
            number,
            file: format!("values-{number}.csv"),
            rows,
            size: rows * 30,
            elapsed_ms,
        })
    }

    fn run(elapsed_ms: u64, outcome: RunOutcome, steps: Vec<StepMetrics>) -> ReportRun {
        ReportRun {
            report: ReportName::new("values").unwrap(),
            started_at: Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_millis(elapsed_ms),
            outcome,
            params: Default::default(),
            steps,
        }
    }

    fn finished(elapsed_ms: u64, steps: Vec<StepMetrics>) -> ReportRun {
        run(elapsed_ms, RunOutcome::Finished, steps)
    }

    #[test]
    fn takes_the_outputs_time_off_the_step_that_feeds_them() {
        let combined = finished(
            1_000,
            vec![
                source("tx", 10, 300),
                combine(600, None),
                output(1, 40, 500),
            ],
        );
        let direct = finished(1_000, vec![source("tx", 10, 900), output(1, 10, 700)]);
        let attached = StepMetrics::Attached {
            id: "prices".to_owned(),
            data_source: "prices".to_owned(),
        };
        let cases = [
            (&combined, &combined.steps[0], Some(300)),
            (&combined, &combined.steps[1], Some(100)),
            (&combined, &combined.steps[2], Some(500)),
            (&direct, &direct.steps[0], Some(200)),
            (&combined, &attached, None),
        ];
        for (run, step, time) in cases {
            assert_eq!(own_time(run, step), time, "{step:?}");
        }
    }

    #[test]
    fn names_the_step_with_the_longest_share() {
        let cases = [
            (
                finished(1_000, vec![source("tx", 10, 800), combine(150, None)]),
                Some((StepKey::Source("tx".to_owned()), 0.8)),
            ),
            (
                finished(
                    1_000,
                    vec![
                        source("tx", 10, 100),
                        combine(900, None),
                        output(1, 40, 600),
                    ],
                ),
                Some((StepKey::Output(1), 0.6)),
            ),
            (finished(0, vec![source("tx", 10, 0)]), None),
            (finished(1_000, Vec::new()), None),
        ];
        for (run, expected) in cases {
            let found = dominant(&run).map(|dominant| (dominant.step, dominant.share));
            assert_eq!(found, expected, "{:?}", run.steps);
        }
    }

    #[test]
    fn names_the_dominant_step_only_when_every_finished_run_agrees() {
        let slow_source = || finished(1_000, vec![source("tx", 10, 800), combine(100, None)]);
        let slow_combine = || finished(1_000, vec![source("tx", 10, 100), combine(800, None)]);
        let failed = run(
            50,
            RunOutcome::Failed(RunError {
                step: "the combine step".to_owned(),
                message: "no".to_owned(),
            }),
            vec![source("tx", 10, 50)],
        );
        let cases = [
            (
                vec![slow_source(), slow_source()],
                Some(StepKey::Source("tx".to_owned())),
            ),
            (vec![failed.clone(), slow_combine()], Some(StepKey::Combine)),
            (vec![slow_source(), slow_combine()], None),
            (vec![failed], None),
            (Vec::new(), None),
        ];
        for (runs, expected) in cases {
            assert_eq!(
                dominant_throughout(&runs).map(|dominant| dominant.step),
                expected
            );
        }
    }

    #[test]
    fn hints_at_a_source_that_reads_far_more_than_is_written() {
        let cases = [
            (9_999, 10, false),
            (10_000, 10, true),
            (10_000, 1_000, true),
            (10_000, 1_001, false),
            (2_000_000, 300_000, false),
        ];
        for (read, written, hinted) in cases {
            let runs = [finished(
                1_000,
                vec![
                    source("tx", read, 500),
                    combine(400, None),
                    output(1, written, 10),
                ],
            )];
            let expected = hinted.then(|| Hint::Filter {
                source: "tx".to_owned(),
                read,
                written,
            });
            assert_eq!(
                hints(&runs).first().cloned(),
                expected,
                "{read} for {written}"
            );
        }
    }

    #[test]
    fn hints_at_a_combine_step_that_spilled_from_the_newest_finished_run() {
        let spilled = |bytes| finished(1_000, vec![combine(900, bytes), output(1, 40, 10)]);
        let cancelled = run(500, RunOutcome::Cancelled, vec![combine(400, Some(0))]);
        let cases = [
            (
                vec![spilled(Some(3_200_000_000))],
                vec![Hint::Spilled {
                    bytes: 3_200_000_000,
                }],
            ),
            (vec![spilled(Some(0))], Vec::new()),
            (vec![spilled(None)], Vec::new()),
            (
                vec![cancelled, spilled(Some(1 << 20))],
                vec![Hint::Spilled { bytes: 1 << 20 }],
            ),
            (vec![spilled(Some(0)), spilled(Some(1 << 20))], Vec::new()),
        ];
        for (runs, expected) in cases {
            assert_eq!(hints(&runs), expected);
        }
        assert_eq!(
            Hint::Spilled {
                bytes: 3_200_000_000
            }
            .to_string(),
            "Combine spilled 3.2 GB to disk, past its memory_limit. A larger memory_limit in \
             [combine], or fewer rows from the sources, keeps it in memory."
        );
    }

    #[test]
    fn lists_sources_then_the_combine_step_then_the_outputs() {
        let runs = [
            finished(
                1,
                vec![output(1, 1, 1), combine(1, None), source("assets", 1, 1)],
            ),
            finished(
                1,
                vec![source("tx", 1, 1), source("assets", 1, 1), output(2, 1, 1)],
            ),
        ];
        assert_eq!(
            step_keys(&runs),
            [
                StepKey::Source("assets".to_owned()),
                StepKey::Source("tx".to_owned()),
                StepKey::Combine,
                StepKey::Output(1),
                StepKey::Output(2),
            ]
        );
    }
}
