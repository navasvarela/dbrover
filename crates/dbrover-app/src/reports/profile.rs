//! A kept run's profile as the report's tab draws it (`docs/reports-at-scale.md`, "A
//! timeline" and RS1c): a lane a step, each bar split into the phases its time went to, and
//! the bottleneck, the step on the run's critical path that held it longest and the phase
//! that held that step.
//!
//! Pure functions of a [`ReportRun`], so they are tested without drawing anything.

use std::fmt;

use dbrover_store::{CombineMetrics, OutputMetrics, ReportRun, SourceMetrics, StepMetrics};

use super::insights::{StepKey, own_time, seconds};

/// How far a step may start before the one it waits for ends and still be counted as waiting
/// for it: events reach the app a few milliseconds apart, and a source of the next round is
/// opened right after the last of its round ends.
const WAIT_SLACK_MS: u64 = 50;

/// What a stretch of a step's time went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A source connecting to its data source.
    Connect,
    /// A source's engine working on its statement, until its first row.
    Server,
    /// A source receiving the rest of its rows.
    Transfer,
    /// A source's rows appended to its table in DuckDB.
    Load,
    /// A source's table typed once its rows are in.
    Type,
    /// The combine query, until its first row.
    Query,
    /// The combine query's rows after its first.
    Rows,
    /// An output's writer, or the outputs' share of the step that feeds them.
    Write,
    /// An output waiting for the rows of the step that feeds it.
    Idle,
    /// A step measured before RS1b, whose time isn't split.
    Unsplit,
}

impl Phase {
    /// The phase in a legend.
    pub fn name(self) -> &'static str {
        match self {
            Self::Connect => "Connecting",
            Self::Server => "Until the first row",
            Self::Transfer => "Receiving rows",
            Self::Load => "Loading into DuckDB",
            Self::Type => "Typing columns",
            Self::Query => "Combine until its first row",
            Self::Rows => "Combine's rows",
            Self::Write => "Writing",
            Self::Idle => "Waiting for rows",
            Self::Unsplit => "Not split",
        }
    }

    /// What a step held by this phase was doing, for the bottleneck's sentence: `engine` is a
    /// source's engine, where it has one.
    fn doing(self, engine: Option<&str>) -> String {
        let engine = engine.unwrap_or("its database");
        match self {
            Self::Connect => format!("connecting to {engine}"),
            Self::Server => format!("waiting on {engine} for its first row"),
            Self::Transfer => format!("receiving its rows from {engine}"),
            Self::Load => "loading its rows into DuckDB".to_owned(),
            Self::Type => "typing its columns in DuckDB".to_owned(),
            Self::Query => "waiting on DuckDB for its first row".to_owned(),
            Self::Rows => "reading DuckDB's rows".to_owned(),
            Self::Write | Self::Idle => "writing the file".to_owned(),
            Self::Unsplit => "at work".to_owned(),
        }
    }
}

/// A stretch of a step's lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub phase: Phase,
    /// From the run's start.
    pub start_ms: u64,
    pub end_ms: u64,
}

/// A step's lane in the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub step: StepKey,
    /// A source's engine.
    pub engine: Option<String>,
    pub segments: Vec<Segment>,
    /// A source that waited for an unrelated source of the round before: from the end of the
    /// source it reads, or the run's start, to its own start.
    pub waited: Option<(u64, u64)>,
}

/// A run's steps on one time axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub total_ms: u64,
    pub lanes: Vec<Lane>,
}

/// `run`'s timeline; `None` for a run kept before RS1b, whose steps don't say when they ran.
pub fn timeline(run: &ReportRun) -> Option<Timeline> {
    let total_ms = millis(run.elapsed);
    let lanes: Vec<Lane> = run
        .steps
        .iter()
        .filter_map(|step| lane(run, step))
        .collect();
    (!lanes.is_empty()).then_some(Timeline { total_ms, lanes })
}

fn lane(run: &ReportRun, step: &StepMetrics) -> Option<Lane> {
    let key = StepKey::of(step);
    match step {
        StepMetrics::Source(source) => {
            let start = source.started_ms?;
            Some(Lane {
                step: key,
                engine: source.engine.clone(),
                segments: source_segments(run, source, start),
                waited: waited(run, source, start),
            })
        }
        StepMetrics::Combine(combine) => {
            let start = combine.started_ms?;
            Some(Lane {
                step: key,
                engine: None,
                segments: combine_segments(combine, start, outputs_ms(run)),
                waited: None,
            })
        }
        StepMetrics::Output(output) => Some(Lane {
            step: key,
            engine: None,
            segments: output_segments(output)?,
            waited: None,
        }),
        StepMetrics::Attached { .. } => None,
    }
}

