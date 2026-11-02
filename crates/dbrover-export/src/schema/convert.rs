//! Each declared type's check of a value, and the plain form it rewrites the value in.

use jiff::civil::{Date, DateTime};
use jiff::{Timestamp, Unit};

use super::decimal::Decimal;
use super::{ColumnType, DecimalSpec, SchemaColumn};

/// Excel's numbers are IEEE doubles, exact to 15 significant decimal digits.
const EXCEL_DIGITS: usize = 15;

/// Where a converted value is going, which decides how finely a time is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTarget {
    /// A text format, which writes every digit it is given.
    Text,
    /// A spreadsheet, whose times hold milliseconds: finer fractions are rounded, so the
    /// value stays a date rather than turning into text.
    Excel,
}

/// `text` as `column_type` writes it, or why it doesn't fit, as the end of a sentence that
/// starts with the value.
pub(super) fn value(
    column_type: ColumnType,
    column: &SchemaColumn,
    target: CellTarget,
    text: &str,
) -> Result<String, String> {
    match column_type {
        ColumnType::Text => Ok(text.to_owned()),
        ColumnType::Integer => integer(text),
        ColumnType::Decimal(spec) => decimal(text, spec),
        ColumnType::Boolean => boolean(text),
        ColumnType::Date => date(text),
        ColumnType::Timestamp => timestamp(text, column.time_zone.as_ref(), target),
    }
}

/// Whether the Excel writer will write a number in this plain form as text: more digits
/// than its numbers hold, counted as [`Decimal::significant_digits`] does.
pub(super) fn beyond_excel(written: &str) -> bool {
    Decimal::parse(written).is_some_and(|number| number.significant_digits() > EXCEL_DIGITS)
}

fn integer(text: &str) -> Result<String, String> {
    let number = Decimal::parse(text)
        .filter(Decimal::is_whole)
        .ok_or("isn't a whole number")?;
    Ok(number.round(0).to_string())
}

fn decimal(text: &str, spec: Option<DecimalSpec>) -> Result<String, String> {
    let number = Decimal::parse(text).ok_or("isn't a decimal number")?;
    let Some(spec) = spec else {
        return Ok(number.to_string());
    };
    let rounded = number.round(usize::from(spec.scale));
    let whole_digits = usize::from(spec.precision - spec.scale);
    let needed = rounded.whole_digits_needed();
    if needed > whole_digits {
        return Err(format!(
            "has {needed} digits before the point, more than the {whole_digits} \
             decimal({},{}) takes",
            spec.precision, spec.scale
        ));
    }
    Ok(rounded.to_string())
}

fn boolean(text: &str) -> Result<String, String> {
    match text.to_ascii_lowercase().as_str() {
        "t" | "true" | "y" | "yes" | "1" => Ok("true".to_owned()),
        "f" | "false" | "n" | "no" | "0" => Ok("false".to_owned()),
        _ => Err("isn't true or false".to_owned()),
    }
}

/// `2026-10-01`, or a date and time at midnight, as a `DATETIME` column holds dates.
fn date(text: &str) -> Result<String, String> {
    let not_a_date = || "isn't a date, as 2026-10-01".to_owned();
    let day = match text.parse::<Date>() {
        Ok(day) if text.len() == 10 => day,
        _ => {
            let at = civil_date_time(text).ok_or_else(not_a_date)?;
            if at.time() != jiff::civil::Time::midnight() {
                return Err("has a time of day, which a date column doesn't take".to_owned());
            }
            at.date()
        }
    };
    Ok(day.strftime("%Y-%m-%d").to_string())
}

fn timestamp(
    text: &str,
    zone: Option<&jiff::tz::TimeZone>,
    target: CellTarget,
) -> Result<String, String> {
    let at = match (civil_date_time(text), zone) {
        (Some(at), _) => at,
        (None, Some(zone)) => {
            let instant = with_offset(text).ok_or("isn't a date and time")?;
            instant.to_zoned(zone.clone()).datetime()
        }
        (None, None) if with_offset(text).is_some() => {
            return Err(
                "has a time zone offset; give the column a time_zone, such as \"UTC\", to \
                 convert it"
                    .to_owned(),
            );
        }
        (None, None) => return Err("isn't a date and time, as 2026-10-01 12:00:00".to_owned()),
    };
    let at = match target {
        CellTarget::Text => at,
        CellTarget::Excel => at
            .round(Unit::Millisecond)
            .map_err(|error| format!("can't be rounded to a millisecond: {error}"))?,
    };
    Ok(at.strftime("%Y-%m-%d %H:%M:%S%.f").to_string())
}

/// A date and time with no offset: `2026-10-01 12:00`, `2026-10-01T12:00:00.123456`, or a
/// date alone, at midnight.
fn civil_date_time(text: &str) -> Option<DateTime> {
    if has_offset(text) {
        return None;
    }
    text.parse::<DateTime>().ok()
}

/// An instant written with its offset, as `timestamptz` prints: `2026-10-01 12:00:00+02`,
/// `…Z`, `…+05:30`.
fn with_offset(text: &str) -> Option<Timestamp> {
    if !has_offset(text) {
        return None;
    }
    text.parse::<Timestamp>().ok()
}

/// Whether what follows the date names an offset (`Z`, `+02`, `-05:00`).
fn has_offset(text: &str) -> bool {
    text.get(10..)
        .is_some_and(|time| time.contains(['Z', 'z', '+', '-']))
}
