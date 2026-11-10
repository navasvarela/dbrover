//! What a running step spends each second on, for its progress events
//! (`docs/reports-at-scale.md`, "Live progress and the bottleneck", RS1d): waiting on its
//! engine (a source's database, or DuckDB for the combine step), loading into DuckDB, or
//! writing the outputs. A step waiting minutes for its first row still says so each second.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::Busy;
use crate::run::PROGRESS_EVERY;

/// What a step did since its last progress event.
pub(crate) struct ProgressClock {
    since: Instant,
    waiting: Duration,
    loading: Duration,
    writing: Duration,
    /// How much of the wait going on now an earlier lap already counted.
    wait_counted: Duration,
    /// How much of the wait for the loader going on now an earlier lap already counted.
    load_counted: Duration,
}

impl ProgressClock {
    pub(crate) fn new() -> Self {
        Self {
            since: Instant::now(),
            waiting: Duration::ZERO,
            loading: Duration::ZERO,
            writing: Duration::ZERO,
            wait_counted: Duration::ZERO,
            load_counted: Duration::ZERO,
        }
    }

    /// A wait of `time` that has ended.
    pub(crate) fn waited(&mut self, time: Duration) {
        self.waiting += time.saturating_sub(self.wait_counted);
        self.wait_counted = Duration::ZERO;
    }

    /// A wait of `time` for DuckDB to load what came before, which has ended.
    pub(crate) fn loaded(&mut self, time: Duration) {
        self.loading += time.saturating_sub(self.load_counted);
        self.load_counted = Duration::ZERO;
    }

    pub(crate) fn wrote(&mut self, time: Duration) {
        self.writing += time;
    }

    /// Whether a progress event is due.
    pub(crate) fn due(&self) -> bool {
        self.since.elapsed() >= PROGRESS_EVERY
    }

    /// The shares of the time since the last lap, `waiting_now` being how long the wait for
    /// the engine going on has lasted so far, and starts the next lap.
    pub(crate) fn lap(&mut self, waiting_now: Duration) -> Busy {
        self.waiting += waiting_now.saturating_sub(self.wait_counted);
        self.end_lap(waiting_now, Duration::ZERO)
    }

    /// As [`Self::lap`], during a wait for DuckDB to load what came before, which has lasted
    /// `loading_now` so far.
    pub(crate) fn lap_loading(&mut self, loading_now: Duration) -> Busy {
        self.loading += loading_now.saturating_sub(self.load_counted);
        self.end_lap(Duration::ZERO, loading_now)
    }

    fn end_lap(&mut self, waiting_now: Duration, loading_now: Duration) -> Busy {
        let interval = self.since.elapsed();
        let busy = Busy {
            waiting_pct: percent(self.waiting, interval),
            loading_pct: percent(self.loading, interval),
            writing_pct: percent(self.writing, interval),
        };
        *self = Self {
            wait_counted: waiting_now,
            load_counted: loading_now,
            ..Self::new()
        };
        busy
    }
}

fn percent(part: Duration, whole: Duration) -> u8 {
    if whole.is_zero() {
        return 0;
    }
    let share = (part.as_secs_f64() / whole.as_secs_f64() * 100.0).round();
    // Clamped: each part is measured apart from the whole, so it can pass it by a little.
    share.clamp(0.0, 100.0) as u8
}

/// Awaits `work`, calling `tick` with how long it has waited so far every
/// [`PROGRESS_EVERY`] it takes; returns its output and how long it took.
///
/// `work` is boxed: opening a source is the whole of connecting to any engine, a future large
/// enough that keeping it on the stack beside this one overflowed a test thread's 2 MiB stack in
/// a debug build on Linux, and the worker's threads have no more.
pub(crate) async fn waiting<T>(
    work: impl Future<Output = T>,
    mut tick: impl FnMut(Duration),
) -> (T, Duration) {
    let started = Instant::now();
    let mut work = Box::pin(work);
    loop {
        tokio::select! {
            output = &mut work => return (output, started.elapsed()),
            () = tokio::time::sleep(PROGRESS_EVERY) => tick(started.elapsed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_a_wait_once_across_the_laps_it_spans() {
        let mut clock = ProgressClock::new();
        // A lap during a wait counts what it has lasted so far...
        let busy = clock.lap(Duration::from_secs(5));
        assert_eq!(busy.waiting_pct, 100);
        // ...and when it ends, only the rest is new.
        clock.waited(Duration::from_secs(5));
        assert_eq!(clock.waiting, Duration::ZERO);
        clock.waited(Duration::from_millis(30));
        assert_eq!(clock.waiting, Duration::from_millis(30));
    }

    #[test]
    fn names_what_a_step_mostly_does() {
        let cases = [
            ((92, 0, 3), Some("waiting on PostgreSQL (92%)")),
            ((10, 70, 0), Some("loading into DuckDB (70%)")),
            ((5, 0, 80), Some("writing the files (80%)")),
            ((10, 10, 10), None),
        ];
        for ((waiting_pct, loading_pct, writing_pct), expected) in cases {
            let busy = Busy {
                waiting_pct,
                loading_pct,
                writing_pct,
            };
            assert_eq!(busy.describe("PostgreSQL").as_deref(), expected);
        }
    }
}
