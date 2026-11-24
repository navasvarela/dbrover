//! What a statement would cost on an engine that bills each read, and the data source's guard
//! that judges it before the statement runs: C0's cost guards (`docs/cloud-data.md`, C0e),
//! DynamoDB's first (`docs/nosql.md`, N5).

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a statement would read, as the driver estimates it before running it: what a data
/// source's [`CostGuard`] judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostEstimate {
    /// Every item of a table or index, as DynamoDB's Scan reads it.
    WholeTable(FullScan),
    /// The bytes the statement will scan, whatever it returns, as a dry run reports them
    /// (BigQuery, C4).
    Bytes(ScanEstimate),
}

/// The bytes a statement will scan, as its engine estimates them before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanEstimate {
    pub bytes: ByteCount,
    /// How close to `bytes` the statement's own read will be.
    pub precision: Precision,
}

/// What an estimate of a statement's bytes says about the read itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Precision {
    /// What the statement scans, as the engine bills it (BigQuery's dry run of a table).
    #[default]
    Exactly,
    /// At least that many: BigQuery's dry run over an external table can say no more.
    AtLeast,
    /// At most that many: a lake's files hold them, and DuckDB's pruning reads the parts that
    /// matter, so the read is this or less (RS7).
    AtMost,
}

impl ScanEstimate {
    /// An estimate the engine gives as exact.
    pub fn exactly(bytes: ByteCount) -> Self {
        Self {
            bytes,
            precision: Precision::Exactly,
        }
    }

    /// An estimate that bounds the read from above, which is all a lake's files say.
    pub fn at_most(bytes: ByteCount) -> Self {
        Self {
            bytes,
            precision: Precision::AtMost,
        }
    }

    /// Whether the statement may scan more than [`ScanEstimate::bytes`].
    pub fn may_scan_more(&self) -> bool {
        self.precision == Precision::AtLeast
    }
}

impl fmt::Display for ScanEstimate {
    /// `2.5 GB`, `at least 2.5 GB` or `at most 2.5 GB`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.precision {
            Precision::Exactly => {}
            Precision::AtLeast => f.write_str("at least ")?,
            Precision::AtMost => f.write_str("at most ")?,
        }
        self.bytes.fmt(f)
    }
}

impl CostEstimate {
    /// What the statement reads, for the question before it runs and the refusals.
    pub fn describe(&self) -> String {
        match self {
            Self::WholeTable(scan) => scan.describe(),
            Self::Bytes(scan) if scan.precision == Precision::AtMost => {
                format!(
                    "This reads {scan}, and less where its filters and columns let the \
                         engine skip part of it."
                )
            }
            Self::Bytes(scan) => format!("This scans {scan}, billed whatever it returns."),
        }
    }

    /// The question's heading: what running it anyway does.
    pub fn heading(&self) -> String {
        match self {
            Self::WholeTable(scan) => format!("Read all of {}?", scan.table),
            Self::Bytes(scan) if scan.precision == Precision::AtMost => format!("Read {scan}?"),
            Self::Bytes(scan) => format!("Scan {scan}?"),
        }
    }
}

/// A statement that would read every item of a table or index, which an engine that bills each
/// read charges for whatever its filter keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullScan {
    pub table: String,
    /// The secondary index read instead of the table, when the statement names one.
    pub index: Option<String>,
    /// The key whose value, given at the top of the statement's condition, would have read
    /// only its partitions.
    pub partition_key: String,
}

impl FullScan {
    /// Why the statement reads everything.
    pub fn describe(&self) -> String {
        let read = match &self.index {
            Some(index) => format!("every item of {}'s index {index}", self.table),
            None => format!("every item of {}", self.table),
        };
        format!(
            "This reads {read}, billed for each item read whatever the WHERE keeps: it has no = \
             or IN on the partition key {} at the top of its WHERE.",
            self.partition_key
        )
    }
}

/// A number of bytes, shown in the decimal units cloud bills use (1 GB is 10⁹ bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ByteCount(pub u64);

