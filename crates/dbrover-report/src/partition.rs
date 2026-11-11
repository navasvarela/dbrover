//! Partitions (`docs/reports-at-scale.md`, "Partitioning" and "RS4 against today's code"): a
//! source's query read in ranges of time or of an integer key, each range a statement that
//! binds `:part_from` and `:part_to`.
//!
//! Ranges come from the file and the run's values alone, never from the data, so two runs with
//! the same values read the same ranges: what RS5 needs to skip the ones a stopped run finished.

use std::collections::BTreeMap;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use dbrover_export::ColumnType;
use dbrover_language::Literal;
use jiff::Span;
use jiff::civil::{Date, DateTime};
use serde::{Deserialize, Serialize};

/// The parameter a range's first value is bound to.
pub const PART_FROM: &str = "part_from";
/// The parameter a range's end is bound to: the first value past the range.
pub const PART_TO: &str = "part_to";

/// The most ranges a partition makes. A year by day is 366 and ten years by day 3,653; past
/// this many, `every` is almost surely a slip (`1 minute` over a year makes 525,600 statements),
/// and each range costs a statement and a line of the run's history.
pub const MAX_RANGES: u64 = 10_000;

/// A partition as the file writes it: `{ from = ":since", to = ":until", every = "1 month" }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub from: RangeBound,
    pub to: RangeBound,
    pub every: Every,
}

/// Where a partition starts or ends: one of the report's parameters, or a value of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeBound {
    /// The parameter's name, without its colon.
    Param(String),
    Date(Date),
    Timestamp(DateTime),
    Integer(i64),
}

/// How wide each range is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    /// So many units of the calendar or the clock, for dates and timestamps.
    Calendar {
        count: NonZeroU32,
        unit: CalendarUnit,
    },
    /// So many values of an integer key.
    Key(NonZeroU64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarUnit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
}

impl CalendarUnit {
    const ALL: [Self; 6] = [
        Self::Year,
        Self::Month,
        Self::Week,
        Self::Day,
        Self::Hour,
        Self::Minute,
    ];

    fn word(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
        }
    }

    /// Whether a range of dates can be this wide: a day or more.
    fn fits_dates(self) -> bool {
        !matches!(self, Self::Hour | Self::Minute)
    }

    fn span(self, count: i64) -> Result<Span, jiff::Error> {
        match self {
            Self::Year => Span::new().try_years(count),
            Self::Month => Span::new().try_months(count),
            Self::Week => Span::new().try_weeks(count),
            Self::Day => Span::new().try_days(count),
            Self::Hour => Span::new().try_hours(count),
            Self::Minute => Span::new().try_minutes(count),
        }
    }
}

impl Every {
    /// `every` as the file writes it for time: `"1 month"`, `"3 months"`, `"6 hours"`.
    ///
    /// # Errors
    ///
    /// Why it isn't one, as a sentence.
    pub fn parse(text: &str) -> Result<Self, String> {
        let problem = || {
            format!(
                "every {text:?} isn't a width of range: write a number and a unit, as \"1 month\", \
                 \"7 days\" or \"6 hours\" (years, months, weeks, days, hours or minutes), or an \
                 integer for ranges of a key"
            )
        };
        let mut words = text.split_whitespace();
        let (Some(count), Some(unit), None) = (words.next(), words.next(), words.next()) else {
            return Err(problem());
        };
        let count = count.parse::<NonZeroU32>().map_err(|_| problem())?;
        let unit = unit.to_ascii_lowercase();
        let unit = CalendarUnit::ALL
            .into_iter()
            .find(|candidate| unit == candidate.word() || unit == format!("{}s", candidate.word()))
            .ok_or_else(problem)?;
        Ok(Self::Calendar { count, unit })
    }

    /// The word for one range, for a run's progress: `month` for `1 month`, `range` for anything
    /// that isn't one unit of the calendar.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Calendar { count, unit } if count.get() == 1 => unit.word(),
            Self::Calendar { .. } | Self::Key(_) => "range",
        }
    }
}

