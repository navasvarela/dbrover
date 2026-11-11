//! `dbrover report run --progress`: a line on a terminal's standard error that says, about once
//! a second, what each running step has read so far, rewritten in place, and cleared before
//! any other line (`docs/reporting.md`, "Finishing RP6"). RS1d added each step's pace and what
//! it is mostly doing, as the report's tab says them.

use dbrover_core::ByteCount;
use dbrover_report::{Batch, Parts, RunEvent, Step, grouped};

use crate::reports::live::{self, Pace};

/// Moves to the start of the terminal's line and erases it: a carriage return, then ANSI's
/// "erase in line", which every terminal DbRover runs in reads, Windows' since Windows 10.
pub const CLEAR_LINE: &str = "\r\x1b[2K";

/// The running steps, as their last progress event left them.
#[derive(Debug, Default)]
pub struct LiveLine {
    /// Each source's engine, as the run's start named it.
    engines: Vec<(String, String)>,
    /// Each partitioned source's word for one range, as `month`.
    units: Vec<(String, String)>,
    /// A partitioned run's word for one range, and how far it has gone through them.
    run_parts: Option<(String, Parts)>,
    running: Vec<Running>,
}

#[derive(Debug)]
struct Running {
    step: Step,
    pace: Pace,
    text: String,
}

impl LiveLine {
    /// Takes in `event`. Returns whether the line's text changed.
    pub fn apply(&mut self, event: &RunEvent) -> bool {
        match event {
            RunEvent::Started {
                sources, partition, ..
            } => {
                self.run_parts = partition.as_ref().map(|partition| {
                    let parts = Parts {
                        done: 0,
                        running: 1,
                        of: partition.ranges,
                    };
                    (partition.unit.clone(), parts)
                });
                self.engines = sources
                    .iter()
                    .filter_map(|source| Some((source.id.clone(), source.engine.clone()?)))
                    .collect();
                self.units = sources
                    .iter()
                    .filter_map(|source| {
                        let partition = source.partition.as_ref()?;
                        Some((source.id.clone(), partition.unit.clone()))
                    })
                    .collect();
                false
            }
            RunEvent::Progress {
                step,
                rows,
                bytes,
                elapsed_ms,
                batch,
                parts,
                busy,
            } => {
                let engine = self.engine(step).map(str::to_owned);
                let unit = self.unit(step).to_owned();
                let running = match self
                    .running
                    .iter_mut()
                    .position(|running| running.step == *step)
                {
                    Some(index) => &mut self.running[index],
                    None => {
                        self.running.push(Running {
                            step: step.clone(),
                            pace: Pace::default(),
                            text: String::new(),
                        });
                        let last = self.running.len() - 1;
                        &mut self.running[last]
                    }
                };
                running.pace.see(*elapsed_ms, *rows, *bytes);
                let mut text = amounts(step, *rows, *bytes, *elapsed_ms, *batch);
                if let Some(parts) = parts {
                    text = format!("{text}, {}", parts.describe(&unit));
                }
                let waits_on = live::waits_on(step, engine.as_deref());
                let now = live::describe(&running.pace, busy.as_ref(), waits_on, ", ");
                if !now.is_empty() {
                    text = format!("{text}, {now}");
                }
                running.text = text;
                true
            }
            RunEvent::PartDone(part) if self.run_parts.is_some() => {
                if let Some((_, parts)) = &mut self.run_parts {
                    *parts = Parts {
                        done: part.number,
                        running: u64::from(part.number < part.of),
                        of: part.of,
                    };
                }
                true
            }
            RunEvent::SourceDone { id, .. } => self.end(&Step::Source { id: id.clone() }),
            RunEvent::CombineDone { .. } => self.end(&Step::Combine),
            // Nothing runs once the run has ended, however it ended.
            ended if ended.ends_run() => !std::mem::take(&mut self.running).is_empty(),
            _ => false,
        }
    }

