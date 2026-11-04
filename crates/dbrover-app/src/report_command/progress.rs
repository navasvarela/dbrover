//! `dbrover report run --progress`: a line on a terminal's standard error that says, about once
//! a second, what each running step has read so far, rewritten in place, and cleared before
//! any other line (`docs/reporting.md`, "Finishing RP6"). RS1d adds throughput and what a step
//! waits on to the same line.

use dbrover_core::ByteCount;
use dbrover_report::{Batch, RunEvent, Step, grouped};

/// Moves to the start of the terminal's line and erases it: a carriage return, then ANSI's
/// "erase in line", which every terminal DbRover runs in reads, Windows' since Windows 10.
pub const CLEAR_LINE: &str = "\r\x1b[2K";

/// The running steps, as their last progress event left them.
#[derive(Debug, Default)]
pub struct LiveLine {
    running: Vec<(Step, String)>,
}

impl LiveLine {
    /// Takes in `event`. Returns whether the line's text changed.
    pub fn apply(&mut self, event: &RunEvent) -> bool {
        match event {
            RunEvent::Progress {
                step,
                rows,
                bytes,
                elapsed_ms,
                batch,
            } => {
                let text = amounts(step, *rows, *bytes, *elapsed_ms, *batch);
                match self.running.iter_mut().find(|(running, _)| running == step) {
                    Some((_, shown)) => *shown = text,
                    None => self.running.push((step.clone(), text)),
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

    fn end(&mut self, step: &Step) -> bool {
        let before = self.running.len();
        self.running.retain(|(running, _)| running != step);
        before != self.running.len()
    }

    /// What the line says now; `None` while no step runs.
    pub fn text(&self) -> Option<String> {
        if self.running.is_empty() {
            return None;
        }
        let steps: Vec<&str> = self.running.iter().map(|(_, text)| text.as_str()).collect();
        Some(steps.join(" · "))
    }
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
    use super::*;

    fn progress(step: Step, rows: u64, batch: Option<Batch>) -> RunEvent {
        RunEvent::Progress {
            step,
            rows,
            bytes: rows * 26,
            elapsed_ms: 4_200,
            batch,
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
                progress(Step::Combine, 13, None),
                Some("Combine: 13 rows, 338 bytes, 4 s"),
            ),
        ];
        let mut line = LiveLine::default();
        for (event, expected) in cases {
            line.apply(&event);
            assert_eq!(line.text().as_deref(), expected, "{event:?}");
        }
    }
}
