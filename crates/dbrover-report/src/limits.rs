//! The limits a run stops at, and their defaults (`docs/reporting.md`, "Guardrails in RP1"
//! and "Guardrails in RP2").
//!
//! The defaults are generous for a report a person runs from the app, and small enough that a
//! source without a filter stops long before it fills a laptop's memory or disk. A report
//! changes them in its file; RP4's metrics will say whether they should move.

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

/// The most rows a source returns. A general ledger past a million rows sets its own.
pub const DEFAULT_MAX_ROWS: u64 = 1_000_000;
/// The most bytes of values a source returns, counted as the text each value arrives as.
pub const DEFAULT_MAX_BYTES: ByteSize = ByteSize(1 << 30);
/// How long a source's statement may run, as a console's statement timeout bounds one.
pub const DEFAULT_SOURCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The most bytes of values an output writes, before its format's own overhead.
pub const DEFAULT_MAX_SIZE: ByteSize = ByteSize(1 << 30);
/// How long a whole run may take before the app stops the worker.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How long the combine step's query may run.
pub const DEFAULT_COMBINE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The memory the run's DuckDB holds before it spills to disk: room for joins and sorts of a
/// few million rows, and a fraction of a laptop's memory.
pub const DEFAULT_MEMORY_LIMIT: ByteSize = ByteSize(2 << 30);
/// What the run's DuckDB may spill to disk, past which the combine step fails.
pub const DEFAULT_MAX_TEMP_SIZE: ByteSize = ByteSize(10 << 30);

/// A source's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLimits {
    pub max_rows: u64,
    pub max_bytes: ByteSize,
    pub timeout: Duration,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: DEFAULT_SOURCE_TIMEOUT,
        }
    }
}

/// The limits of the run's DuckDB, which the sources load into and the combine step runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombineLimits {
    pub timeout: Duration,
    pub memory_limit: ByteSize,
    /// `None` for half the processor's cores, which leaves the app and the rest of the machine
    /// room while a report runs.
    pub threads: Option<NonZeroUsize>,
    pub max_temp_size: ByteSize,
}

impl CombineLimits {
    /// The threads DuckDB runs on: those set, or half the processor's cores.
    pub fn threads(&self) -> usize {
        self.threads.map_or_else(
            || {
                std::thread::available_parallelism()
                    .map_or(1, |cores| cores.get() / 2)
                    .max(1)
            },
            NonZeroUsize::get,
        )
    }
}

impl Default for CombineLimits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_COMBINE_TIMEOUT,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            threads: None,
            max_temp_size: DEFAULT_MAX_TEMP_SIZE,
        }
    }
}

/// An output's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    pub max_size: ByteSize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_SIZE,
        }
    }
}

/// A number of bytes, as a report file writes it: `"500 MB"`, `"1 GiB"`, or a number of
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl ByteSize {
    /// The units a size can be written in, decimal and binary, as people and operating
    /// systems write them.
    const UNITS: [(&str, u64); 9] = [
        ("B", 1),
        ("KB", 1_000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("TB", 1_000_000_000_000),
        ("KIB", 1 << 10),
        ("MIB", 1 << 20),
        ("GIB", 1 << 30),
        ("TIB", 1 << 40),
    ];

    /// `"500 MB"`, `"1GiB"`, `"2 gb"`: a whole number and a unit, or a whole number alone.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let digits_end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (number, unit) = text.split_at(digits_end);
        let number: u64 = number.parse().ok()?;
        let unit = unit.trim().to_ascii_uppercase();
        let multiplier = if unit.is_empty() {
            1
        } else {
            Self::UNITS
                .iter()
                .find(|(name, _)| *name == unit)
                .map(|(_, multiplier)| *multiplier)?
        };
        number.checked_mul(multiplier).map(Self)
    }
}

/// In the largest unit it is a whole number of, as a report file would write it (`1 GiB`,
/// `500 MB`), so an error names the limit as it was set; else in bytes.
impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const LARGEST_FIRST: [(&str, u64); 8] = [
            ("TiB", 1 << 40),
            ("TB", 1_000_000_000_000),
            ("GiB", 1 << 30),
            ("GB", 1_000_000_000),
            ("MiB", 1 << 20),
            ("MB", 1_000_000),
            ("KiB", 1 << 10),
            ("KB", 1_000),
        ];
        match LARGEST_FIRST
            .iter()
            .find(|(_, unit)| self.0 >= *unit && self.0.is_multiple_of(*unit))
        {
            Some((name, unit)) => write!(f, "{} {name}", self.0 / unit),
            None => write!(f, "{} bytes", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sizes_as_people_write_them() {
        let cases = [
            ("500 MB", Some(500_000_000)),
            ("1GiB", Some(1 << 30)),
            ("2 gb", Some(2_000_000_000)),
            ("4096", Some(4096)),
            ("12 B", Some(12)),
            ("1.5 GB", None),
            ("MB", None),
            ("-1 MB", None),
            ("5 parsecs", None),
            ("99999999999 TB", None),
        ];
        for (text, bytes) in cases {
            assert_eq!(ByteSize::parse(text), bytes.map(ByteSize), "{text:?}");
        }
        assert_eq!(ByteSize(1 << 30).to_string(), "1 GiB");
        assert_eq!(ByteSize(500_000_000).to_string(), "500 MB");
        assert_eq!(ByteSize(1_500).to_string(), "1500 bytes");
    }
}
