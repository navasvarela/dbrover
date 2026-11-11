//! Two runs of a report side by side (`docs/reports-at-scale.md`, "Comparing two runs", RS1f):
//! per step, the change in time, rows, bytes and peaks, with the large ones marked; the
//! parameters and limits that differ; whether the report's file changed between them. And,
//! over every run kept, the steps that took far longer than they usually do.
//!
//! Pure functions of kept runs, shared by the tab's **Compare** and `dbrover report compare`.

use std::collections::BTreeMap;

use dbrover_core::ByteCount;
use dbrover_report::grouped;
use dbrover_store::{ReportRun, RunOutcome, StepMetrics};

use super::insights::{StepKey, own_time, seconds, step_keys};

/// A change past this share of the number before it is marked: half again, or half less.
const MARKED_CHANGE: f64 = 0.5;
/// A step is slower than usual past this many times its median over the runs kept...
const USUAL_RATIO: f64 = 1.5;
/// ... counted over at least this many finished runs that have it: a median of two is the
/// other run, not what is usual.
const USUAL_MIN_RUNS: usize = 3;

/// What a number counts, for writing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millis,
    Rows,
    Bytes,
}

impl Unit {
    pub fn format(self, value: u64) -> String {
        match self {
            Self::Millis => seconds(value),
            Self::Rows => grouped(value),
            Self::Bytes => ByteCount(value).to_string(),
        }
    }
}

/// One number of a step in both runs; `None` where a run didn't measure it.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub what: &'static str,
    pub unit: Unit,
    pub before: Option<u64>,
    pub after: Option<u64>,
}

impl Change {
    /// The change as a share of the number before, as `0.5` for half again; `None` without
    /// both numbers, or from nothing.
    pub fn ratio(&self) -> Option<f64> {
        let (before, after) = (self.before?, self.after?);
        (before > 0).then(|| (after as f64 - before as f64) / before as f64)
    }

    /// Whether the change is large enough to mark.
    pub fn marked(&self) -> bool {
        self.ratio()
            .is_some_and(|ratio| ratio.abs() > MARKED_CHANGE)
    }

    /// `+120%`, `−40%`, `same`; empty without both numbers.
    pub fn describe_ratio(&self) -> String {
        match self.ratio() {
            Some(ratio) if (ratio * 100.0).round() == 0.0 => "same".to_owned(),
            Some(ratio) if ratio > 0.0 => format!("+{:.0}%", ratio * 100.0),
            Some(ratio) => format!("−{:.0}%", -ratio * 100.0),
            None => String::new(),
        }
    }

    pub fn before_text(&self) -> String {
        self.before
            .map_or_else(|| "—".to_owned(), |value| self.unit.format(value))
    }

    pub fn after_text(&self) -> String {
        self.after
            .map_or_else(|| "—".to_owned(), |value| self.unit.format(value))
    }
}

/// A step's numbers in both runs.
#[derive(Debug, Clone, PartialEq)]
pub struct StepChange {
    pub step: StepKey,
    pub changes: Vec<Change>,
}

/// A setting that differs between the runs: a parameter, or a limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Differs {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Two runs compared.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// The whole run's time and the worker's memory.
    pub run: Vec<Change>,
    pub steps: Vec<StepChange>,
    pub params: Vec<Differs>,
    pub limits: Vec<Differs>,
    /// Whether the report's file changed between them; `None` when either run predates
    /// VC1e's versions.
    pub file_changed: Option<bool>,
}

/// `before` and `after` compared, step by step.
pub fn compare(before: &ReportRun, after: &ReportRun) -> Comparison {
    let both = [before.clone(), after.clone()];
    let steps = step_keys(&both)
        .into_iter()
        .map(|key| StepChange {
            changes: step_changes(before, after, &key),
            step: key,
        })
        .collect();
    let run = vec![
        Change {
            what: "time",
            unit: Unit::Millis,
            before: Some(millis(before)),
            after: Some(millis(after)),
        },
        Change {
            what: "worker memory",
            unit: Unit::Bytes,
            before: before.profile.peak_memory,
            after: after.profile.peak_memory,
        },
    ];
    Comparison {
        run: run.into_iter().filter(has_a_number).collect(),
        steps,
        params: differing(&before.params, &after.params),
        limits: differing(&limits(before), &limits(after)),
        file_changed: before
            .version
            .zip(after.version)
            .map(|(before, after)| before != after),
    }
}

fn millis(run: &ReportRun) -> u64 {
    u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn has_a_number(change: &Change) -> bool {
    change.before.is_some() || change.after.is_some()
}

fn find<'a>(run: &'a ReportRun, key: &StepKey) -> Option<&'a StepMetrics> {
    run.steps.iter().find(|step| StepKey::of(step) == *key)
}

