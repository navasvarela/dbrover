//! What tells a long report run from a stuck one (`docs/reports-at-scale.md`, RS3e): its last
//! progress, which the app's watchdog holds against the run's `stall_timeout`.
//!
//! Progress events come every second, also while a step waits on its engine (RS1d), so their
//! arrival says nothing. A run has progressed when:
//! - an event other than progress came: a step started or ended, or a file was written;
//! - a progress event says more rows or bytes than the step's last, another batch, or another
//!   step;
//! - the worker used at least [`BUSY_PROCESSOR`] of processor time since the last progress,
//!   which is DuckDB at work (loading, typing, a long combine query) without a row to show;
//! - or the worker doesn't say its processor time (Windows): then every event counts, and only
//!   a worker that stops sending them is stuck.

use std::time::{Duration, Instant};

use dbrover_report::{Batch, RunEvent, Step, TimedEvent};

/// The processor time that counts as the worker being at work since the last progress: far more
/// than its runtime and its once-a-second events use while it waits, and less than one busy
/// thread uses in two seconds.
const BUSY_PROCESSOR: Duration = Duration::from_secs(1);

/// A run's last progress, and what its progress events said then.
#[derive(Debug)]
pub(crate) struct Activity {
    last_progress: Instant,
    step: Option<Step>,
    rows: u64,
    bytes: u64,
    batch: Option<Batch>,
    /// The worker's processor time at the last progress.
    cpu_ms: Option<u64>,
}

impl Activity {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_progress: now,
            step: None,
            rows: 0,
            bytes: 0,
            batch: None,
            cpu_ms: None,
        }
    }

    /// Takes `timed`, which came `now`, as progress if it is.
    pub(crate) fn observe(&mut self, timed: &TimedEvent, now: Instant) {
        if self.is_progress(timed) {
            self.last_progress = now;
            self.cpu_ms = timed.cpu_ms.or(self.cpu_ms);
        }
        if let RunEvent::Progress {
            step,
            rows,
            bytes,
            batch,
            ..
        } = &timed.event
        {
            self.step = Some(step.clone());
            self.rows = *rows;
            self.bytes = *bytes;
            self.batch = *batch;
        }
    }

    fn is_progress(&self, timed: &TimedEvent) -> bool {
        let RunEvent::Progress {
            step,
            rows,
            bytes,
            batch,
            ..
        } = &timed.event
        else {
            return true;
        };
        let moved = self.step.as_ref() != Some(step)
            || *rows > self.rows
            || *bytes > self.bytes
            || *batch != self.batch;
        moved || self.worked(timed.cpu_ms)
    }

    /// Whether the worker was busy since the last progress, by its processor time `cpu_ms` now;
    /// busy when it doesn't say.
    fn worked(&self, cpu_ms: Option<u64>) -> bool {
        let busy_ms = u64::try_from(BUSY_PROCESSOR.as_millis()).unwrap_or(u64::MAX);
        match (cpu_ms, self.cpu_ms) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(now), Some(then)) => now.saturating_sub(then) >= busy_ms,
        }
    }

    /// How long the run has gone without progress, as of `now`.
    pub(crate) fn idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(rows: u64, cpu_ms: Option<u64>) -> TimedEvent {
        TimedEvent {
            at_ms: 0,
            cpu_ms,
            event: RunEvent::Progress {
                step: Step::Combine,
                rows,
                bytes: rows * 10,
                elapsed_ms: 0,
                batch: None,
                busy: None,
            },
        }
    }

    #[test]
    fn counts_rows_steps_and_processor_time_as_progress_and_waiting_as_none() {
        let start = Instant::now();
        let later = |seconds| start + Duration::from_secs(seconds);
        let mut activity = Activity::new(start);
        activity.observe(&progress(0, Some(1_000)), later(1));
        assert_eq!(
            activity.idle(later(1)),
            Duration::ZERO,
            "a step's first event"
        );
        // Waiting: the same rows, and the processor barely used.
        activity.observe(&progress(0, Some(1_050)), later(2));
        activity.observe(&progress(0, Some(1_100)), later(3));
        assert_eq!(activity.idle(later(3)), Duration::from_secs(2));
        // DuckDB at work: no new row, but a second of processor time since the last progress.
        activity.observe(&progress(0, Some(2_000)), later(4));
        assert_eq!(activity.idle(later(4)), Duration::ZERO);
        // Rows.
        activity.observe(&progress(5, Some(2_010)), later(5));
        assert_eq!(activity.idle(later(5)), Duration::ZERO);
        activity.observe(&progress(5, Some(2_020)), later(9));
        assert_eq!(activity.idle(later(9)), Duration::from_secs(4));
        // Any other event.
        let done = RunEvent::CombineDone {
            rows: 5,
            bytes: 50,
            first_row_ms: None,
            elapsed_ms: 0,
            peak_memory: None,
            peak_temp_size: None,
            query_ms: None,
            plan: None,
        };
        activity.observe(&done.at(Duration::ZERO), later(10));
        assert_eq!(activity.idle(later(10)), Duration::ZERO);
    }

    #[test]
    fn counts_every_event_where_the_worker_doesnt_say_its_processor_time() {
        let start = Instant::now();
        let mut activity = Activity::new(start);
        activity.observe(&progress(0, None), start + Duration::from_secs(1));
        activity.observe(&progress(0, None), start + Duration::from_secs(5));
        assert_eq!(
            activity.idle(start + Duration::from_secs(5)),
            Duration::ZERO
        );
        assert_eq!(
            activity.idle(start + Duration::from_secs(8)),
            Duration::from_secs(3),
            "a worker that stops sending events is idle"
        );
    }
}
