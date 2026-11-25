//! When a Server activity tab reads next: on its timer while shown, at once when asked, and
//! never while a read is out (W15).

use std::time::{Duration, Instant};

/// The intervals the toolbar offers, in seconds.
pub(super) const CHOICES: [u64; 5] = [1, 2, 5, 10, 30];

/// What a tab paused from the start reads at once resumed.
const RESUMED_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(super) struct Refresh {
    every: Duration,
    paused: bool,
    /// A read is out: the next waits for its answer, so a slow server isn't sent more.
    waiting: bool,
    /// When the last answer came; the next read is due `every` after it.
    answered: Option<Instant>,
    /// Asked for a read now (Refresh, or after a stop), timer or not.
    wanted: bool,
}

impl Refresh {
    /// A tab reading every `every`, or paused when `None`. Either way the tab's first read is
    /// sent when it opens.
    pub(super) fn new(every: Option<Duration>) -> Self {
        Self {
            every: every.unwrap_or(RESUMED_EVERY),
            paused: every.is_none(),
            waiting: false,
            answered: None,
            wanted: false,
        }
    }

    pub(super) fn every(&self) -> Duration {
        self.every
    }

    pub(super) fn set_every(&mut self, every: Duration) {
        self.every = every;
    }

    pub(super) fn is_paused(&self) -> bool {
        self.paused
    }

    pub(super) fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub(super) fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    pub(super) fn read_now(&mut self) {
        self.wanted = true;
    }

    /// Whether to send a read, for a tab that is shown.
    pub(super) fn is_due(&self, now: Instant) -> bool {
        if self.waiting {
            return false;
        }
        self.wanted || self.answered.is_none() || self.left(now) == Some(Duration::ZERO)
    }

    pub(super) fn sent(&mut self) {
        self.waiting = true;
        self.wanted = false;
    }

    /// The read came back, or won't: its connection failed.
    pub(super) fn answered(&mut self, now: Instant) {
        self.waiting = false;
        self.answered = Some(now);
    }

    /// How long until the timer's next read; `None` while paused or waiting.
    pub(super) fn left(&self, now: Instant) -> Option<Duration> {
        if self.paused || self.waiting {
            return None;
        }
        let since = self
            .answered
            .map_or(self.every, |at| now.duration_since(at));
        Some(self.every.saturating_sub(since))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIVE: Duration = Duration::from_secs(5);

    #[test]
    fn reads_at_once_then_every_interval_after_each_answer() {
        let start = Instant::now();
        let mut refresh = Refresh::new(Some(FIVE));
        assert!(refresh.is_due(start));

        refresh.sent();
        assert!(!refresh.is_due(start + FIVE * 3), "one read at a time");
        refresh.answered(start + Duration::from_secs(1));

        assert!(!refresh.is_due(start + Duration::from_secs(5)));
        assert_eq!(
            refresh.left(start + Duration::from_secs(5)),
            Some(Duration::from_secs(1))
        );
        assert!(refresh.is_due(start + Duration::from_secs(6)));
    }

    #[test]
    fn a_paused_tab_reads_only_when_asked() {
        let start = Instant::now();
        let mut refresh = Refresh::new(Some(FIVE));
        refresh.sent();
        refresh.answered(start);
        refresh.toggle_pause();

        assert!(!refresh.is_due(start + FIVE * 10));
        assert_eq!(refresh.left(start), None);
        refresh.read_now();
        assert!(refresh.is_due(start));
        refresh.sent();
        assert!(!refresh.is_due(start));
    }

    #[test]
    fn opens_paused_without_an_interval_and_still_reads_once() {
        let start = Instant::now();
        let mut refresh = Refresh::new(None);
        assert!(refresh.is_paused());
        assert!(refresh.is_due(start));
        refresh.sent();
        refresh.answered(start);
        assert!(!refresh.is_due(start + FIVE * 10));

        refresh.toggle_pause();
        assert_eq!(refresh.every(), FIVE);
        assert!(refresh.is_due(start + FIVE));
    }
}