/// The time every output spent in its writer.
fn outputs_ms(run: &ReportRun) -> u64 {
    run.steps
        .iter()
        .filter_map(|step| match step {
            StepMetrics::Output(output) => Some(output.elapsed_ms),
            _ => None,
        })
        .sum()
}

/// A source's bar: its phases in the order they begin, each its share of the source's time,
/// and, for a source whose rows go straight to the outputs, their writing last.
fn source_segments(run: &ReportRun, source: &SourceMetrics, start: u64) -> Vec<Segment> {
    let Some(phases) = source.phases else {
        return vec![Segment {
            phase: Phase::Unsplit,
            start_ms: start,
            end_ms: start + source.elapsed_ms,
        }];
    };
    let writes = if feeds_outputs(run) {
        outputs_ms(run)
    } else {
        0
    };
    let shares = [
        (Phase::Connect, phases.connect_ms),
        (Phase::Server, phases.server_ms),
        (Phase::Transfer, phases.transfer_ms),
        (Phase::Load, phases.load_ms),
        (Phase::Type, phases.type_ms),
        (Phase::Write, writes),
    ];
    split(start, source.elapsed_ms, &shares)
}

/// The combine step's bar: its query until the first row, the rest of its rows, and the
/// outputs' writing, which happens between its pages.
fn combine_segments(combine: &CombineMetrics, start: u64, outputs: u64) -> Vec<Segment> {
    let Some(query) = combine.query_ms else {
        return vec![Segment {
            phase: Phase::Unsplit,
            start_ms: start,
            end_ms: start + combine.elapsed_ms,
        }];
    };
    let first = combine.first_row_ms.unwrap_or(query).min(query);
    let shares = [
        (Phase::Query, first),
        (Phase::Rows, query - first),
        (Phase::Write, outputs),
    ];
    split(start, combine.elapsed_ms, &shares)
}

/// An output's bar: from when its file was started to when it was put in place, its writer's
/// time drawn last, after the time it waited for rows.
fn output_segments(output: &OutputMetrics) -> Option<Vec<Segment>> {
    let (start, end) = (output.started_ms?, output.ended_ms?);
    let span = end.saturating_sub(start);
    let shares = [
        (Phase::Idle, span.saturating_sub(output.elapsed_ms)),
        (Phase::Write, output.elapsed_ms.min(span)),
    ];
    Some(split(start, span, &shares))
}

/// `span` from `start`, cut in proportion to `shares`, empty ones left out. The shares are
/// measured apart, so they rarely add up to the span exactly; scaling them keeps each bar as
/// long as its step took.
fn split(start: u64, span: u64, shares: &[(Phase, u64)]) -> Vec<Segment> {
    let total: u64 = shares.iter().map(|(_, ms)| ms).sum();
    if total == 0 {
        return vec![Segment {
            phase: Phase::Unsplit,
            start_ms: start,
            end_ms: start + span,
        }];
    }
    let mut segments = Vec::new();
    let mut done = 0;
    let mut at = start;
    for &(phase, ms) in shares {
        if ms == 0 {
            continue;
        }
        done += ms;
        // Each segment ends where its running share of the span does, so rounding never
        // leaves a gap or runs past the bar.
        let end = start + scaled(span, done, total);
        segments.push(Segment {
            phase,
            start_ms: at,
            end_ms: end,
        });
        at = end;
    }
    segments
}

/// `value` × `part` / `whole`, without overflow.
fn scaled(value: u64, part: u64, whole: u64) -> u64 {
    u64::try_from(u128::from(value) * u128::from(part) / u128::from(whole.max(1)))
        .unwrap_or(u64::MAX)
}

/// Whether the run's one source writes its rows straight to the outputs.
fn feeds_outputs(run: &ReportRun) -> bool {
    !run.steps
        .iter()
        .any(|step| matches!(step, StepMetrics::Combine(_)))
}