    fn engine(&self, step: &Step) -> Option<&str> {
        let Step::Source { id } = step else {
            return None;
        };
        self.engines
            .iter()
            .find(|(source, _)| source == id)
            .map(|(_, engine)| engine.as_str())
    }

    fn unit(&self, step: &Step) -> &str {
        let found = match step {
            Step::Source { id } => self.units.iter().find(|(source, _)| source == id),
            _ => None,
        };
        found.map_or("range", |(_, unit)| unit.as_str())
    }

    fn end(&mut self, step: &Step) -> bool {
        let before = self.running.len();
        self.running.retain(|running| running.step != *step);
        before != self.running.len()
    }

    /// What the line says now; `None` while no step runs.
    pub fn text(&self) -> Option<String> {
        if self.running.is_empty() {
            return None;
        }
        let steps: Vec<&str> = self
            .running
            .iter()
            .map(|running| running.text.as_str())
            .collect();
        let steps = steps.join(" · ");
        Some(match &self.run_parts {
            Some((unit, parts)) => format!("{}: {steps}", capitalised(&parts.describe(unit))),
            None => steps,
        })
    }
}

/// `Month 3 of 12` from `month 3 of 12`.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `Source lines: 120,000 rows, 3.1 MB, 4 s`, with a lookup's batch.
fn amounts(step: &Step, rows: u64, bytes: u64, elapsed_ms: u64, batch: Option<Batch>) -> String {
    let name = match step {
        Step::Source { id } => format!("Source {id}"),
        Step::Combine => "Combine".to_owned(),
        other => other.to_string(),
    };
    let batch = batch.map_or_else(String::new, |batch| {
        format!(", batch {} of {}", batch.number, batch.of)
    });
    format!(
        "{name}: {} rows, {}, {} s{batch}",
        grouped(rows),
        ByteCount(bytes),
        elapsed_ms / 1000
    )
}

#[cfg(test)]
mod tests {
    use dbrover_report::{Busy, PartitionStart, SourceStart};

    use super::*;

    fn progress(step: Step, rows: u64, batch: Option<Batch>) -> RunEvent {
        RunEvent::Progress {
            step,
            rows,
            bytes: rows * 26,
            elapsed_ms: 4_200,
            batch,
            parts: None,
            busy: None,
        }
    }

    fn source(id: &str) -> Step {
        Step::Source { id: id.to_owned() }
    }

