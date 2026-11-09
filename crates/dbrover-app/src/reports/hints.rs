//! A run's hints as rules (`docs/reports-at-scale.md`, "Hints", RS1e): each rule a pure
//! function of the newest finished run's profile, with an id a report's `quiet_hints` silences
//! it by, thresholds named here and in the manual, and one sentence naming the step and the
//! number behind it.
//!
//! The rules live in the app rather than `dbrover-report` because a kept run is
//! `dbrover-store`'s type, which `dbrover-report` doesn't know (`docs/reports-at-scale.md`,
//! "RS1 against today's code").

use std::fmt;

use dbrover_core::ByteCount;
use dbrover_report::{HintId, grouped};
use dbrover_store::{
    CombineMetrics, OutputMetrics, ReportRun, RunOutcome, SourceMetrics, StepMetrics,
};

use super::insights::seconds;

/// `source-reads-more`: a source must read at least this many rows, below which filtering in
/// its query saves too little to mention...
const FILTER_MIN_ROWS: u64 = 10_000;
/// ... and at least this many times the rows the outputs write.
const FILTER_RATIO: u64 = 10;
/// A step that took less than this isn't worth a hint about its time: five seconds, about
/// what a person notices waiting for a run.
const MIN_HINTED_MS: u64 = 5_000;
/// `slow-first-row`: the share of a source's time before its first row, from 0 to 100.
const SLOW_FIRST_ROW_PCT: u64 = 80;
/// `load-bound`, `writer-bound`: "most of" a source's or the run's time.
const MOST_PCT: u64 = 50;
/// `many-batches`: a lookup's statements.
const MANY_BATCHES: u64 = 50;
/// `round-wait`: how long a source waited for sources it doesn't read.
const ROUND_WAIT_MS: u64 = 30_000;
/// `near-limit`: how close to a limit a peak came without passing it.
const NEAR_LIMIT_PCT: u64 = 90;

/// The table of the report's file a hint is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HintAt {
    Report,
    Source(String),
    Combine,
    /// Counted from 1.
    Output(usize),
}

/// Something to change, from what a run measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub id: HintId,
    pub at: HintAt,
    /// What was measured and what to do, a sentence or two.
    pub text: String,
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// A rule: the hints a finished run gives.
type Rule = fn(&ReportRun) -> Vec<Hint>;

/// Every rule, in the order their hints are listed.
const RULES: [Rule; 8] = [
    source_reads_more,
    combine_spilled,
    slow_first_row,
    load_bound,
    many_batches,
    round_wait,
    writer_bound,
    near_limit,
];

/// The hints the newest finished run of `runs` gives.
pub fn hints(runs: &[ReportRun]) -> Vec<Hint> {
    let Some(run) = runs.iter().find(|run| run.outcome == RunOutcome::Finished) else {
        return Vec::new();
    };
    RULES.iter().flat_map(|rule| rule(run)).collect()
}

fn sources(run: &ReportRun) -> impl Iterator<Item = &SourceMetrics> {
    run.steps.iter().filter_map(|step| match step {
        StepMetrics::Source(source) => Some(source),
        _ => None,
    })
}

fn combine(run: &ReportRun) -> Option<&CombineMetrics> {
    run.steps.iter().find_map(|step| match step {
        StepMetrics::Combine(combine) => Some(combine),
        _ => None,
    })
}

fn outputs(run: &ReportRun) -> impl Iterator<Item = &OutputMetrics> {
    run.steps.iter().filter_map(|step| match step {
        StepMetrics::Output(output) => Some(output),
        _ => None,
    })
}

