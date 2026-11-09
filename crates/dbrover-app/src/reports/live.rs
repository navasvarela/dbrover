//! A running step as the tab and `dbrover report run --progress` say it (RS1d): its pace over
//! the last progress event, and what it is mostly doing.

use dbrover_core::ByteCount;
use dbrover_report::{Busy, Step, grouped};

/// What a step waits on when the run's start didn't name its engine.
const SOME_DATABASE: &str = "its database";

/// A running step's rows and bytes a second, between its last two progress events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pace {
    last: Option<Read>,
    rate: Option<Read>,
}

/// What a step has read, at a time in its run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Read {
    elapsed_ms: u64,
    rows: u64,
    bytes: u64,
}

impl Pace {
    /// Takes in a progress event's numbers.
    pub fn see(&mut self, elapsed_ms: u64, rows: u64, bytes: u64) {
        let now = Read {
            elapsed_ms,
            rows,
            bytes,
        };
        if let Some(last) = self.last
            && elapsed_ms > last.elapsed_ms
        {
            let per_second = |amount: u64, before: u64| {
                amount.saturating_sub(before) * 1000 / (elapsed_ms - last.elapsed_ms)
            };
            self.rate = Some(Read {
                elapsed_ms: elapsed_ms - last.elapsed_ms,
                rows: per_second(rows, last.rows),
                bytes: per_second(bytes, last.bytes),
            });
        }
        self.last = Some(now);
    }

    /// `30,000 rows/s, 1.2 MB/s`, once two events have come.
    pub fn describe(&self) -> Option<String> {
        self.rate
            .map(|rate| format!("{} rows/s, {}/s", grouped(rate.rows), ByteCount(rate.bytes)))
    }
}

/// What `step` waits on: its engine, DuckDB for the combine step.
pub fn waits_on<'a>(step: &Step, engine: Option<&'a str>) -> &'a str {
    match step {
        Step::Combine => "DuckDB",
        _ => engine.unwrap_or(SOME_DATABASE),
    }
}

/// The pace and what the step mostly does, as `30,000 rows/s, 1.2 MB/s · waiting on
/// PostgreSQL (92%)`; empty before either is known.
pub fn describe(pace: &Pace, busy: Option<&Busy>, waits_on: &str, separator: &str) -> String {
    let parts: Vec<String> = [
        pace.describe(),
        busy.and_then(|busy| busy.describe(waits_on)),
    ]
    .into_iter()
    .flatten()
    .collect();
    parts.join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paces_a_step_by_its_last_two_events() {
        let mut pace = Pace::default();
        pace.see(1_000, 10_000, 1_000_000);
        assert_eq!(pace.describe(), None);
        pace.see(3_000, 70_000, 4_000_000);
        assert_eq!(pace.describe().as_deref(), Some("30,000 rows/s, 1.5 MB/s"));
        let busy = Busy {
            waiting_pct: 92,
            loading_pct: 0,
            writing_pct: 5,
        };
        let source = Step::Source {
            id: "tx".to_owned(),
        };
        assert_eq!(
            describe(
                &pace,
                Some(&busy),
                waits_on(&source, Some("PostgreSQL")),
                " · "
            ),
            "30,000 rows/s, 1.5 MB/s · waiting on PostgreSQL (92%)"
        );
        assert_eq!(
            describe(
                &Pace::default(),
                Some(&busy),
                waits_on(&Step::Combine, None),
                ", "
            ),
            "waiting on DuckDB (92%)"
        );
    }
}