impl fmt::Display for ByteCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
        if self.0 < 1000 {
            return write!(f, "{} bytes", self.0);
        }
        // u64 to f64 loses precision past 2⁵³ bytes, far below what one decimal shows.
        #[allow(clippy::cast_precision_loss)]
        let mut value = self.0 as f64 / 1000.0;
        let mut unit = UNITS[0];
        for next in &UNITS[1..] {
            if value < 1000.0 {
                break;
            }
            value /= 1000.0;
            unit = next;
        }
        write!(f, "{value:.1} {unit}")
    }
}

/// What a data source on an engine that bills each read asks about before a statement runs,
/// and how long one may run on an engine that bills time, kept in the engine's own settings
/// (`ConnectionConfig::cost_guard`). Its keys are written only when they differ from the
/// default: `cost_guard = "off"`, as N5 wrote it, `ask_above_bytes` and `stop_after_seconds`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostGuard {
    #[serde(
        rename = "cost_guard",
        default,
        skip_serializing_if = "ScanGuard::is_default"
    )]
    pub whole_table: ScanGuard,
    /// Asks before a statement the driver estimates will scan more than this many bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_above_bytes: Option<ByteCount>,
    /// How long a statement may run before it is stopped, on an engine that bills the time a
    /// warehouse runs: kept by the engine where it takes such a limit (Snowflake's `timeout`,
    /// C5f), else by the driver cancelling the statement at the poll that finds it past the
    /// limit (Databricks, C6f). It holds nothing back: nothing says beforehand how long a
    /// statement will take.
    #[serde(
        rename = "stop_after_seconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_after: Option<TimeLimit>,
}

/// A statement's longest run, in whole seconds, never zero: a limit of nothing would stop
/// every statement, and an engine may read zero as no limit at all (Snowflake reads it as its
/// longest, seven days).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeLimit(std::num::NonZeroU32);

impl TimeLimit {
    /// `seconds` as a limit; `None` for zero.
    pub fn from_seconds(seconds: u32) -> Option<Self> {
        std::num::NonZeroU32::new(seconds).map(Self)
    }

    pub fn seconds(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for TimeLimit {
    /// `45 s`, `10 min`, `1 h 30 min`: what the limit's messages say.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.seconds();
        let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
        match (hours, minutes, rest) {
            (0, 0, rest) => write!(f, "{rest} s"),
            (0, minutes, 0) => write!(f, "{minutes} min"),
            (hours, 0, 0) => write!(f, "{hours} h"),
            (hours, minutes, 0) => write!(f, "{hours} h {minutes} min"),
            (0, minutes, rest) => write!(f, "{minutes} min {rest} s"),
            (hours, minutes, rest) => write!(f, "{hours} h {minutes} min {rest} s"),
        }
    }
}

/// Whether a statement that would read a whole table is asked about first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanGuard {
    /// A console asks first; Run to file and the MCP server refuse.
    #[default]
    AskBeforeScans,
    /// Nothing asks. Each page a result shows still reads only so much.
    Off,
}

impl ScanGuard {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// How the result of a statement is going to be read, which changes what it costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPace {
    /// To its end, as a console's run, Run to file and the MCP server read it.
    Whole,
    /// A page at a time, as **Open data** reads it: an engine that reads a page per request,
    /// as DynamoDB does, bills only the pages read.
    PageAtATime,
}

impl CostGuard {
    /// A guard that holds nothing.
    pub const OFF: Self = Self {
        whole_table: ScanGuard::Off,
        ask_above_bytes: None,
        stop_after: None,
    };

    /// Whether the driver is worth asking for an estimate: something would be held.
    pub fn is_on(&self) -> bool {
        self.whole_table == ScanGuard::AskBeforeScans || self.ask_above_bytes.is_some()
    }

