//! Reading one value's text as a number or a time.
//!
//! Values arrive as the database prints them (ADR 0004). A chart only needs to place them, so
//! a number is any finite decimal, with an exponent or not, and a time is an ISO date or
//! timestamp, with or without an offset, as every engine prints them.

use jiff::civil::DateTime;
use jiff::fmt::temporal::Pieces;
use jiff::tz::Offset;

/// `text` as a number, if it is a finite decimal: `42`, `-0.5`, `1e5`.
pub(crate) fn number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    let starts_like_a_number = trimmed
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.'));
    // `parse` also reads `inf`, `NaN` and `infinity`, which are not values to place.
    if !starts_like_a_number {
        return None;
    }
    trimmed
        .parse()
        .ok()
        .filter(|number: &f64| number.is_finite())
}

/// `text` as a number when its column declares no type: as [`number`], except that a leading
/// zero (`007`, `00501`) makes it a code rather than a quantity, as the Excel export reads it.
pub(crate) fn undeclared_number(text: &str) -> Option<f64> {
    let unsigned = text.trim().trim_start_matches(['-', '+']);
    let leading_zero =
        unsigned.len() > 1 && unsigned.starts_with('0') && !unsigned.starts_with("0.");
    if leading_zero {
        return None;
    }
    number(text)
}

/// A date or a timestamp as written: the date and time it shows, and the offset it names, if
/// any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WrittenTime {
    pub(crate) date_time: DateTime,
    pub(crate) offset: Option<Offset>,
    /// It had a time of day, not only a date.
    pub(crate) has_time: bool,
}

impl WrittenTime {
    /// The date and time this names, as seen in `offset`. A time written without an offset is
    /// taken as it is.
    pub(crate) fn in_offset(self, offset: Offset) -> DateTime {
        match self.offset {
            Some(own) => own
                .to_timestamp(self.date_time)
                .map(|instant| offset.to_datetime(instant))
                .unwrap_or(self.date_time),
            None => self.date_time,
        }
    }
}

/// `text` as an ISO date or timestamp: `2026-03-14`, `2026-03-14 09:30:00`,
/// `2026-03-14T09:30:00.25+01`, and SQL Server's `2026-03-14 09:30:00.0000000 +01:00`, whose
/// offset follows a space.
pub(crate) fn time(text: &str) -> Option<WrittenTime> {
    let trimmed = text.trim();
    pieces(trimmed).or_else(|| pieces(&without_space_before_offset(trimmed)?))
}

fn pieces(text: &str) -> Option<WrittenTime> {
    let pieces = Pieces::parse(text).ok()?;
    let has_time = pieces.time().is_some();
    let date_time = pieces.date().to_datetime(pieces.time().unwrap_or_default());
    Some(WrittenTime {
        date_time,
        offset: pieces.offset().map(|offset| offset.to_numeric_offset()),
        has_time,
    })
}

/// SQL Server writes a `datetimeoffset` with a space before its offset, which ISO 8601 has no
/// room for.
fn without_space_before_offset(text: &str) -> Option<String> {
    let space = text.rfind(' ')?;
    let offset = &text[space + 1..];
    offset
        .starts_with(['+', '-'])
        .then(|| format!("{}{offset}", &text[..space]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_as_databases_print_them() {
        let cases = [
            ("42", Some(42.0)),
            ("-0.5", Some(-0.5)),
            ("1e5", Some(100_000.0)),
            ("1234.50", Some(1234.5)),
            (" 7 ", Some(7.0)),
            ("NaN", None),
            ("Infinity", None),
            ("-inf", None),
            ("", None),
            ("12 apples", None),
            ("$12", None),
        ];
        for (text, expected) in cases {
            assert_eq!(number(text), expected, "{text:?}");
        }
    }

    #[test]
    fn a_leading_zero_makes_an_undeclared_value_a_code() {
        let cases = [
            ("007", None),
            ("00501", None),
            ("-01", None),
            ("0", Some(0.0)),
            ("0.25", Some(0.25)),
            ("10", Some(10.0)),
        ];
        for (text, expected) in cases {
            assert_eq!(undeclared_number(text), expected, "{text:?}");
        }
    }

    #[test]
    fn reads_dates_and_timestamps_with_and_without_offsets() {
        let utc = |hour| Some(Offset::constant(hour));
        let cases = [
            ("2026-03-14", "2026-03-14T00:00:00", None, false),
            ("2026-03-14 09:30:00", "2026-03-14T09:30:00", None, true),
            (
                "2026-03-14T09:30:00.25",
                "2026-03-14T09:30:00.25",
                None,
                true,
            ),
            (
                "2026-03-14 09:30:00+01",
                "2026-03-14T09:30:00",
                utc(1),
                true,
            ),
            ("2026-03-14 09:30:00Z", "2026-03-14T09:30:00", utc(0), true),
            (
                "2026-03-14 09:30:00.0000000 +01:00",
                "2026-03-14T09:30:00",
                utc(1),
                true,
            ),
        ];
        for (text, date_time, offset, has_time) in cases {
            let read = time(text).unwrap_or_else(|| panic!("{text:?} should read as a time"));
            assert_eq!(read.date_time.to_string(), date_time, "{text:?}");
            assert_eq!(read.offset, offset, "{text:?}");
            assert_eq!(read.has_time, has_time, "{text:?}");
        }
    }

    #[test]
    fn does_not_read_other_text_as_a_time() {
        for text in ["2026-03", "2026-13-01", "March 2026", "42", "", "09:30"] {
            assert_eq!(time(text), None, "{text:?}");
        }
    }

    #[test]
    fn sees_a_time_with_an_offset_in_another_offset() {
        let written = time("2026-03-14 23:30:00+00").unwrap();
        assert_eq!(
            written.in_offset(Offset::constant(2)).to_string(),
            "2026-03-15T01:30:00"
        );
        let local = time("2026-03-14 23:30:00").unwrap();
        assert_eq!(
            local.in_offset(Offset::constant(2)).to_string(),
            "2026-03-14T23:30:00"
        );
    }
}