fn run_ms(run: &ReportRun) -> u64 {
    u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Whether `part` is at least `pct` percent of `whole`.
fn at_least(part: u64, whole: u64, pct: u64) -> bool {
    u128::from(part) * 100 >= u128::from(whole) * u128::from(pct)
}

fn percent(part: u64, whole: u64) -> u64 {
    u64::try_from(u128::from(part) * 100 / u128::from(whole.max(1))).unwrap_or(u64::MAX)
}

/// `source-reads-more`: a source read far more rows than the outputs wrote.
fn source_reads_more(run: &ReportRun) -> Vec<Hint> {
    let Some(written) = outputs(run).map(|output| output.rows).max() else {
        return Vec::new();
    };
    sources(run)
        .filter(|source| {
            source.rows >= FILTER_MIN_ROWS && source.rows >= written.saturating_mul(FILTER_RATIO)
        })
        .map(|source| Hint {
            id: HintId::SourceReadsMore,
            at: HintAt::Source(source.id.clone()),
            text: format!(
                "Source {} read {} rows for {} written. If the combine step drops most of \
                 them, filter them in the source's query, where its database can use an index.",
                source.id,
                grouped(source.rows),
                grouped(written)
            ),
        })
        .collect()
}

/// `combine-spilled`: the combine step held something on disk.
fn combine_spilled(run: &ReportRun) -> Vec<Hint> {
    combine(run)
        .and_then(|combine| combine.peak_temp_size)
        .filter(|bytes| *bytes > 0)
        .map(|bytes| Hint {
            id: HintId::CombineSpilled,
            at: HintAt::Combine,
            text: format!(
                "Combine spilled {} to disk, past its memory_limit. A larger memory_limit in \
                 [combine], or fewer rows from the sources, keeps it in memory.",
                ByteCount(bytes)
            ),
        })
        .into_iter()
        .collect()
}

/// `slow-first-row`: a source spent most of its time before its first row: its engine's own
/// work, which an index or a better query shortens.
fn slow_first_row(run: &ReportRun) -> Vec<Hint> {
    sources(run)
        .filter_map(|source| {
            let phases = source.phases?;
            let before = phases.connect_ms + phases.server_ms;
            let slow = source.elapsed_ms >= MIN_HINTED_MS
                && at_least(before, source.elapsed_ms, SLOW_FIRST_ROW_PCT);
            slow.then(|| Hint {
                id: HintId::SlowFirstRow,
                at: HintAt::Source(source.id.clone()),
                text: format!(
                    "Source {} spent {} of its {} before its first row, waiting on {}. Explain \
                     its query in a console to see how its database runs it, and look for an \
                     index on what it filters and joins by.",
                    source.id,
                    seconds(before),
                    seconds(source.elapsed_ms),
                    source.engine.as_deref().unwrap_or("its database")
                ),
            })
        })
        .collect()
}

/// `load-bound`: a source spent most of its time in DuckDB, appending and typing its rows.
fn load_bound(run: &ReportRun) -> Vec<Hint> {
    sources(run)
        .filter_map(|source| {
            let phases = source.phases?;
            let loading = phases.load_ms + phases.type_ms;
            let bound = source.elapsed_ms >= MIN_HINTED_MS
                && at_least(loading, source.elapsed_ms, MOST_PCT);
            bound.then(|| Hint {
                id: HintId::LoadBound,
                at: HintAt::Source(source.id.clone()),
                text: format!(
                    "Source {} spent {} of its {} loading its {} rows into DuckDB. Every row is \
                     loaded as text, then typed: read fewer rows or columns in its query, or \
                     aggregate there.",
                    source.id,
                    seconds(loading),
                    seconds(source.elapsed_ms),
                    grouped(source.rows)
                ),
            })
        })
        .collect()
}

/// `many-batches`: a lookup sent many statements.
fn many_batches(run: &ReportRun) -> Vec<Hint> {
    sources(run)
        .filter_map(|source| {
            let batches = source.batches.filter(|batches| *batches >= MANY_BATCHES)?;
            Some(Hint {
                id: HintId::ManyBatches,
                at: HintAt::Source(source.id.clone()),
                text: format!(
                    "Source {}'s lookup ran {} batches. A larger batch_size sends fewer \
                     statements; or read the table whole and join it in the combine step.",
                    source.id,
                    grouped(batches)
                ),
            })
        })
        .collect()
}

/// `round-wait`: a source waited for sources it doesn't read, because they were in the round
/// before it.
fn round_wait(run: &ReportRun) -> Vec<Hint> {
    let end_of = |id: &str| {
        sources(run)
            .find(|source| source.id.eq_ignore_ascii_case(id))
            .and_then(|source| Some(source.started_ms? + source.elapsed_ms))
    };
    sources(run)
        .filter_map(|source| {
            let waits_for = source.waits_for.as_deref()?;
            let started = source.started_ms?;
            let waited = started.saturating_sub(end_of(waits_for)?);
            (waited >= ROUND_WAIT_MS).then(|| Hint {
                id: HintId::RoundWait,
                at: HintAt::Source(source.id.clone()),
                text: format!(
                    "Source {} waited {} after {}, the source its lookup reads, for the rest of \
                     that round to end. A source starts once every source of the round before \
                     it is done, so a slow one there holds every source after it.",
                    source.id,
                    seconds(waited),
                    waits_for
                ),
            })
        })
        .collect()
}

/// `writer-bound`: an output's writer took most of the run.
fn writer_bound(run: &ReportRun) -> Vec<Hint> {
    let total = run_ms(run);
    outputs(run)
        .filter(|output| {
            output.elapsed_ms >= MIN_HINTED_MS && at_least(output.elapsed_ms, total, MOST_PCT)
        })
        .map(|output| {
            let excel = output
                .file
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("xlsx"));
            let advice = if excel {
                "Excel is the slowest format to write: a CSV output of the same rows is faster, \
                 and Excel opens it too."
            } else {
                "Writing fewer rows or columns shortens it."
            };
            Hint {
                id: HintId::WriterBound,
                at: HintAt::Output(output.number),
                text: format!(
                    "Output {} ({}) took {} of the run's {} writing its {} rows. {advice}",
                    output.number,
                    output.file,
                    seconds(output.elapsed_ms),
                    seconds(total),
                    grouped(output.rows)
                ),
            }
        })
        .collect()
}