impl fmt::Display for Every {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Calendar { count, unit } if count.get() == 1 => write!(f, "1 {}", unit.word()),
            Self::Calendar { count, unit } => write!(f, "{count} {}s", unit.word()),
            Self::Key(width) => write!(f, "{width}"),
        }
    }
}

/// What a partition's ranges are of: the type of its bounds and of `:part_from` and
/// `:part_to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeKind {
    Date,
    Timestamp,
    Integer,
}

impl RangeKind {
    pub fn column_type(self) -> ColumnType {
        match self {
            Self::Date => ColumnType::Date,
            Self::Timestamp => ColumnType::Timestamp,
            Self::Integer => ColumnType::Integer,
        }
    }

    fn of(column_type: ColumnType) -> Option<Self> {
        match column_type {
            ColumnType::Date => Some(Self::Date),
            ColumnType::Timestamp => Some(Self::Timestamp),
            ColumnType::Integer => Some(Self::Integer),
            ColumnType::Text | ColumnType::Decimal(_) | ColumnType::Boolean => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Date => "dates",
            Self::Timestamp => "timestamps",
            Self::Integer => "integers",
        }
    }

    /// A range's bound, in its plain form, as a literal of a language without bound
    /// parameters.
    pub fn literal(self, plain: &str) -> Literal {
        match self {
            Self::Date => Literal::Date(plain.to_owned()),
            Self::Timestamp => Literal::Timestamp(plain.to_owned()),
            Self::Integer => Literal::Number(plain.to_owned()),
        }
    }
}

/// A partition checked against the report's parameters, so its ranges' kind is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPartition {
    pub partition: Partition,
    pub kind: RangeKind,
}

/// One range of a partition: its number, counted from 1, and its bounds in their plain form
/// (`2026-07-01`), the first value in it and the first past it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRange {
    pub number: u64,
    pub from: String,
    pub to: String,
}

impl PlannedPartition {
    /// Checks `partition` against the report's parameters, by name and type.
    ///
    /// # Errors
    ///
    /// The first problem, as a sentence that follows `partition: `.
    pub fn plan(partition: Partition, params: &BTreeMap<&str, ColumnType>) -> Result<Self, String> {
        let from = bound_kind(&partition.from, "from", params)?;
        let to = bound_kind(&partition.to, "to", params)?;
        if from != to {
            return Err(format!(
                "from is of {} and to of {}; both bounds are dates, timestamps or integers alike",
                from.name(),
                to.name()
            ));
        }
        match (from, partition.every) {
            (RangeKind::Integer, Every::Key(_))
            | (RangeKind::Timestamp, Every::Calendar { .. }) => {}
            (RangeKind::Date, Every::Calendar { unit, .. }) if unit.fits_dates() => {}
            (RangeKind::Date, Every::Calendar { unit, .. }) => {
                return Err(format!(
                    "ranges of dates are a day or more wide, not {}s; make from and to \
                     timestamps for ranges within a day",
                    unit.word()
                ));
            }
            (RangeKind::Integer, Every::Calendar { .. }) => {
                return Err(
                    "ranges of an integer key are so many values wide: every is an integer, as \
                     every = 1000000"
                        .to_owned(),
                );
            }
            (RangeKind::Date | RangeKind::Timestamp, Every::Key(_)) => {
                return Err(format!(
                    "ranges of {} are a span of time wide, as every = \"1 month\"",
                    from.name()
                ));
            }
        }
        Ok(Self {
            partition,
            kind: from,
        })
    }

    /// The parameters whose values the bounds read.
    pub fn params(&self) -> impl Iterator<Item = &str> {
        [&self.partition.from, &self.partition.to]
            .into_iter()
            .filter_map(|bound| match bound {
                RangeBound::Param(name) => Some(name.as_str()),
                RangeBound::Date(_) | RangeBound::Timestamp(_) | RangeBound::Integer(_) => None,
            })
    }