/// Each number of `key`'s step that either run measured.
fn step_changes(before: &ReportRun, after: &ReportRun, key: &StepKey) -> Vec<Change> {
    let numbers = |run: &ReportRun| -> Vec<(&'static str, Unit, Option<u64>)> {
        let step = find(run, key);
        let time = step.and_then(|step| own_time(run, step));
        let (rows, bytes, memory, spilled) = match step {
            Some(StepMetrics::Source(source)) => {
                (Some(source.rows), Some(source.bytes), None, None)
            }
            Some(StepMetrics::Combine(combine)) => (
                Some(combine.rows),
                Some(combine.bytes),
                combine.peak_memory,
                combine.peak_temp_size,
            ),
            Some(StepMetrics::Output(output)) => (Some(output.rows), Some(output.size), None, None),
            Some(StepMetrics::Attached { .. }) | None => (None, None, None, None),
        };
        vec![
            ("time", Unit::Millis, time),
            ("rows", Unit::Rows, rows),
            ("bytes", Unit::Bytes, bytes),
            ("peak memory", Unit::Bytes, memory),
            ("spilled", Unit::Bytes, spilled),
        ]
    };
    numbers(before)
        .into_iter()
        .zip(numbers(after))
        .map(|((what, unit, before), (_, _, after))| Change {
            what,
            unit,
            before,
            after,
        })
        .filter(has_a_number)
        .collect()
}

/// The limits a run ran under, by a name that says where each is set.
fn limits(run: &ReportRun) -> BTreeMap<String, String> {
    let mut limits = BTreeMap::new();
    if let Some(timeout) = run.profile.timeout_ms {
        limits.insert("[report] timeout".to_owned(), seconds(timeout));
    }
    for step in &run.steps {
        match step {
            StepMetrics::Source(source) => {
                if let Some(caps) = source.limits {
                    let at = format!("source {}", source.id);
                    limits.insert(format!("{at} max_rows"), grouped(caps.max_rows));
                    limits.insert(
                        format!("{at} max_bytes"),
                        ByteCount(caps.max_bytes).to_string(),
                    );
                    limits.insert(format!("{at} timeout"), seconds(caps.timeout_ms));
                }
            }
            StepMetrics::Combine(combine) => {
                if let Some(caps) = combine.limits {
                    limits.insert(
                        "[combine] memory_limit".to_owned(),
                        ByteCount(caps.memory_limit).to_string(),
                    );
                    limits.insert(
                        "[combine] max_temp_size".to_owned(),
                        ByteCount(caps.max_temp_size).to_string(),
                    );
                    limits.insert("[combine] timeout".to_owned(), seconds(caps.timeout_ms));
                }
            }
            StepMetrics::Attached { .. } | StepMetrics::Output(_) => {}
        }
    }
    limits
}

/// The names whose values differ between `before` and `after`, in name order.
fn differing(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<Differs> {
    let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| before.get(*name) != after.get(*name))
        .map(|name| Differs {
            name: name.clone(),
            before: before.get(name).cloned(),
            after: after.get(name).cloned(),
        })
        .collect()
}

/// A step that took far longer than it usually does, in one of the runs listed.
#[derive(Debug, Clone, PartialEq)]
pub struct Slower {
    /// The run, by its place in the list.
    pub run: usize,
    pub step: StepKey,
    /// Its time over its median.
    pub times: f64,
}

