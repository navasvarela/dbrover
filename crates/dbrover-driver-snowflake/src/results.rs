//! A result set of Snowflake's SQL API as DbRover's columns and rows.
//!
//! Every value comes as a JSON string "regardless of the Snowflake data type", `null` for
//! `NULL`, in a form of the API's own: a `DATE` as days since 1970, a `TIME` or a timestamp as
//! seconds since the epoch with nine decimals, a `TIMESTAMP_TZ` with its offset in minutes plus
//! 1,440. The console shows them as Snowflake's own console writes them.

use dbrover_core::{Cell, ColumnInfo, Row};
use jiff::Timestamp;
use jiff::tz::Offset;
use serde::Deserialize;
use serde_json::Value;

/// What a result set says about its columns and partitions.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    #[serde(default)]
    pub row_type: Vec<Column>,
    /// One entry for each partition; partition 0 comes with the result set itself.
    #[serde(default)]
    pub partition_info: Vec<Value>,
}

/// One column of `rowType`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Column {
    pub name: String,
    /// `fixed`, `real`, `text`, `date`, `timestamp_ntz`, … in either case: the reference and
    /// the guide spell them differently.
    #[serde(rename = "type")]
    pub kind: String,
    pub precision: Option<u32>,
    pub scale: Option<u32>,
}

impl Column {
    fn kind(&self) -> Kind {
        Kind::of(&self.kind)
    }

    /// The type as Snowflake writes it: `NUMBER(38,0)`, `VARCHAR`, `TIMESTAMP_NTZ(9)`.
    pub(crate) fn type_name(&self) -> String {
        let name = self.kind.to_uppercase();
        match (self.kind(), self.precision, self.scale) {
            (Kind::Fixed, Some(precision), scale) => {
                format!("NUMBER({precision},{})", scale.unwrap_or(0))
            }
            (Kind::Fixed, None, _) => "NUMBER".to_owned(),
            (Kind::Real, ..) => "FLOAT".to_owned(),
            (Kind::Text, ..) => "VARCHAR".to_owned(),
            (Kind::Time | Kind::Timestamp(_), _, Some(scale)) => format!("{name}({scale})"),
            _ => name,
        }
    }

    pub(crate) fn info(&self) -> ColumnInfo {
        ColumnInfo {
            name: self.name.clone(),
            type_name: Some(self.type_name()),
        }
    }

    /// The digits after the second a `TIME` or timestamp shows: its scale, nine where the
    /// result set doesn't say.
    fn fraction_digits(&self) -> usize {
        self.scale.map_or(9, |scale| scale.min(9) as usize)
    }
}

/// The kinds of value whose text the API writes differently from Snowflake's console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fixed,
    Real,
    Text,
    Date,
    Time,
    Timestamp(Zone),
    /// Everything shown as the API writes it: `BOOLEAN`, `BINARY` in hexadecimal, `VARIANT`,
    /// `OBJECT` and `ARRAY` as JSON, geospatial values.
    AsSent,
}

/// Where a timestamp's wall-clock time is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    /// `TIMESTAMP_NTZ`: no zone, so the seconds are the wall-clock time itself.
    None,
    /// `TIMESTAMP_LTZ`: an instant, which Snowflake shows in the session's time zone. The SQL
    /// API keeps no session to read one from, so DbRover shows it in UTC and says so.
    Utc,
    /// `TIMESTAMP_TZ`: an instant with the offset it was written with.
    Own,
}

impl Kind {
    fn of(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "fixed" | "number" => Self::Fixed,
            "real" | "float" => Self::Real,
            "text" | "varchar" => Self::Text,
            "date" => Self::Date,
            "time" => Self::Time,
            "timestamp_ntz" => Self::Timestamp(Zone::None),
            "timestamp_ltz" => Self::Timestamp(Zone::Utc),
            "timestamp_tz" => Self::Timestamp(Zone::Own),
            _ => Self::AsSent,
        }
    }
}

/// `data`'s rows as cells, each value as Snowflake's console writes it. A value DbRover can't
/// read in its column's form is shown as the API sent it rather than lost.
pub(crate) fn rows(columns: &[Column], data: &[Vec<Value>]) -> Vec<Row> {
    data.iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| cell(column, row.get(index).unwrap_or(&Value::Null)))
                .collect()
        })
        .collect()
}

fn cell(column: &Column, value: &Value) -> Cell {
    let text = match value {
        Value::Null => return None,
        Value::String(text) => text.as_str(),
        other => return Some(other.to_string()),
    };
    let shown = match column.kind() {
        Kind::Date => date(text),
        Kind::Time => time(text, column.fraction_digits()),
        Kind::Timestamp(zone) => timestamp(text, zone, column.fraction_digits()),
        Kind::Fixed | Kind::Real | Kind::Text | Kind::AsSent => None,
    };
    Some(shown.unwrap_or_else(|| text.to_owned()))
}

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// `seconds.fraction` (or whole seconds) as nanoseconds since the epoch, negative before it.
fn nanoseconds(text: &str) -> Option<i128> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (seconds, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds: i128 = seconds.parse().ok()?;
    let fraction: i128 = format!("{fraction:0<9}").parse().ok()?;
    let total = seconds * NANOS_PER_SECOND + fraction;
    Some(if negative { -total } else { total })
}

/// Days since 1970-01-01 as `2026-01-02`.
fn date(text: &str) -> Option<String> {
    let days: i64 = text.parse().ok()?;
    let midnight = Timestamp::from_second(days.checked_mul(86_400)?).ok()?;
    Some(
        midnight
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date()
            .to_string(),
    )
}