    /// The whole period, `from` to `to`, as one range numbered 0: what every range reads
    /// together, which the cost guard judges.
    ///
    /// # Errors
    ///
    /// As [`PlannedPartition::ranges`].
    pub fn whole(&self, values: &BTreeMap<&str, (ColumnType, &str)>) -> Result<PartRange, String> {
        let from = self.value(&self.partition.from, values)?;
        let to = self.value(&self.partition.to, values)?;
        Ok(PartRange {
            number: 0,
            from: from.plain(),
            to: to.plain(),
        })
    }

    /// The ranges of a run with `values`, the report's parameters in their plain form.
    ///
    /// # Errors
    ///
    /// Why there are none, or too many, as a sentence that follows `partition: `.
    pub fn ranges(
        &self,
        values: &BTreeMap<&str, (ColumnType, &str)>,
    ) -> Result<Vec<PartRange>, String> {
        let from = self.value(&self.partition.from, values)?;
        let to = self.value(&self.partition.to, values)?;
        if from >= to {
            return Err(format!(
                "from ({}) isn't before to ({}), so there is no range to read",
                from.plain(),
                to.plain()
            ));
        }
        let mut ranges = Vec::new();
        let mut start = from;
        for number in 1.. {
            let next = from.step(self.partition.every, number)?;
            let end = next.min(to);
            ranges.push(PartRange {
                number: number.unsigned_abs(),
                from: start.plain(),
                to: end.plain(),
            });
            if next >= to {
                break;
            }
            if number.unsigned_abs() >= MAX_RANGES {
                return Err(format!(
                    "every = \"{}\" makes more than {MAX_RANGES} ranges from {} to {}; widen \
                     every",
                    self.partition.every,
                    from.plain(),
                    to.plain()
                ));
            }
            start = next;
        }
        Ok(ranges)
    }

    /// `bound`'s value for the run.
    fn value(
        &self,
        bound: &RangeBound,
        values: &BTreeMap<&str, (ColumnType, &str)>,
    ) -> Result<Point, String> {
        match bound {
            RangeBound::Date(date) => Ok(Point::Date(*date)),
            RangeBound::Timestamp(at) => Ok(Point::Timestamp(*at)),
            RangeBound::Integer(key) => Ok(Point::Integer(*key)),
            RangeBound::Param(name) => {
                let Some((_, text)) = values.get(name.as_str()) else {
                    return Err(format!("there is no value for :{name}"));
                };
                Point::parse(self.kind, text).map_err(|reason| format!(":{name} {reason}"))
            }
        }
    }
}

/// The kind of value `bound` is, checking a parameter's type.
fn bound_kind(
    bound: &RangeBound,
    key: &str,
    params: &BTreeMap<&str, ColumnType>,
) -> Result<RangeKind, String> {
    match bound {
        RangeBound::Date(_) => Ok(RangeKind::Date),
        RangeBound::Timestamp(_) => Ok(RangeKind::Timestamp),
        RangeBound::Integer(_) => Ok(RangeKind::Integer),
        RangeBound::Param(name) => {
            let Some(&param_type) = params.get(name.as_str()) else {
                return Err(format!(
                    "{key} names :{name}, which isn't one of [report] params"
                ));
            };
            RangeKind::of(param_type).ok_or_else(|| {
                format!(
                    "{key} names :{name}, a {param_type} parameter; ranges are of dates, \
                     timestamps or integers"
                )
            })
        }
    }
}

/// A bound's value, of the partition's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Point {
    Date(Date),
    Timestamp(DateTime),
    Integer(i64),
}

impl Point {
    fn parse(kind: RangeKind, text: &str) -> Result<Self, String> {
        match kind {
            RangeKind::Date => text
                .parse()
                .map(Self::Date)
                .map_err(|_| "isn't a date".to_owned()),
            RangeKind::Timestamp => text
                .parse()
                .map(Self::Timestamp)
                .map_err(|_| "isn't a timestamp".to_owned()),
            RangeKind::Integer => text
                .parse()
                .map(Self::Integer)
                .map_err(|_| "isn't an integer".to_owned()),
        }
    }

