//! Axes of time: ticks at a round unit for the span shown, and labels written to that unit.
//!
//! Times are positions in seconds since 1970 of the date and time as seen in one offset
//! ([`crate::data::seconds`]), so ticks are computed on civil dates and times: a tick a month
//! after another falls on the same day of the next month, whatever its length.

use jiff::civil::DateTime;
use jiff::tz::Offset;
use jiff::{Span, Timestamp};

use crate::data::seconds;
use crate::ticks::nice_step;

const SECONDS_PER_DAY: f64 = 86_400.0;
/// A month and a year on average, for picking a step; ticks themselves step by calendar.
const SECONDS_PER_MONTH: f64 = SECONDS_PER_DAY * 30.44;
const SECONDS_PER_YEAR: f64 = SECONDS_PER_DAY * 365.25;

/// The most ticks an axis gets, as for numbers.
const MAX_TICKS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl Unit {
    fn seconds(self) -> f64 {
        match self {
            Self::Second => 1.0,
            Self::Minute => 60.0,
            Self::Hour => 3_600.0,
            Self::Day => SECONDS_PER_DAY,
            Self::Month => SECONDS_PER_MONTH,
            Self::Year => SECONDS_PER_YEAR,
        }
    }
}

/// Steps a time axis can tick at, finest first: each one divides the unit above it, so ticks
/// fall on round times (every 15 minutes from the hour, every 3 months from January).
const STEPS: [(Unit, i64); 18] = [
    (Unit::Second, 1),
    (Unit::Second, 5),
    (Unit::Second, 15),
    (Unit::Second, 30),
    (Unit::Minute, 1),
    (Unit::Minute, 5),
    (Unit::Minute, 15),
    (Unit::Minute, 30),
    (Unit::Hour, 1),
    (Unit::Hour, 3),
    (Unit::Hour, 6),
    (Unit::Hour, 12),
    (Unit::Day, 1),
    (Unit::Day, 7),
    (Unit::Month, 1),
    (Unit::Month, 3),
    (Unit::Month, 6),
    (Unit::Year, 1),
];

/// The ticks of a time axis, and how to label them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimeTicks {
    pub(crate) values: Vec<f64>,
    unit: Unit,
    /// The ticks span more than one year, so days are labelled with theirs.
    years_differ: bool,
}

impl TimeTicks {
    /// About `target` ticks from `low` to `high`, at the finest step that isn't more. With
    /// `dates_only`, no step is finer than a day.
    pub(crate) fn new(low: f64, high: f64, target: usize, dates_only: bool) -> Self {
        let (unit, count) = step(high - low, target, dates_only);
        let values: Vec<f64> = date_time(low)
            .and_then(|start| floor(start, unit, count))
            .map(|start| ticks_from(start, unit, count, low, high))
            .unwrap_or_default();
        let year = |value: &f64| date_time(*value).map(|time| time.year());
        let years_differ = values.first().map(year) != values.last().map(year);
        Self {
            values,
            unit,
            years_differ,
        }
    }

    /// A tick's label, written to the step's unit: `2026`, `Mar 2026`, `14 Mar`, `09:30`.
    /// A tick at midnight on an axis of hours names its day instead.
    pub(crate) fn label(&self, value: f64) -> String {
        let Some(time) = date_time(value) else {
            return String::new();
        };
        let midnight = time.time() == jiff::civil::Time::midnight();
        let format = match self.unit {
            Unit::Year => "%Y",
            Unit::Month => "%b %Y",
            Unit::Day if self.years_differ => "%-d %b %Y",
            Unit::Day => "%-d %b",
            Unit::Hour | Unit::Minute if midnight => "%-d %b",
            Unit::Hour | Unit::Minute => "%H:%M",
            Unit::Second => "%H:%M:%S",
        };
        time.strftime(format).to_string()
    }
}

/// The step for `span` seconds in about `target` ticks. Spans of centuries step by a round
/// number of years.
fn step(span: f64, target: usize, dates_only: bool) -> (Unit, i64) {
    let target = target.max(1) as f64;
    let fits = |&&(unit, count): &&(Unit, i64)| span / (unit.seconds() * count as f64) <= target;
    let allowed = |&&(unit, _): &&(Unit, i64)| !dates_only || unit >= Unit::Day;
    if let Some(&found) = STEPS.iter().filter(allowed).find(fits) {
        return found;
    }
    let years = nice_step(span / SECONDS_PER_YEAR / target).max(1.0);
    (Unit::Year, years as i64)
}