/// Seconds since midnight as `13:45:30.125`, with `digits` after the second.
fn time(text: &str, digits: usize) -> Option<String> {
    let nanos = nanoseconds(text)?;
    let instant = Timestamp::from_nanosecond(nanos).ok()?;
    let wall = instant.to_zoned(jiff::tz::TimeZone::UTC).time();
    Some(with_fraction(
        &wall.strftime("%H:%M:%S").to_string(),
        wall.subsec_nanosecond(),
        digits,
    ))
}

/// A timestamp's seconds (and, for `TIMESTAMP_TZ`, its offset) as
/// `2026-01-02 03:04:05.000 +01:00`.
fn timestamp(text: &str, zone: Zone, digits: usize) -> Option<String> {
    let (seconds, offset) = match (zone, text.split_once(' ')) {
        (Zone::Own, Some((seconds, offset))) => (seconds, Some(offset)),
        (Zone::Own, None) => return None,
        (Zone::None | Zone::Utc, _) => (text, None),
    };
    let instant = Timestamp::from_nanosecond(nanoseconds(seconds)?).ok()?;
    let offset = match offset {
        // The offset in minutes, plus 1,440 so that it is never negative.
        Some(offset) => {
            let minutes: i32 = offset.parse::<i32>().ok()? - 1440;
            Offset::from_seconds(minutes.checked_mul(60)?).ok()?
        }
        None => Offset::UTC,
    };
    let wall = instant.to_zoned(offset.to_time_zone()).datetime();
    let shown = with_fraction(
        &wall.strftime("%Y-%m-%d %H:%M:%S").to_string(),
        wall.subsec_nanosecond(),
        digits,
    );
    Some(match zone {
        Zone::None => shown,
        Zone::Utc => format!("{shown} Z"),
        Zone::Own => format!("{shown} {}", hours_and_minutes(offset)),
    })
}

/// `+01:00` or `-05:30`, as Snowflake writes a `TIMESTAMP_TZ`'s offset (jiff's own leaves out
/// whole minutes).
fn hours_and_minutes(offset: Offset) -> String {
    let seconds = offset.seconds();
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `whole` and the first `digits` of `nanoseconds`, after a point when there are any.
fn with_fraction(whole: &str, nanoseconds: i32, digits: usize) -> String {
    if digits == 0 {
        return whole.to_owned();
    }
    let fraction = format!("{nanoseconds:09}");
    format!("{whole}.{}", &fraction[..digits])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn column(kind: &str, precision: Option<u32>, scale: Option<u32>) -> Column {
        Column {
            name: kind.to_owned(),
            kind: kind.to_owned(),
            precision,
            scale,
        }
    }

    #[test]
    fn columns_are_named_as_snowflake_writes_them() {
        for (column, named) in [
            (column("fixed", Some(38), Some(0)), "NUMBER(38,0)"),
            (column("FIXED", Some(10), Some(2)), "NUMBER(10,2)"),
            (column("real", None, None), "FLOAT"),
            (column("text", None, None), "VARCHAR"),
            (
                column("timestamp_ntz", Some(0), Some(9)),
                "TIMESTAMP_NTZ(9)",
            ),
            (column("TIME", None, Some(3)), "TIME(3)"),
            (column("variant", None, None), "VARIANT"),
            (column("date", None, None), "DATE"),
        ] {
            assert_eq!(column.type_name(), named);
        }
    }

    #[test]
    fn values_read_as_snowflakes_console_shows_them() {
        let columns = [
            column("fixed", Some(38), Some(0)),
            column("fixed", Some(10), Some(2)),
            column("text", None, None),
            column("date", None, None),
            column("time", None, Some(3)),
            column("timestamp_ntz", None, Some(9)),
            column("timestamp_ltz", None, Some(3)),
            column("timestamp_tz", None, Some(0)),
            column("binary", None, None),
            column("boolean", None, None),
            column("variant", None, None),
        ];
        let data = vec![vec![
            json!("99999999999999999999999999999999999999"),
            json!("1.50"),
            json!(""),
            json!("18262"),
            json!("49530.125000000"),
            json!("1767323045.123456789"),
            json!("1767323045.000000000"),
            json!("1616173619.000000000 1500"),
            json!("DBF00D"),
            json!("true"),
            json!("{\n  \"a\": [1, 2]\n}"),
        ]];
        let shown: Vec<Option<String>> = rows(&columns, &data).remove(0);
        let text = |value: &str| Some(value.to_owned());
        assert_eq!(
            shown,
            [
                text("99999999999999999999999999999999999999"),
                text("1.50"),
                text(""),
                text("2020-01-01"),
                text("13:45:30.125"),
                text("2026-01-02 03:04:05.123456789"),
                text("2026-01-02 03:04:05.000 Z"),
                text("2021-03-19 18:06:59 +01:00"),
                text("DBF00D"),
                text("true"),
                text("{\n  \"a\": [1, 2]\n}"),
            ]
        );
    }

    #[test]
    fn null_stays_apart_from_empty_text_and_odd_values_stay_as_sent() {
        let columns = [column("text", None, None), column("date", None, None)];
        let data = vec![
            vec![Value::Null, json!("not a day")],
            vec![json!(""), Value::Null],
        ];
        assert_eq!(
            rows(&columns, &data),
            [
                vec![None, Some("not a day".to_owned())],
                vec![Some(String::new()), None],
            ]
        );
    }

    #[test]
    fn times_before_1970_and_west_of_greenwich() {
        assert_eq!(date("-1").as_deref(), Some("1969-12-31"));
        assert_eq!(
            timestamp("-1.500000000", Zone::None, 3).as_deref(),
            Some("1969-12-31 23:59:58.500")
        );
        // 1,440 − 300: five hours west.
        assert_eq!(
            timestamp("1616173619.000000000 1140", Zone::Own, 0).as_deref(),
            Some("2021-03-19 12:06:59 -05:00")
        );
    }
}