    /// The bound `number` widths of `every` past this one, counted from it rather than from
    /// the bound before, so months that start on the 31st keep their day where a month has
    /// one.
    fn step(&self, every: Every, number: i64) -> Result<Self, String> {
        let too_far = || "the ranges reach past the last date DbRover can count".to_owned();
        match (self, every) {
            (Self::Integer(from), Every::Key(width)) => i64::try_from(width.get())
                .ok()
                .and_then(|width| width.checked_mul(number))
                .and_then(|offset| from.checked_add(offset))
                .map(Self::Integer)
                .ok_or_else(|| "the ranges reach past the largest integer".to_owned()),
            (Self::Date(from), Every::Calendar { count, unit }) => unit
                .span(i64::from(count.get()) * number)
                .and_then(|span| from.checked_add(span))
                .map(Self::Date)
                .map_err(|_| too_far()),
            (Self::Timestamp(from), Every::Calendar { count, unit }) => unit
                .span(i64::from(count.get()) * number)
                .and_then(|span| from.checked_add(span))
                .map(Self::Timestamp)
                .map_err(|_| too_far()),
            // Planning matched every's kind to the bounds'.
            _ => Err(format!(
                "every = \"{every}\" doesn't fit the partition's bounds"
            )),
        }
    }

    /// As its type's plain form writes it: `2026-07-01`, `2026-07-01 06:00:00`, `1000`.
    fn plain(&self) -> String {
        match self {
            Self::Date(date) => date.to_string(),
            Self::Timestamp(at) => at.strftime("%Y-%m-%d %H:%M:%S").to_string(),
            Self::Integer(key) => key.to_string(),
        }
    }
}

/// `partition = { from = ":since", to = ":until", every = "1 month" }`, as the file writes it.
///
/// # Errors
///
/// Every problem it has, each a sentence that starts with `partition`.
pub(crate) fn read_partition(value: toml::Value) -> Result<Partition, Vec<String>> {
    let toml::Value::Table(mut table) = value else {
        return Err(vec![
            "partition is a table, as partition = { from = \":since\", to = \":until\", every = \
             \"1 month\" }"
                .to_owned(),
        ]);
    };
    let from = read_bound(table.remove("from"), "from");
    let to = read_bound(table.remove("to"), "to");
    let every = read_every(table.remove("every"));
    let mut problems: Vec<String> = table
        .keys()
        .map(|key| {
            format!("partition: {key:?} is not part of a partition, which has from, to and every")
        })
        .collect();
    match (from, to, every) {
        (Ok(from), Ok(to), Ok(every)) if problems.is_empty() => Ok(Partition { from, to, every }),
        (from, to, every) => {
            problems.extend(from.err());
            problems.extend(to.err());
            problems.extend(every.err());
            Err(problems)
        }
    }
}

/// `from` or `to`: `":since"`, a date, a local date and time, or an integer.
fn read_bound(value: Option<toml::Value>, key: &str) -> Result<RangeBound, String> {
    let problem = || {
        format!(
            "partition: {key} is a parameter, as {key} = \":since\", or a date, a timestamp or \
             an integer"
        )
    };
    match value {
        None => Err(format!(
            "partition needs {key}, a parameter as {key} = \":since\", or a value"
        )),
        Some(toml::Value::String(text)) => match text.strip_prefix(':') {
            Some(name) if !name.is_empty() => Ok(RangeBound::Param(name.to_owned())),
            _ => text
                .parse::<Date>()
                .map(RangeBound::Date)
                .or_else(|_| text.parse::<DateTime>().map(RangeBound::Timestamp))
                .map_err(|_| problem()),
        },
        Some(toml::Value::Integer(key)) => Ok(RangeBound::Integer(key)),
        Some(toml::Value::Datetime(at)) if at.offset.is_none() && at.date.is_some() => {
            let text = at.to_string();
            match at.time {
                None => text.parse().map(RangeBound::Date).map_err(|_| problem()),
                Some(_) => text
                    .parse()
                    .map(RangeBound::Timestamp)
                    .map_err(|_| problem()),
            }
        }
        Some(toml::Value::Datetime(at)) if at.offset.is_some() => Err(format!(
            "partition: {key} has a time zone offset; ranges are of local times, as a timestamp \
             parameter's values are"
        )),
        Some(_) => Err(problem()),
    }
}