/// Every step of `runs` that took more than half again its median time over the finished runs
/// that have it, once there are enough of them.
pub fn slower_than_usual(runs: &[ReportRun]) -> Vec<Slower> {
    let finished: Vec<(usize, &ReportRun)> = runs
        .iter()
        .enumerate()
        .filter(|(_, run)| run.outcome == RunOutcome::Finished)
        .collect();
    let mut slower = Vec::new();
    for key in step_keys(runs) {
        let times: Vec<(usize, u64)> = finished
            .iter()
            .filter_map(|(index, run)| Some((*index, own_time(run, find(run, &key)?)?)))
            .collect();
        if times.len() < USUAL_MIN_RUNS {
            continue;
        }
        let mut sorted: Vec<u64> = times.iter().map(|(_, time)| *time).collect();
        sorted.sort_unstable();
        // The middle time, the later of the two middle ones for an even count.
        let median = sorted[sorted.len() / 2];
        if median == 0 {
            continue;
        }
        for (run, time) in times {
            let ratio = time as f64 / median as f64;
            if ratio > USUAL_RATIO {
                slower.push(Slower {
                    run,
                    step: key.clone(),
                    times: ratio,
                });
            }
        }
    }
    slower
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::BlobId;
    use dbrover_store::{CombineCaps, CombineMetrics, ReportName, SourceMetrics};
    use jiff::Timestamp;

    use super::*;

    fn source(id: &str, rows: u64, elapsed_ms: u64) -> StepMetrics {
        StepMetrics::Source(SourceMetrics {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            rows,
            bytes: rows * 10,
            first_row_ms: Some(1),
            elapsed_ms,
            batches: None,
            started_ms: None,
            engine: None,
            waits_for: None,
            phases: None,
            limits: None,
            columns: Vec::new(),
            scanned_bytes: None,
            parts: Vec::new(),
        })
    }

    fn combine(elapsed_ms: u64, memory_limit: u64) -> StepMetrics {
        StepMetrics::Combine(CombineMetrics {
            rows: 10,
            bytes: 100,
            first_row_ms: Some(1),
            elapsed_ms,
            peak_memory: Some(1_000),
            peak_temp_size: None,
            started_ms: None,
            query_ms: None,
            limits: Some(CombineCaps {
                memory_limit,
                max_temp_size: 1 << 30,
                timeout_ms: 600_000,
            }),
            plan: None,
        })
    }

    fn run(elapsed_ms: u64, since: &str, version: &[u8], steps: Vec<StepMetrics>) -> ReportRun {
        ReportRun {
            id: None,
            report: ReportName::new("valuation").unwrap(),
            started_at: Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_millis(elapsed_ms),
            outcome: RunOutcome::Finished,
            params: [("since".to_owned(), since.to_owned())].into(),
            steps,
            origin: dbrover_store::RunOrigin::App,
            version: Some(BlobId::of(version)),
            profile: Default::default(),
        }
    }

    #[test]
    fn compares_each_step_and_marks_the_large_changes() {
        let before = run(
            2_000,
            "2026-09-01",
            b"a",
            vec![source("tx", 1_000, 1_000), combine(500, 1 << 30)],
        );
        let after = run(
            5_000,
            "2026-10-01",
            b"b",
            vec![source("tx", 1_400, 3_500), combine(600, 2 << 30)],
        );
        let comparison = compare(&before, &after);
        let tx = &comparison.steps[0];
        assert_eq!(tx.step, StepKey::Source("tx".to_owned()));
        let marks: Vec<(&str, String, bool)> = tx
            .changes
            .iter()
            .map(|change| (change.what, change.describe_ratio(), change.marked()))
            .collect();
        assert_eq!(
            marks,
            [
                ("time", "+250%".to_owned(), true),
                ("rows", "+40%".to_owned(), false),
                ("bytes", "+40%".to_owned(), false),
            ]
        );
        let combine = &comparison.steps[1];
        assert_eq!(
            combine
                .changes
                .iter()
                .map(|change| (change.what, change.describe_ratio()))
                .collect::<Vec<_>>(),
            [
                ("time", "+20%".to_owned()),
                ("rows", "same".to_owned()),
                ("bytes", "same".to_owned()),
                ("peak memory", "same".to_owned()),
            ]
        );
        assert_eq!(
            comparison.params,
            [Differs {
                name: "since".to_owned(),
                before: Some("2026-09-01".to_owned()),
                after: Some("2026-10-01".to_owned()),
            }]
        );
        assert_eq!(
            comparison.limits,
            [Differs {
                name: "[combine] memory_limit".to_owned(),
                before: Some("1.1 GB".to_owned()),
                after: Some("2.1 GB".to_owned()),
            }]
        );
        assert_eq!(comparison.file_changed, Some(true));
        assert_eq!(comparison.run[0].describe_ratio(), "+150%");
    }

    #[test]
    fn a_step_only_one_run_has_compares_with_nothing() {
        let before = run(1_000, "x", b"a", vec![source("tx", 10, 100)]);
        let after = run(
            1_000,
            "x",
            b"a",
            vec![source("tx", 10, 100), source("fx", 5, 50)],
        );
        let comparison = compare(&before, &after);
        let fx = &comparison.steps[1];
        assert_eq!(fx.changes[0].before_text(), "—");
        assert_eq!(fx.changes[0].describe_ratio(), "");
        assert!(!fx.changes[0].marked());
        assert_eq!(comparison.file_changed, Some(false));
    }

    #[test]
    fn marks_a_step_past_half_again_its_median_over_enough_runs() {
        let runs = |times: &[u64]| -> Vec<ReportRun> {
            times
                .iter()
                .map(|time| run(*time + 10, "x", b"a", vec![source("tx", 10, *time)]))
                .collect()
        };
        let slower = slower_than_usual(&runs(&[1_700, 1_000, 1_000, 900, 1_100]));
        assert_eq!(
            slower,
            [Slower {
                run: 0,
                step: StepKey::Source("tx".to_owned()),
                times: 1.7,
            }]
        );
        assert!(slower_than_usual(&runs(&[1_500, 1_000, 1_000, 900, 1_100])).is_empty());
        // Two runs aren't enough to say what is usual.
        assert!(slower_than_usual(&runs(&[5_000, 1_000])).is_empty());
    }
}