/// A peak and the limit it was measured against.
struct Peak {
    at: HintAt,
    /// The step, as a sentence starts with it.
    step: String,
    /// What reached the limit, as `read 950,000 rows`.
    reached: String,
    key: &'static str,
    /// The limit, as `1,000,000`.
    limit: String,
    value: u64,
    of: u64,
}

/// `near-limit`: a peak came within a tenth of its limit without passing it, so a larger
/// month or a slower day will stop the run.
fn near_limit(run: &ReportRun) -> Vec<Hint> {
    let mut peaks = Vec::new();
    for source in sources(run) {
        let Some(limits) = source.limits else {
            continue;
        };
        let step = format!("Source {}", source.id);
        let at = || HintAt::Source(source.id.clone());
        peaks.extend([
            Peak {
                at: at(),
                step: step.clone(),
                reached: format!("read {} rows", grouped(source.rows)),
                key: "max_rows",
                limit: grouped(limits.max_rows),
                value: source.rows,
                of: limits.max_rows,
            },
            Peak {
                at: at(),
                step: step.clone(),
                reached: format!("read {}", ByteCount(source.bytes)),
                key: "max_bytes",
                limit: ByteCount(limits.max_bytes).to_string(),
                value: source.bytes,
                of: limits.max_bytes,
            },
            Peak {
                at: at(),
                step,
                reached: format!("took {}", seconds(source.elapsed_ms)),
                key: "timeout",
                limit: seconds(limits.timeout_ms),
                value: source.elapsed_ms,
                of: limits.timeout_ms,
            },
        ]);
    }
    if let Some(combine) = combine(run)
        && let Some(limits) = combine.limits
    {
        let step = "Combine".to_owned();
        if let Some(memory) = combine.peak_memory {
            peaks.push(Peak {
                at: HintAt::Combine,
                step: step.clone(),
                reached: format!("held {} in memory", ByteCount(memory)),
                key: "memory_limit",
                limit: ByteCount(limits.memory_limit).to_string(),
                value: memory,
                of: limits.memory_limit,
            });
        }
        if let Some(spilled) = combine.peak_temp_size {
            peaks.push(Peak {
                at: HintAt::Combine,
                step: step.clone(),
                reached: format!("spilled {}", ByteCount(spilled)),
                key: "max_temp_size",
                limit: ByteCount(limits.max_temp_size).to_string(),
                value: spilled,
                of: limits.max_temp_size,
            });
        }
        peaks.push(Peak {
            at: HintAt::Combine,
            step,
            reached: format!("took {}", seconds(combine.elapsed_ms)),
            key: "timeout",
            limit: seconds(limits.timeout_ms),
            value: combine.elapsed_ms,
            of: limits.timeout_ms,
        });
    }
    if let Some(timeout_ms) = run.profile.timeout_ms {
        peaks.push(Peak {
            at: HintAt::Report,
            step: "The run".to_owned(),
            reached: format!("took {}", seconds(run_ms(run))),
            key: "timeout",
            limit: seconds(timeout_ms),
            value: run_ms(run),
            of: timeout_ms,
        });
    }
    peaks
        .into_iter()
        .filter(|peak| peak.of > 0 && peak.value <= peak.of)
        .filter(|peak| at_least(peak.value, peak.of, NEAR_LIMIT_PCT))
        .map(|peak| Hint {
            id: HintId::NearLimit,
            text: format!(
                "{} {}, {}% of its {} of {}. Raise {} before a larger run passes it and stops.",
                peak.step,
                peak.reached,
                percent(peak.value, peak.of),
                peak.key,
                peak.limit,
                peak.key
            ),
            at: peak.at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_store::{CombineCaps, ReportName, RunProfile, SourceCaps, SourcePhases};
    use jiff::Timestamp;

    use super::*;

    fn source(id: &str, rows: u64, elapsed_ms: u64) -> SourceMetrics {
        SourceMetrics {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            rows,
            bytes: rows * 20,
            first_row_ms: Some(1),
            elapsed_ms,
            batches: None,
            started_ms: Some(0),
            engine: Some("PostgreSQL".to_owned()),
            waits_for: None,
            phases: None,
            limits: None,
        }
    }

    fn phased(id: &str, elapsed_ms: u64, server_ms: u64, load_ms: u64) -> SourceMetrics {
        SourceMetrics {
            phases: Some(SourcePhases {
                connect_ms: 0,
                server_ms,
                transfer_ms: elapsed_ms.saturating_sub(server_ms + load_ms),
                load_ms,
                type_ms: 0,
            }),
            ..source(id, 1_000, elapsed_ms)
        }
    }

    fn combine(elapsed_ms: u64, peak_temp_size: Option<u64>) -> CombineMetrics {
        CombineMetrics {
            rows: 40,
            bytes: 800,
            first_row_ms: Some(1),
            elapsed_ms,
            peak_memory: Some(1 << 30),
            peak_temp_size,
            started_ms: None,
            query_ms: None,
            limits: None,
        }
    }

    fn output(number: usize, file: &str, rows: u64, elapsed_ms: u64) -> OutputMetrics {
        OutputMetrics {
            number,
            file: file.to_owned(),
            rows,
            size: rows * 30,
            elapsed_ms,
            started_ms: None,
            ended_ms: None,
        }
    }

    fn run(elapsed_ms: u64, outcome: RunOutcome, steps: Vec<StepMetrics>) -> ReportRun {
        ReportRun {
            report: ReportName::new("values").unwrap(),
            started_at: Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_millis(elapsed_ms),
            outcome,
            params: Default::default(),
            steps,
            origin: dbrover_store::RunOrigin::App,
            version: None,
            profile: RunProfile::default(),
        }
    }

    fn finished(elapsed_ms: u64, steps: Vec<StepMetrics>) -> ReportRun {
        run(elapsed_ms, RunOutcome::Finished, steps)
    }

    /// The ids `run` hints with.
    fn ids(run: ReportRun) -> Vec<HintId> {
        hints(&[run]).into_iter().map(|hint| hint.id).collect()
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
            let hints = hints(&[finished(
                1_000,
                vec![
                    StepMetrics::Source(source("tx", read, 500)),
                    StepMetrics::Combine(combine(400, None)),
                    StepMetrics::Output(output(1, "v.csv", written, 10)),
                ],
            )]);
            assert_eq!(
                hints.first().map(|hint| (hint.id, hint.at.clone())),
                hinted.then(|| (HintId::SourceReadsMore, HintAt::Source("tx".to_owned()))),
                "{read} for {written}"
            );
        }
    }

    #[test]
    fn hints_at_a_combine_step_that_spilled_from_the_newest_finished_run() {
        let spilled = |bytes| {
            finished(
                1_000,
                vec![
                    StepMetrics::Combine(combine(900, bytes)),
                    StepMetrics::Output(output(1, "v.csv", 40, 10)),
                ],
            )
        };
        let cancelled = run(
            500,
            RunOutcome::Cancelled,
            vec![StepMetrics::Combine(combine(400, Some(0)))],
        );
        let cases = [
            (vec![spilled(Some(3_200_000_000))], true),
            (vec![spilled(Some(0))], false),
            (vec![spilled(None)], false),
            (vec![cancelled, spilled(Some(1 << 20))], true),
            (vec![spilled(Some(0)), spilled(Some(1 << 20))], false),
        ];
        for (runs, hinted) in cases {
            assert_eq!(!hints(&runs).is_empty(), hinted, "{runs:?}");
        }
        assert_eq!(
            hints(&[spilled(Some(3_200_000_000))])[0].to_string(),
            "Combine spilled 3.2 GB to disk, past its memory_limit. A larger memory_limit in \
             [combine], or fewer rows from the sources, keeps it in memory."
        );
    }

    #[test]
    fn hints_at_a_slow_first_row_and_a_load_bound_source_past_their_thresholds() {
        let cases = [
            // Before the first row: 80% of five seconds, or more, is slow.
            (phased("tx", 5_000, 4_000, 0), vec![HintId::SlowFirstRow]),
            (phased("tx", 5_000, 3_999, 0), vec![]),
            // Too short to mention, however it splits.
            (phased("tx", 4_999, 4_999, 0), vec![]),
            // Loading: half its time, or more.
            (phased("tx", 10_000, 0, 5_000), vec![HintId::LoadBound]),
            (phased("tx", 10_000, 0, 4_999), vec![]),
            // A source kept before RS1b has no phases to judge.
            (source("tx", 1_000, 60_000), vec![]),
        ];
        for (tx, expected) in cases {
            let label = format!("{:?}", tx.phases);
            assert_eq!(
                ids(finished(tx.elapsed_ms, vec![StepMetrics::Source(tx)])),
                expected,
                "{label}"
            );
        }
        let hint = &hints(&[finished(
            9_000,
            vec![StepMetrics::Source(phased("tx", 9_000, 8_000, 0))],
        )])[0];
        assert_eq!(
            hint.text,
            "Source tx spent 8.0 s of its 9.0 s before its first row, waiting on PostgreSQL. \
             Explain its query in a console to see how its database runs it, and look for an \
             index on what it filters and joins by."
        );
    }

    #[test]
    fn hints_at_many_batches_and_a_long_wait_for_the_round_before() {
        let lookup = |batches| SourceMetrics {
            batches: Some(batches),
            ..source("names", 10, 100)
        };
        assert_eq!(
            ids(finished(1_000, vec![StepMetrics::Source(lookup(50))])),
            [HintId::ManyBatches]
        );
        assert!(ids(finished(1_000, vec![StepMetrics::Source(lookup(49))])).is_empty());

        // `names` reads `small`, done at 1 s, and starts when `tx` is done.
        let round = |tx_ms: u64| {
            let names = SourceMetrics {
                waits_for: Some("small".to_owned()),
                started_ms: Some(tx_ms),
                ..source("names", 10, 100)
            };
            finished(
                tx_ms + 100,
                vec![
                    StepMetrics::Source(source("small", 10, 1_000)),
                    StepMetrics::Source(source("tx", 10, tx_ms)),
                    StepMetrics::Source(names),
                ],
            )
        };
        assert_eq!(ids(round(31_000)), [HintId::RoundWait]);
        assert!(ids(round(30_999)).is_empty());
        assert_eq!(
            hints(&[round(41_000)])[0].text,
            "Source names waited 40.0 s after small, the source its lookup reads, for the rest \
             of that round to end. A source starts once every source of the round before it is \
             done, so a slow one there holds every source after it."
        );
    }

    #[test]
    fn hints_at_a_writer_that_took_most_of_the_run() {
        let cases = [
            (output(1, "v.xlsx", 10, 5_000), 10_000, true),
            (output(1, "v.xlsx", 10, 4_999), 9_000, false),
            (output(1, "v.csv", 10, 6_000), 13_000, false),
        ];
        for (written, run_ms, hinted) in cases {
            let label = format!("{written:?} in {run_ms}");
            assert_eq!(
                ids(finished(run_ms, vec![StepMetrics::Output(written)])),
                if hinted {
                    vec![HintId::WriterBound]
                } else {
                    vec![]
                },
                "{label}"
            );
        }
        assert!(
            hints(&[finished(
                8_000,
                vec![StepMetrics::Output(output(2, "sales.xlsx", 10, 6_000))]
            )])[0]
                .text
                .contains("Excel is the slowest format to write")
        );
    }

    #[test]
    fn hints_at_a_peak_near_its_limit_but_not_past_it() {
        let capped = |rows: u64| SourceMetrics {
            limits: Some(SourceCaps {
                max_rows: 1_000_000,
                max_bytes: u64::MAX,
                timeout_ms: u64::MAX,
            }),
            ..source("tx", rows, 100)
        };
        let cases = [
            (899_999, false),
            (900_000, true),
            (1_000_000, true),
            // Past it, the run stopped: no finished run to hint from has that.
            (1_000_001, false),
        ];
        for (rows, hinted) in cases {
            assert_eq!(
                ids(finished(1_000, vec![StepMetrics::Source(capped(rows))])),
                if hinted {
                    vec![HintId::NearLimit]
                } else {
                    vec![]
                },
                "{rows} rows"
            );
        }
        assert_eq!(
            hints(&[finished(1_000, vec![StepMetrics::Source(capped(950_000))])])[0].text,
            "Source tx read 950,000 rows, 95% of its max_rows of 1,000,000. Raise max_rows \
             before a larger run passes it and stops."
        );

        let mut memory = combine(100, None);
        memory.peak_memory = Some(1_900);
        memory.limits = Some(CombineCaps {
            memory_limit: 2_000,
            max_temp_size: 10_000,
            timeout_ms: 600_000,
        });
        let mut slow = finished(28_000, vec![StepMetrics::Combine(memory)]);
        slow.profile.timeout_ms = Some(30_000);
        let found: Vec<(HintAt, String)> = hints(&[slow])
            .into_iter()
            .map(|hint| (hint.at, hint.text))
            .collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, HintAt::Combine);
        assert!(found[0].1.contains("memory_limit"), "{found:?}");
        assert_eq!(found[1].0, HintAt::Report);
        assert!(
            found[1]
                .1
                .starts_with("The run took 28.0 s, 93% of its timeout of 30.0 s."),
            "{found:?}"
        );
    }

    #[test]
    fn every_id_has_a_rule() {
        // A run that trips every rule at once.
        let mut tx = phased("tx", 20_000, 0, 15_000);
        tx.rows = 950_000;
        tx.limits = Some(SourceCaps {
            max_rows: 1_000_000,
            max_bytes: u64::MAX,
            timeout_ms: u64::MAX,
        });
        let slow = phased("slow", 5_000, 4_500, 0);
        let names = SourceMetrics {
            waits_for: Some("slow".to_owned()),
            started_ms: Some(40_000),
            batches: Some(60),
            ..source("names", 10, 100)
        };
        let run = finished(
            60_000,
            vec![
                StepMetrics::Source(tx),
                StepMetrics::Source(slow),
                StepMetrics::Source(names),
                StepMetrics::Combine(combine(100, Some(1 << 20))),
                StepMetrics::Output(output(1, "v.xlsx", 10, 40_000)),
            ],
        );
        let mut found = ids(run);
        found.sort();
        found.dedup();
        assert_eq!(found, HintId::ALL);
    }
}