/// `every`: `"1 month"` for time, a positive integer for a key.
fn read_every(value: Option<toml::Value>) -> Result<Every, String> {
    match value {
        None => {
            Err("partition needs every, how wide each range is, as every = \"1 month\"".to_owned())
        }
        Some(toml::Value::String(text)) => {
            Every::parse(&text).map_err(|problem| format!("partition: {problem}"))
        }
        Some(toml::Value::Integer(width)) => u64::try_from(width)
            .ok()
            .and_then(NonZeroU64::new)
            .map(Every::Key)
            .ok_or_else(|| "partition: every must be greater than zero".to_owned()),
        Some(other) => Err(format!(
            "partition: every is a span, as every = \"1 month\", or an integer, not a {}",
            other.type_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every(text: &str) -> Every {
        Every::parse(text).unwrap()
    }

    fn planned(from: RangeBound, to: RangeBound, every: Every) -> PlannedPartition {
        let params: BTreeMap<&str, ColumnType> = [
            ("since", ColumnType::Date),
            ("until", ColumnType::Date),
            ("at", ColumnType::Timestamp),
            ("first", ColumnType::Integer),
            ("name", ColumnType::Text),
        ]
        .into_iter()
        .collect();
        PlannedPartition::plan(Partition { from, to, every }, &params).unwrap()
    }

    fn param(name: &str) -> RangeBound {
        RangeBound::Param(name.to_owned())
    }

    fn bounds(ranges: &[PartRange]) -> Vec<(&str, &str)> {
        ranges
            .iter()
            .map(|range| (range.from.as_str(), range.to.as_str()))
            .collect()
    }

    #[test]
    fn reads_every_as_a_count_and_a_unit() {
        let cases = [
            ("1 month", Some(("1 month", "month"))),
            ("3 months", Some(("3 months", "range"))),
            ("1 Day", Some(("1 day", "day"))),
            ("2 weeks", Some(("2 weeks", "range"))),
            ("6 hours", Some(("6 hours", "range"))),
            ("1 year", Some(("1 year", "year"))),
            ("month", None),
            ("0 days", None),
            ("1 fortnight", None),
            ("1 month 2 days", None),
            ("-1 day", None),
        ];
        for (text, read) in cases {
            let parsed = Every::parse(text);
            assert_eq!(
                parsed
                    .as_ref()
                    .ok()
                    .map(|every| (every.to_string(), every.unit())),
                read.map(|(shown, unit)| (shown.to_owned(), unit)),
                "{text}: {parsed:?}"
            );
        }
    }

    #[test]
    fn refuses_bounds_and_widths_that_dont_fit_together() {
        let params: BTreeMap<&str, ColumnType> = [
            ("since", ColumnType::Date),
            ("at", ColumnType::Timestamp),
            ("name", ColumnType::Text),
        ]
        .into_iter()
        .collect();
        let date = || RangeBound::Date(Date::constant(2026, 1, 1));
        let cases = [
            (
                param("missing"),
                date(),
                every("1 day"),
                "from names :missing",
            ),
            (param("name"), date(), every("1 day"), "a text parameter"),
            (
                param("since"),
                param("at"),
                every("1 day"),
                "from is of dates and to of timestamps",
            ),
            (
                param("since"),
                date(),
                every("6 hours"),
                "ranges of dates are a day or more",
            ),
            (
                RangeBound::Integer(0),
                RangeBound::Integer(10),
                every("1 day"),
                "so many values wide",
            ),
            (
                param("since"),
                date(),
                Every::Key(NonZeroU64::MIN),
                "a span of time wide",
            ),
        ];
        for (from, to, every, problem) in cases {
            let planned = PlannedPartition::plan(Partition { from, to, every }, &params);
            assert!(
                planned.as_ref().is_err_and(|error| error.contains(problem)),
                "{problem}: {planned:?}"
            );
        }
    }

    #[test]
    fn makes_ranges_from_the_bounds_and_values() {
        let values: BTreeMap<&str, (ColumnType, &str)> = [
            ("since", (ColumnType::Date, "2024-01-31")),
            ("until", (ColumnType::Date, "2024-05-15")),
            ("at", (ColumnType::Timestamp, "2026-03-01 00:00:00")),
            ("first", (ColumnType::Integer, "0")),
        ]
        .into_iter()
        .collect();
        let cases: [(PlannedPartition, &[(&str, &str)]); 4] = [
            // From the 31st: each month keeps its day where it has one, and the last range is cut
            // at `to`. 2024 is a leap year.
            (
                planned(param("since"), param("until"), every("1 month")),
                &[
                    ("2024-01-31", "2024-02-29"),
                    ("2024-02-29", "2024-03-31"),
                    ("2024-03-31", "2024-04-30"),
                    ("2024-04-30", "2024-05-15"),
                ],
            ),
            (
                planned(
                    param("at"),
                    RangeBound::Timestamp(DateTime::constant(2026, 3, 1, 15, 0, 0, 0)),
                    every("6 hours"),
                ),
                &[
                    ("2026-03-01 00:00:00", "2026-03-01 06:00:00"),
                    ("2026-03-01 06:00:00", "2026-03-01 12:00:00"),
                    ("2026-03-01 12:00:00", "2026-03-01 15:00:00"),
                ],
            ),
            (
                planned(
                    param("first"),
                    RangeBound::Integer(25),
                    Every::Key(NonZeroU64::new(10).unwrap()),
                ),
                &[("0", "10"), ("10", "20"), ("20", "25")],
            ),
            (
                planned(
                    RangeBound::Date(Date::constant(2025, 1, 1)),
                    RangeBound::Date(Date::constant(2026, 1, 1)),
                    every("1 year"),
                ),
                &[("2025-01-01", "2026-01-01")],
            ),
        ];
        for (partition, expected) in cases {
            let ranges = partition.ranges(&values).unwrap();
            assert_eq!(bounds(&ranges), expected, "{partition:?}");
            let numbers: Vec<u64> = ranges.iter().map(|range| range.number).collect();
            let counted: Vec<u64> = (1..=expected.len() as u64).collect();
            assert_eq!(numbers, counted);
            let whole = partition.whole(&values).unwrap();
            assert_eq!(
                (whole.from.as_str(), whole.to.as_str()),
                (expected[0].0, expected[expected.len() - 1].1)
            );
        }
    }

    #[test]
    fn says_why_there_are_no_ranges_or_too_many() {
        let values =
            |since: &'static str, until: &'static str| -> BTreeMap<&str, (ColumnType, &str)> {
                [
                    ("since", (ColumnType::Date, since)),
                    ("until", (ColumnType::Date, until)),
                ]
                .into_iter()
                .collect()
            };
        let by_day = planned(param("since"), param("until"), every("1 day"));
        let cases = [
            (values("2026-02-01", "2026-02-01"), "isn't before to"),
            (values("2026-03-01", "2026-02-01"), "isn't before to"),
            (values("2000-01-01", "2030-01-01"), "more than 10000 ranges"),
            (values("2026-02-30", "2026-03-01"), ":since isn't a date"),
        ];
        for (values, problem) in cases {
            let ranges = by_day.ranges(&values);
            assert!(
                ranges.as_ref().is_err_and(|error| error.contains(problem)),
                "{problem}: {ranges:?}"
            );
        }
        let ten_thousand = values("2000-01-01", "2027-05-19");
        assert_eq!(
            by_day.ranges(&ten_thousand).map(|ranges| ranges.len()),
            Ok(10_000)
        );
    }
}