    /// Whether a statement estimated as `estimate`, read at `pace`, is held back until someone
    /// says yes (or refused where nobody can be asked). A byte estimate is what the whole
    /// statement scans, so reading its result a page at a time costs as much.
    pub fn holds(&self, estimate: &CostEstimate, pace: ReadPace) -> bool {
        match estimate {
            CostEstimate::WholeTable(_) => {
                self.whole_table == ScanGuard::AskBeforeScans && pace == ReadPace::Whole
            }
            CostEstimate::Bytes(scan) => {
                self.ask_above_bytes.is_some_and(|limit| scan.bytes > limit)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan() -> CostEstimate {
        CostEstimate::WholeTable(FullScan {
            table: "orders".to_owned(),
            index: None,
            partition_key: "customer".to_owned(),
        })
    }

    #[test]
    fn a_guard_holds_what_its_settings_ask_about() {
        const GB: u64 = 1_000_000_000;
        let ask = ScanGuard::AskBeforeScans;
        let off = ScanGuard::Off;
        let limit = Some(ByteCount(10 * GB));
        let bytes = |count| CostEstimate::Bytes(ScanEstimate::exactly(ByteCount(count)));
        let whole = ReadPace::Whole;
        let paged = ReadPace::PageAtATime;
        for (whole_table, ask_above_bytes, estimate, pace, held) in [
            (ask, None, scan(), whole, true),
            (ask, None, scan(), paged, false),
            (off, None, scan(), whole, false),
            (off, limit, scan(), whole, false),
            (ask, None, bytes(100 * GB), whole, false),
            (off, limit, bytes(10 * GB), whole, false),
            (off, limit, bytes(10 * GB + 1), whole, true),
            (off, limit, bytes(10 * GB + 1), paged, true),
            (ask, limit, bytes(GB), paged, false),
        ] {
            let guard = CostGuard {
                whole_table,
                ask_above_bytes,
                stop_after: None,
            };
            assert_eq!(
                guard.holds(&estimate, pace),
                held,
                "{guard:?} {estimate:?} {pace:?}"
            );
        }
        assert!(
            !CostGuard {
                whole_table: off,
                ask_above_bytes: None,
                stop_after: TimeLimit::from_seconds(600),
            }
            .is_on(),
            "a time limit holds nothing back"
        );
    }

    #[test]
    fn time_limits_say_their_length_and_are_never_zero() {
        assert_eq!(TimeLimit::from_seconds(0), None);
        for (seconds, shown) in [
            (45, "45 s"),
            (600, "10 min"),
            (3600, "1 h"),
            (5400, "1 h 30 min"),
            (90, "1 min 30 s"),
            (3661, "1 h 1 min 1 s"),
        ] {
            let limit = TimeLimit::from_seconds(seconds).expect("not zero");
            assert_eq!(limit.to_string(), shown);
            assert_eq!(limit.seconds(), seconds);
        }
    }

    #[test]
    fn bytes_show_in_decimal_units() {
        for (bytes, shown) in [
            (0, "0 bytes"),
            (999, "999 bytes"),
            (1_000, "1.0 KB"),
            (12_345_678, "12.3 MB"),
            (999_999_999_999, "1000.0 GB"),
            (1_500_000_000_000, "1.5 TB"),
            (u64::MAX, "18446.7 PB"),
        ] {
            assert_eq!(ByteCount(bytes).to_string(), shown);
        }
    }

    #[test]
    fn each_estimate_says_what_running_it_costs() {
        assert_eq!(scan().heading(), "Read all of orders?");
        assert!(scan().describe().contains("partition key customer"));
        let bytes = CostEstimate::Bytes(ScanEstimate::exactly(ByteCount(2_500_000_000)));
        assert_eq!(bytes.heading(), "Scan 2.5 GB?");
        assert_eq!(
            bytes.describe(),
            "This scans 2.5 GB, billed whatever it returns."
        );
        let at_least = CostEstimate::Bytes(ScanEstimate {
            bytes: ByteCount(2_500_000_000),
            precision: Precision::AtLeast,
        });
        assert_eq!(at_least.heading(), "Scan at least 2.5 GB?");
        assert!(at_least.describe().contains("scans at least 2.5 GB"));
        // A lake's files bound the read from above, so what it says of itself differs: the
        // bytes are what a query may read, not what it is billed for (RS7).
        let at_most = CostEstimate::Bytes(ScanEstimate::at_most(ByteCount(2_500_000_000)));
        assert_eq!(at_most.heading(), "Read at most 2.5 GB?");
        assert!(
            at_most
                .describe()
                .starts_with("This reads at most 2.5 GB, and less where"),
            "{}",
            at_most.describe()
        );
        let scan_of = |estimate: &CostEstimate| match estimate {
            CostEstimate::Bytes(scan) => scan.may_scan_more(),
            CostEstimate::WholeTable(_) => unreachable!("both are byte estimates"),
        };
        assert!(scan_of(&at_least));
        assert!(!scan_of(&at_most));
    }
}
