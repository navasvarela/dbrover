//! The disk a run may use (`docs/reports-at-scale.md`, "Bounding memory and disk", RS3b): its
//! `max_disk`, or less when its work folder's disk has less free, and the failure of a run
//! that grew past it. The app's side measures the run's folders against it and stops the
//! worker; this is what it measures against, and what it says.

use serde::{Deserialize, Serialize};

use crate::{ByteSize, LimitHit, RunFailure, Step};

/// The share of a disk's free space a run may fill, when that is less than its `max_disk`:
/// DuckDB's own default for what it spills, so the disk keeps a little room for everything
/// else on it.
const FREE_SPACE_SHARE_PCT: u64 = 90;

/// What a run may hold on disk, in its work folder and its staging folder together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskBudget {
    pub max_disk: ByteSize,
    /// The work folder's disk's free space when the run started; `None` where the system
    /// doesn't say (Windows).
    pub free: Option<ByteSize>,
}

impl DiskBudget {
    /// The bytes the run may hold: `max_disk`, or most of the free space if that is less.
    pub fn bytes(&self) -> u64 {
        match self.free_share() {
            Some(share) => share.min(self.max_disk.0),
            None => self.max_disk.0,
        }
    }

    /// Whether the disk's free space, not `max_disk`, bounds the run.
    pub fn is_bound_by_disk(&self) -> bool {
        self.free_share()
            .is_some_and(|share| share < self.max_disk.0)
    }

    fn free_share(&self) -> Option<u64> {
        self.free.map(|free| free.0 / 100 * FREE_SPACE_SHARE_PCT)
    }

    /// The failure of a run whose folders came to `used` bytes, past [`Self::bytes`].
    pub fn exceeded(&self, used: u64) -> RunFailure {
        let used = ByteSize(used);
        let limit = LimitHit {
            key: "max_disk".to_owned(),
            value: self.max_disk.to_string(),
            raise_in: "[report]".to_owned(),
            set_in: None,
        };
        let message = match self.free.filter(|_| self.is_bound_by_disk()) {
            Some(free) => format!(
                "the run's files came to {used}, as much of its disk as it may fill (it had \
                 {free} free when the run started) and was stopped; free space on that disk, or \
                 choose a work folder on another disk in Settings › Reports"
            ),
            None => format!(
                "the run's files came to {used}, past {}, its max_disk, and was stopped; load \
                 fewer rows or columns, or raise max_disk",
                self.max_disk
            ),
        };
        RunFailure::at_limit(Step::Worker, message, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn takes_the_smaller_of_max_disk_and_most_of_the_free_space() {
        let cases = [
            (20 * GIB, None, 20 * GIB, false),
            (20 * GIB, Some(100 * GIB), 20 * GIB, false),
            (20 * GIB, Some(10 * GIB), 9 * GIB, true),
        ];
        for (max_disk, free, bytes, by_disk) in cases {
            let budget = DiskBudget {
                max_disk: ByteSize(max_disk),
                free: free.map(ByteSize),
            };
            assert_eq!(budget.bytes(), bytes, "{budget:?}");
            assert_eq!(budget.is_bound_by_disk(), by_disk, "{budget:?}");
        }
    }

    #[test]
    fn says_which_bound_the_run_passed() {
        let by_limit = DiskBudget {
            max_disk: ByteSize(GIB),
            free: Some(ByteSize(100 * GIB)),
        }
        .exceeded(2 * GIB);
        assert!(
            by_limit.message.contains("its max_disk"),
            "{}",
            by_limit.message
        );
        let limit = by_limit.limit.as_deref().map(|limit| limit.key.as_str());
        assert_eq!(limit, Some("max_disk"));

        let by_disk = DiskBudget {
            max_disk: ByteSize(20 * GIB),
            free: Some(ByteSize(2 * GIB)),
        }
        .exceeded(2 * GIB);
        assert!(
            by_disk.message.contains("another disk"),
            "{}",
            by_disk.message
        );
    }
}