/// The stretch a source waited for something other than the source its lookup reads: from
/// that source's end (or the run's start) to its own, when that is more than a moment.
fn waited(run: &ReportRun, source: &SourceMetrics, start: u64) -> Option<(u64, u64)> {
    let ready = match &source.waits_for {
        Some(read) => end_of_source(run, read)?,
        None => 0,
    };
    (start > ready + WAIT_SLACK_MS).then_some((ready, start))
}

fn end_of_source(run: &ReportRun, id: &str) -> Option<u64> {
    run.steps.iter().find_map(|step| match step {
        StepMetrics::Source(source) if source.id.eq_ignore_ascii_case(id) => {
            Some(source.started_ms? + source.elapsed_ms)
        }
        _ => None,
    })
}

/// The step that held a run longest, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Bottleneck {
    pub step: StepKey,
    /// The step's own time: a feed's without its outputs' writing.
    pub own_ms: u64,
    pub total_ms: u64,
    /// The phase that held it, and its share of the step's split time, from 0 to 1; `None`
    /// for a step kept before RS1b.
    pub phase: Option<(Phase, f64)>,
    pub engine: Option<String>,
}

impl fmt::Display for Bottleneck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} held the run longest: {} of {}",
            self.step,
            seconds(self.own_ms),
            seconds(self.total_ms)
        )?;
        match self.phase {
            Some((phase, share)) => write!(
                f,
                ", {:.0}% of it {}.",
                share * 100.0,
                phase.doing(self.engine.as_deref())
            ),
            None => f.write_str("."),
        }
    }
}

/// The step on `run`'s critical path with the most time of its own, and the phase that held
/// it; `None` for a run kept before RS1b, or one with no step.
///
/// The critical path is walked back from the step that ended last: an output's comes from the
/// step that feeds it, the combine step's and a source's from the source that ended last
/// before it started. Sources of one round run at once, so only the slowest of a round is on
/// the path; a faster one, however long, didn't hold the run.
pub fn bottleneck(run: &ReportRun) -> Option<Bottleneck> {
    let path = critical_path(run);
    let (step, own_ms) = path
        .into_iter()
        .filter_map(|step| Some((step, own_time(run, step)?)))
        .max_by_key(|(_, own)| *own)?;
    Some(Bottleneck {
        step: StepKey::of(step),
        own_ms,
        total_ms: millis(run.elapsed),
        phase: held_by(step),
        engine: match step {
            StepMetrics::Source(source) => source.engine.clone(),
            _ => None,
        },
    })
}

/// The steps the run waited on, last first.
fn critical_path(run: &ReportRun) -> Vec<&StepMetrics> {
    let spans: Vec<(&StepMetrics, u64, u64)> = run
        .steps
        .iter()
        .filter_map(|step| span(step).map(|(start, end)| (step, start, end)))
        .collect();
    let Some(&(last, mut start, _)) = spans.iter().max_by_key(|(_, _, end)| *end) else {
        return Vec::new();
    };
    let mut path = vec![last];
    if matches!(last, StepMetrics::Output(_)) {
        let feed = spans.iter().find(|(step, ..)| match step {
            StepMetrics::Combine(_) => true,
            StepMetrics::Source(_) => feeds_outputs(run),
            _ => false,
        });
        match feed {
            Some(&(feed, feed_start, _)) => {
                path.push(feed);
                start = feed_start;
            }
            None => return path,
        }
    }
    // Each step's predecessor: the source that ended last before it started.
    while let Some(&(before, before_start, _)) = spans
        .iter()
        .filter(|(step, _, end)| {
            matches!(step, StepMetrics::Source(_)) && *end <= start + WAIT_SLACK_MS
        })
        .filter(|(step, ..)| !path.iter().any(|on| std::ptr::eq(*on, *step)))
        .max_by_key(|(_, _, end)| *end)
    {
        path.push(before);
        start = before_start;
    }
    path
}

/// When a step ran, from the run's start.
fn span(step: &StepMetrics) -> Option<(u64, u64)> {
    match step {
        StepMetrics::Source(source) => {
            let start = source.started_ms?;
            Some((start, start + source.elapsed_ms))
        }
        StepMetrics::Combine(combine) => {
            let start = combine.started_ms?;
            Some((start, start + combine.elapsed_ms))
        }
        StepMetrics::Output(output) => Some((output.started_ms?, output.ended_ms?)),
        StepMetrics::Attached { .. } => None,
    }
}

