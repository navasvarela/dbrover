//! The limits a run stops at, and their defaults (`docs/reporting.md`, "Guardrails in RP1",
//! "Guardrails in RP2" and "Settings › Reports").
//!
//! The constants are generous for a report a person runs from the app, and small enough that a
//! source without a filter stops long before it fills a laptop's memory or disk. Settings ›
//! Reports changes the defaults for every report ([`LimitDefaults`]), and a report's file
//! changes them key by key ([`SourceLimitsSet`] and the others), which planning puts together.

use std::num::NonZeroUsize;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use dbrover_core::ByteSize;

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

/// The limits a run uses where its report's file leaves one out: the constants above, or what
/// Settings › Reports sets. The app sends them to the worker with each run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitDefaults {
    pub source: SourceLimits,
    pub combine: CombineLimits,
    pub output: OutputLimits,
    /// How long a whole run may take.
    pub run_timeout: Duration,
}

impl Default for LimitDefaults {
    fn default() -> Self {
        Self {
            source: SourceLimits::default(),
            combine: CombineLimits::default(),
            output: OutputLimits::default(),
            run_timeout: DEFAULT_RUN_TIMEOUT,
        }
    }
}

/// A source's limits as its file sets them; `None` where it leaves one to the defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceLimitsSet {
    pub max_rows: Option<u64>,
    pub max_bytes: Option<ByteSize>,
    pub timeout: Option<Duration>,
}

impl SourceLimitsSet {
    /// These limits, with `defaults` where none is set.
    pub fn or(self, defaults: SourceLimits) -> SourceLimits {
        SourceLimits {
            max_rows: self.max_rows.unwrap_or(defaults.max_rows),
            max_bytes: self.max_bytes.unwrap_or(defaults.max_bytes),
            timeout: self.timeout.unwrap_or(defaults.timeout),
        }
    }
}

/// `[combine]`'s limits as the file sets them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CombineLimitsSet {
    pub timeout: Option<Duration>,
    pub memory_limit: Option<ByteSize>,
    pub threads: Option<NonZeroUsize>,
    pub max_temp_size: Option<ByteSize>,
}

impl CombineLimitsSet {
    /// These limits, with `defaults` where none is set.
    pub fn or(self, defaults: CombineLimits) -> CombineLimits {
        CombineLimits {
            timeout: self.timeout.unwrap_or(defaults.timeout),
            memory_limit: self.memory_limit.unwrap_or(defaults.memory_limit),
            threads: self.threads.or(defaults.threads),
            max_temp_size: self.max_temp_size.unwrap_or(defaults.max_temp_size),
        }
    }
}

/// An output's limits as the file sets them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputLimitsSet {
    pub max_size: Option<ByteSize>,
}

impl OutputLimitsSet {
    /// These limits, with `defaults` where none is set.
    pub fn or(self, defaults: OutputLimits) -> OutputLimits {
        OutputLimits {
            max_size: self.max_size.unwrap_or(defaults.max_size),
        }
    }
}

/// A source's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