/// `time` truncated to its unit, and back to a multiple of `count` within the unit above.
/// `None` when that falls before the first year jiff knows.
fn floor(time: DateTime, unit: Unit, count: i64) -> Option<DateTime> {
    let down = |value: i8| {
        let value = i64::from(value);
        i8::try_from(value - value.rem_euclid(count)).ok()
    };
    let (year, month, day) = (time.year(), time.month(), time.day());
    let (hour, minute, second) = (time.hour(), time.minute(), time.second());
    let parts = match unit {
        Unit::Year => {
            let year = i64::from(year);
            (
                i16::try_from(year - year.rem_euclid(count)).ok()?,
                1,
                1,
                0,
                0,
                0,
            )
        }
        // Months count from 1, so they are rounded down from January.
        Unit::Month => (year, down(month - 1)? + 1, 1, 0, 0, 0),
        Unit::Day => (year, month, day, 0, 0, 0),
        Unit::Hour => (year, month, day, down(hour)?, 0, 0),
        Unit::Minute => (year, month, day, hour, down(minute)?, 0),
        Unit::Second => (year, month, day, hour, minute, down(second)?),
    };
    let (year, month, day, hour, minute, second) = parts;
    DateTime::new(year, month, day, hour, minute, second, 0).ok()
}

/// Ticks every `count` `unit`s from `start`, those from `low` to `high`.
fn ticks_from(start: DateTime, unit: Unit, count: i64, low: f64, high: f64) -> Vec<f64> {
    let step = match unit {
        Unit::Year => Span::new().years(count),
        Unit::Month => Span::new().months(count),
        Unit::Day => Span::new().days(count),
        Unit::Hour => Span::new().hours(count),
        Unit::Minute => Span::new().minutes(count),
        Unit::Second => Span::new().seconds(count),
    };
    std::iter::successors(Some(start), |time| time.checked_add(step).ok())
        .map(seconds)
        .skip_while(|&value| value < low)
        .take_while(|&value| value <= high)
        .take(MAX_TICKS)
        .collect()
}

/// The date and time at `value` seconds on the axis.
fn date_time(value: f64) -> Option<DateTime> {
    let whole = value.floor();
    let nanoseconds = ((value - whole) * 1e9).round() as i32;
    let instant = Timestamp::new(whole as i64, nanoseconds.min(999_999_999)).ok()?;
    Some(Offset::UTC.to_datetime(instant))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn at(text: &str) -> f64 {
        seconds(text.parse().unwrap())
    }

    fn labels(ticks: &TimeTicks) -> Vec<String> {
        ticks
            .values
            .iter()
            .map(|value| ticks.label(*value))
            .collect()
    }

    #[test]
    fn a_year_of_months_ticks_by_quarter() {
        let ticks = TimeTicks::new(at("2025-01-01"), at("2025-12-01"), 5, true);
        assert_eq!(
            labels(&ticks),
            ["Jan 2025", "Apr 2025", "Jul 2025", "Oct 2025"]
        );
    }

    #[test]
    fn a_dozen_months_with_room_ticks_every_month() {
        let ticks = TimeTicks::new(at("2025-01-01"), at("2025-12-01"), 12, true);
        assert_eq!(ticks.values.len(), 12);
        assert_eq!(ticks.label(ticks.values[1]), "Feb 2025");
    }

    #[test]
    fn days_carry_their_year_when_the_axis_crosses_one() {
        let within = TimeTicks::new(at("2025-03-01"), at("2025-03-05"), 5, true);
        assert_eq!(
            labels(&within),
            ["1 Mar", "2 Mar", "3 Mar", "4 Mar", "5 Mar"]
        );
        let across = TimeTicks::new(at("2025-12-30"), at("2026-01-02"), 5, true);
        assert_eq!(labels(&across)[0], "30 Dec 2025");
    }

    #[test]
    fn dates_alone_never_tick_finer_than_a_day() {
        let ticks = TimeTicks::new(at("2025-03-01"), at("2025-03-02"), 10, true);
        assert_eq!(labels(&ticks), ["1 Mar", "2 Mar"]);
    }

    #[test]
    fn hours_tick_from_round_times_and_name_midnight_by_its_day() {
        let ticks = TimeTicks::new(at("2025-03-01T19:10"), at("2025-03-02T04:00"), 4, false);
        assert_eq!(labels(&ticks), ["21:00", "2 Mar", "03:00"]);
    }

    #[test]
    fn centuries_tick_by_round_numbers_of_years() {
        let ticks = TimeTicks::new(at("1800-01-01"), at("2000-01-01"), 4, true);
        assert_eq!(labels(&ticks), ["1800", "1850", "1900", "1950", "2000"]);
    }

    #[test]
    fn floors_to_a_multiple_of_the_step() {
        let time = date(2025, 8, 17).at(14, 47, 33, 0);
        let cases = [
            (Unit::Month, 3, date(2025, 7, 1).at(0, 0, 0, 0)),
            (Unit::Hour, 6, date(2025, 8, 17).at(12, 0, 0, 0)),
            (Unit::Minute, 15, date(2025, 8, 17).at(14, 45, 0, 0)),
            (Unit::Second, 30, date(2025, 8, 17).at(14, 47, 30, 0)),
            (Unit::Year, 10, date(2020, 1, 1).at(0, 0, 0, 0)),
        ];
        for (unit, count, expected) in cases {
            assert_eq!(floor(time, unit, count), Some(expected), "{unit:?} {count}");
        }
    }
}