/// The phase that took most of `step`'s own time, and its share of it.
fn held_by(step: &StepMetrics) -> Option<(Phase, f64)> {
    let phases: Vec<(Phase, u64)> = match step {
        StepMetrics::Source(source) => {
            let phases = source.phases?;
            vec![
                (Phase::Connect, phases.connect_ms),
                (Phase::Server, phases.server_ms),
                (Phase::Transfer, phases.transfer_ms),
                (Phase::Load, phases.load_ms),
                (Phase::Type, phases.type_ms),
            ]
        }
        StepMetrics::Combine(combine) => {
            let query = combine.query_ms?;
            let first = combine.first_row_ms.unwrap_or(query).min(query);
            vec![(Phase::Query, first), (Phase::Rows, query - first)]
        }
        StepMetrics::Output(output) => vec![(Phase::Write, output.elapsed_ms)],
        StepMetrics::Attached { .. } => return None,
    };
    let total: u64 = phases.iter().map(|(_, ms)| ms).sum();
    let (phase, ms) = phases.into_iter().max_by_key(|(_, ms)| *ms)?;
    (total > 0).then(|| (phase, ms as f64 / total as f64))
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_store::{ReportName, RunOutcome, SourcePhases};
    use jiff::Timestamp;

    use super::*;

    fn source(
        id: &str,
        started_ms: u64,
        elapsed_ms: u64,
        waits_for: Option<&str>,
    ) -> SourceMetrics {
        SourceMetrics {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            rows: 1_000,
            bytes: 20_000,
            first_row_ms: Some(1),
            elapsed_ms,
            batches: None,
            started_ms: Some(started_ms),
            engine: Some("PostgreSQL".to_owned()),
            waits_for: waits_for.map(str::to_owned),
            phases: Some(SourcePhases {
                connect_ms: elapsed_ms / 20,
                server_ms: elapsed_ms * 8 / 10,
                transfer_ms: elapsed_ms / 10,
                load_ms: elapsed_ms / 20,
                type_ms: 0,
            }),
            limits: None,
            columns: Vec::new(),
            scanned_bytes: None,
            parts: Vec::new(),
        }
    }

    fn combine(started_ms: u64, elapsed_ms: u64) -> CombineMetrics {
        CombineMetrics {
            rows: 3,
            bytes: 30,
            first_row_ms: Some(300),
            elapsed_ms,
            peak_memory: None,
            peak_temp_size: None,
            started_ms: Some(started_ms),
            query_ms: Some(400),
            limits: None,
            plan: None,
        }
    }

    fn output(started_ms: u64, ended_ms: u64, elapsed_ms: u64) -> OutputMetrics {
        OutputMetrics {
            number: 1,
            file: "v.csv".to_owned(),
            rows: 3,
            size: 40,
            elapsed_ms,
            started_ms: Some(started_ms),
            ended_ms: Some(ended_ms),
        }
    }

    fn run(elapsed_ms: u64, steps: Vec<StepMetrics>) -> ReportRun {
        ReportRun {
            report: ReportName::new("valuation").unwrap(),
            started_at: Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_millis(elapsed_ms),
            outcome: RunOutcome::Finished,
            params: Default::default(),
            steps,
            origin: dbrover_store::RunOrigin::App,
            version: None,
            profile: Default::default(),
            id: None,
        }
    }

    /// Two sources in the first round, a lookup reading the shorter one in the second, the
    /// combine step and its file.
    fn two_rounds() -> ReportRun {
        run(
            1_820,
            vec![
                StepMetrics::Source(source("small", 0, 200, None)),
                StepMetrics::Source(source("tx", 0, 1_000, None)),
                StepMetrics::Source(source("names", 1_000, 300, Some("small"))),
                StepMetrics::Combine(combine(1_300, 500)),
                StepMetrics::Output(output(1_300, 1_810, 50)),
            ],
        )
    }

    #[test]
    fn names_the_slowest_step_on_the_critical_path_and_the_phase_that_held_it() {
        let found = bottleneck(&two_rounds()).unwrap();
        assert_eq!(found.step, StepKey::Source("tx".to_owned()));
        assert_eq!(
            found.to_string(),
            "Source tx held the run longest: 1.0 s of 1.8 s, 80% of it waiting on PostgreSQL \
             for its first row."
        );
    }

    #[test]
    fn walks_the_run_back_through_the_slowest_source_of_each_round() {
        let path: Vec<StepKey> = critical_path(&two_rounds())
            .into_iter()
            .map(StepKey::of)
            .collect();
        // `names` reads `small`, but its round started once `tx` was done.
        assert_eq!(
            path,
            [
                StepKey::Output(1),
                StepKey::Combine,
                StepKey::Source("names".to_owned()),
                StepKey::Source("tx".to_owned()),
            ]
        );
    }

    #[test]
    fn draws_each_step_split_into_its_phases_and_a_rounds_wait_hatched() {
        let timeline = timeline(&two_rounds()).unwrap();
        assert_eq!(timeline.total_ms, 1_820);
        let names = &timeline.lanes[2];
        assert_eq!(names.step, StepKey::Source("names".to_owned()));
        // It read `small`, done at 200 ms, but waited for `tx`, the rest of its round.
        assert_eq!(names.waited, Some((200, 1_000)));
        assert_eq!(timeline.lanes[1].waited, None);
        let phases: Vec<Phase> = names.segments.iter().map(|segment| segment.phase).collect();
        assert_eq!(
            phases,
            [Phase::Connect, Phase::Server, Phase::Transfer, Phase::Load]
        );
        assert_eq!(names.segments.first().map(|s| s.start_ms), Some(1_000));
        assert_eq!(names.segments.last().map(|s| s.end_ms), Some(1_300));
        // Segments follow each other without a gap.
        for pair in names.segments.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms);
        }
        let combine = &timeline.lanes[3];
        let phases: Vec<(Phase, u64, u64)> = combine
            .segments
            .iter()
            .map(|segment| (segment.phase, segment.start_ms, segment.end_ms))
            .collect();
        assert_eq!(
            phases,
            [
                (Phase::Query, 1_300, 1_633),
                (Phase::Rows, 1_633, 1_744),
                (Phase::Write, 1_744, 1_800),
            ]
        );
        let output = &timeline.lanes[4];
        let phases: Vec<(Phase, u64, u64)> = output
            .segments
            .iter()
            .map(|segment| (segment.phase, segment.start_ms, segment.end_ms))
            .collect();
        assert_eq!(
            phases,
            [(Phase::Idle, 1_300, 1_760), (Phase::Write, 1_760, 1_810)]
        );
    }

    #[test]
    fn a_source_written_straight_to_the_files_gives_their_time_to_the_outputs() {
        let direct = run(
            1_100,
            vec![
                StepMetrics::Source(source("balances", 0, 1_000, None)),
                StepMetrics::Output(output(0, 1_050, 600)),
            ],
        );
        let found = bottleneck(&direct).unwrap();
        assert_eq!(found.step, StepKey::Output(1));
        assert_eq!(
            found.to_string(),
            "Output 1 held the run longest: 0.6 s of 1.1 s, 100% of it writing the file."
        );
    }

    #[test]
    fn a_run_kept_before_profiles_has_no_timeline_and_names_no_phase() {
        let mut old = two_rounds();
        for step in &mut old.steps {
            match step {
                StepMetrics::Source(source) => {
                    source.started_ms = None;
                    source.phases = None;
                }
                StepMetrics::Combine(combine) => combine.started_ms = None,
                StepMetrics::Output(output) => output.started_ms = None,
                StepMetrics::Attached { .. } => {}
            }
        }
        assert_eq!(timeline(&old), None);
        assert_eq!(bottleneck(&old), None);
    }

    #[test]
    fn splits_a_span_in_proportion_without_gaps() {
        let segments = split(
            100,
            10,
            &[(Phase::Server, 1), (Phase::Transfer, 0), (Phase::Load, 2)],
        );
        assert_eq!(
            segments,
            [
                Segment {
                    phase: Phase::Server,
                    start_ms: 100,
                    end_ms: 103,
                },
                Segment {
                    phase: Phase::Load,
                    start_ms: 103,
                    end_ms: 110,
                },
            ]
        );
        assert_eq!(
            split(0, 5, &[(Phase::Server, 0)]),
            [Segment {
                phase: Phase::Unsplit,
                start_ms: 0,
                end_ms: 5,
            }]
        );
    }
}