    #[test]
    fn says_what_each_running_step_has_read() {
        let done = |id: &str| RunEvent::SourceDone {
            id: id.to_owned(),
            rows: 1,
            bytes: 1,
            first_row_ms: None,
            elapsed_ms: 1,
            table: Vec::new(),
            warnings: Vec::new(),
            batches: None,
            parts: None,
            phases: None,
            scanned_bytes: None,
        };
        let cases = [
            (
                progress(source("lines"), 120_000, None),
                Some("Source lines: 120,000 rows, 3.1 MB, 4 s"),
            ),
            (
                progress(source("customers"), 40, Some(Batch { number: 2, of: 5 })),
                Some(
                    "Source lines: 120,000 rows, 3.1 MB, 4 s · Source customers: 40 rows, \
                     1.0 KB, 4 s, batch 2 of 5",
                ),
            ),
            (
                progress(source("lines"), 150_000, None),
                Some(
                    "Source lines: 150,000 rows, 3.9 MB, 4 s · Source customers: 40 rows, \
                     1.0 KB, 4 s, batch 2 of 5",
                ),
            ),
            (
                done("lines"),
                Some("Source customers: 40 rows, 1.0 KB, 4 s, batch 2 of 5"),
            ),
            (done("customers"), None),
            (
                RunEvent::Progress {
                    step: source("lines"),
                    rows: 0,
                    bytes: 0,
                    elapsed_ms: 1_000,
                    batch: None,
                    parts: None,
                    busy: Some(Busy {
                        waiting_pct: 97,
                        loading_pct: 0,
                        writing_pct: 0,
                    }),
                },
                Some("Source lines: 0 rows, 0 bytes, 1 s, waiting on PostgreSQL (97%)"),
            ),
            (
                RunEvent::Progress {
                    step: source("lines"),
                    rows: 30_000,
                    bytes: 780_000,
                    elapsed_ms: 2_000,
                    batch: None,
                    parts: None,
                    busy: Some(Busy {
                        waiting_pct: 30,
                        loading_pct: 60,
                        writing_pct: 0,
                    }),
                },
                Some(
                    "Source lines: 30,000 rows, 780.0 KB, 2 s, 30,000 rows/s, 780.0 KB/s, \
                     loading into DuckDB (60%)",
                ),
            ),
            (done("lines"), None),
            (
                progress(Step::Combine, 13, None),
                Some("Combine: 13 rows, 338 bytes, 4 s"),
            ),
        ];
        let mut line = LiveLine::default();
        line.apply(&RunEvent::Started {
            report: "Sales".to_owned(),
            sources: vec![SourceStart {
                id: "lines".to_owned(),
                data_source: "shop".to_owned(),
                attached: false,
                waits_for: None,
                engine: Some("PostgreSQL".to_owned()),
                limits: None,
                partition: None,
            }],
            combine: true,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
            partition: None,
        });
        for (event, expected) in cases {
            line.apply(&event);
            assert_eq!(line.text().as_deref(), expected, "{event:?}");
        }
    }

    #[test]
    fn says_which_range_a_partitioned_run_is_on() {
        let mut line = LiveLine::default();
        line.apply(&RunEvent::Started {
            report: "Events".to_owned(),
            sources: Vec::new(),
            combine: true,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
            partition: Some(dbrover_report::RunPartitionStart {
                unit: "month".to_owned(),
                ranges: 12,
            }),
        });
        line.apply(&progress(Step::Combine, 13, None));
        assert_eq!(
            line.text().as_deref(),
            Some("Month 1 of 12: Combine: 13 rows, 338 bytes, 4 s")
        );
        line.apply(&RunEvent::PartDone(dbrover_report::PartDone {
            step: Step::Combine,
            number: 6,
            of: 12,
            from: "2026-06-01".to_owned(),
            to: "2026-07-01".to_owned(),
            rows: 13,
            bytes: 338,
            first_row_ms: None,
            elapsed_ms: 4_000,
            scanned_bytes: None,
        }));
        assert_eq!(
            line.text().as_deref(),
            Some("Month 7 of 12: Combine: 13 rows, 338 bytes, 4 s")
        );
    }

    #[test]
    fn says_which_ranges_a_partitioned_source_reads() {
        let mut line = LiveLine::default();
        line.apply(&RunEvent::Started {
            report: "Events".to_owned(),
            sources: vec![SourceStart {
                id: "events".to_owned(),
                data_source: "warehouse".to_owned(),
                attached: false,
                waits_for: None,
                engine: None,
                limits: None,
                partition: Some(PartitionStart {
                    unit: "month".to_owned(),
                    parallel: 2,
                }),
            }],
            combine: true,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
            partition: None,
        });
        let cases = [
            ((6, 1), "Source events: 40 rows, 1.0 KB, 4 s, month 7 of 12"),
            (
                (6, 2),
                "Source events: 40 rows, 1.0 KB, 4 s, months 7–8 of 12",
            ),
        ];
        for ((done, running), expected) in cases {
            line.apply(&RunEvent::Progress {
                step: source("events"),
                rows: 40,
                bytes: 40 * 26,
                elapsed_ms: 4_200,
                batch: None,
                parts: Some(Parts {
                    done,
                    running,
                    of: 12,
                }),
                busy: None,
            });
            assert_eq!(line.text().as_deref(), Some(expected));
        }
    }
}
