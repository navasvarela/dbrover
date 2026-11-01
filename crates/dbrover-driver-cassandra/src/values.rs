//! CQL values as the text DbRover shows (ADR 0004), and CQL types by their names.
//!
//! Scalars are written as CQL's own literals read them back, without quotes: timestamps in UTC
//! to the millisecond, blobs as `0x…`, decimals and varints with every digit, durations as
//! `1h30m`. Lists, sets, maps, tuples, vectors and user-defined types are JSON, which the cell
//! viewer browses as a tree (N0b): JSON's numbers, booleans and `null` where the value is one,
//! and a string of its text otherwise. A map's keys become JSON strings, as `toJson()` writes
//! them.

use std::fmt::Write as _;

use scylla::cluster::metadata::{CollectionType, ColumnType, NativeType};
use scylla::value::{CqlDate, CqlDecimal, CqlDuration, CqlTime, CqlTimestamp, CqlValue};

const MILLIS_PER_DAY: i64 = 86_400_000;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// How many days before 1970-01-01 a `date` counts from: its days are unsigned, with the epoch
/// in the middle of their range (CQL's native protocol specification, section 6, `date`).
const DATE_EPOCH_OFFSET: i64 = 1 << 31;

/// A value as the grid shows it.
pub(crate) fn text(value: &CqlValue) -> String {
    match value {
        CqlValue::Ascii(text) | CqlValue::Text(text) => text.clone(),
        CqlValue::Boolean(value) => value.to_string(),
        CqlValue::Blob(bytes) => blob(bytes),
        CqlValue::Empty => String::new(),
        CqlValue::Counter(counter) => counter.0.to_string(),
        CqlValue::TinyInt(value) => value.to_string(),
        CqlValue::SmallInt(value) => value.to_string(),
        CqlValue::Int(value) => value.to_string(),
        CqlValue::BigInt(value) => value.to_string(),
        CqlValue::Varint(value) => signed_digits(value.as_signed_bytes_be_slice()),
        CqlValue::Decimal(value) => decimal(value),
        CqlValue::Float(value) => float(f64::from(*value)),
        CqlValue::Double(value) => float(*value),
        CqlValue::Date(value) => date(*value),
        CqlValue::Time(value) => time(*value),
        CqlValue::Timestamp(value) => timestamp(*value),
        CqlValue::Duration(value) => duration(*value),
        CqlValue::Inet(address) => address.to_string(),
        CqlValue::Uuid(uuid) => uuid.to_string(),
        CqlValue::Timeuuid(uuid) => uuid.to_string(),
        compound => {
            let mut json = String::new();
            write_json(&mut json, Some(compound));
            json
        }
    }
}

/// A CQL type's name as CQL writes it: `int`, `map<text, frozen<list<int>>>`, `address`.
pub(crate) fn type_name(column_type: &ColumnType<'_>) -> String {
    match column_type {
        ColumnType::Native(native) => native_name(native).to_owned(),
        ColumnType::Collection { frozen, typ } => {
            let name = match typ {
                CollectionType::List(element) => format!("list<{}>", type_name(element)),
                CollectionType::Set(element) => format!("set<{}>", type_name(element)),
                CollectionType::Map(key, value) => {
                    format!("map<{}, {}>", type_name(key), type_name(value))
                }
                _ => "collection".to_owned(),
            };
            frozen_if(*frozen, name)
        }
        ColumnType::Vector { typ, dimensions } => {
            format!("vector<{}, {dimensions}>", type_name(typ))
        }
        ColumnType::UserDefinedType { frozen, definition } => {
            frozen_if(*frozen, definition.name.to_string())
        }
        ColumnType::Tuple(elements) => {
            let names: Vec<String> = elements.iter().map(type_name).collect();
            format!("tuple<{}>", names.join(", "))
        }
        _ => "unknown".to_owned(),
    }
}

fn frozen_if(frozen: bool, name: String) -> String {
    if frozen {
        format!("frozen<{name}>")
    } else {
        name
    }
}

fn native_name(native: &NativeType) -> &'static str {
    match native {
        NativeType::Ascii => "ascii",
        NativeType::Boolean => "boolean",
        NativeType::Blob => "blob",
        NativeType::Counter => "counter",
        NativeType::Date => "date",
        NativeType::Decimal => "decimal",
        NativeType::Double => "double",
        NativeType::Duration => "duration",
        NativeType::Float => "float",
        NativeType::Int => "int",
        NativeType::BigInt => "bigint",
        NativeType::Text => "text",
        NativeType::Timestamp => "timestamp",
        NativeType::Inet => "inet",
        NativeType::SmallInt => "smallint",
        NativeType::TinyInt => "tinyint",
        NativeType::Time => "time",
        NativeType::Timeuuid => "timeuuid",
        NativeType::Uuid => "uuid",
        NativeType::Varint => "varint",
        _ => "unknown",
    }
}

/// Writes `value` as JSON, `None` as `null`.
fn write_json(out: &mut String, value: Option<&CqlValue>) {
    let Some(value) = value else {
        out.push_str("null");
        return;
    };
    match value {
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => {
            write_array(out, items.iter().map(Some));
        }
        CqlValue::Tuple(items) => write_array(out, items.iter().map(Option::as_ref)),
        CqlValue::Map(entries) => {
            write_object(
                out,
                entries.iter().map(|(key, value)| (text(key), Some(value))),
            );
        }
        CqlValue::UserDefinedType { fields, .. } => {
            write_object(
                out,
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), value.as_ref())),
            );
        }
        CqlValue::Boolean(_)
        | CqlValue::Counter(_)
        | CqlValue::TinyInt(_)
        | CqlValue::SmallInt(_)
        | CqlValue::Int(_)
        | CqlValue::BigInt(_)
        | CqlValue::Varint(_)
        | CqlValue::Decimal(_) => out.push_str(&text(value)),
        CqlValue::Float(number) if number.is_finite() => out.push_str(&text(value)),
        CqlValue::Double(number) if number.is_finite() => out.push_str(&text(value)),
        scalar => write_string(out, &text(scalar)),
    }
}

fn write_array<'a>(out: &mut String, items: impl Iterator<Item = Option<&'a CqlValue>>) {
    out.push('[');
    for (index, item) in items.enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_json(out, item);
    }
    out.push(']');
}

fn write_object<'a>(
    out: &mut String,
    entries: impl Iterator<Item = (String, Option<&'a CqlValue>)>,
) {
    out.push('{');
    for (index, (key, value)) in entries.enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_string(out, &key);
        out.push_str(": ");
        write_json(out, value);
    }
    out.push('}');
}

/// `text` as a JSON string, with the escapes JSON requires.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn blob(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(2 + 2 * bytes.len());
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// CQL's `NaN`, `Infinity` and `-Infinity`, and the shortest digits that read back as the same
/// number otherwise.
fn float(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        value.to_string()
    }
}

/// A decimal's unscaled digits, with its point `scale` digits from the right: `1250` with scale
/// 2 is `12.50`, so trailing zeros the value was written with stay. A negative scale adds zeros.
fn decimal(value: &CqlDecimal) -> String {
    let (bytes, scale) = value.as_signed_be_bytes_slice_and_exponent();
    let digits = signed_digits(bytes);
    let (sign, digits) = match digits.strip_prefix('-') {
        Some(magnitude) => ("-", magnitude),
        None => ("", digits.as_str()),
    };
    let scale = i64::from(scale);
    if scale <= 0 {
        let zeros = if digits == "0" { 0 } else { -scale };
        return format!(
            "{sign}{digits}{}",
            "0".repeat(zeros.unsigned_abs() as usize)
        );
    }
    let scale = scale.unsigned_abs() as usize;
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// A two's complement big-endian integer of any length, in decimal.
fn signed_digits(bytes: &[u8]) -> String {
    let negative = bytes.first().is_some_and(|first| first & 0x80 != 0);
    let mut magnitude: Vec<u8> = bytes.to_vec();
    if negative {
        negate(&mut magnitude);
    }
    let digits = unsigned_digits(magnitude);
    if negative {
        format!("-{digits}")
    } else {
        digits
    }
}

/// Two's complement negation in place: invert every bit, then add one.
fn negate(bytes: &mut [u8]) {
    let mut carry = true;
    for byte in bytes.iter_mut().rev() {
        let (sum, overflowed) = (!*byte).overflowing_add(u8::from(carry));
        *byte = sum;
        carry = overflowed;
    }
}

/// An unsigned big-endian integer in decimal, by long division by 10, most significant byte
/// first, until nothing is left.
fn unsigned_digits(mut bytes: Vec<u8>) -> String {
    let mut digits = Vec::new();
    while bytes.iter().any(|byte| *byte != 0) {
        let mut remainder: u16 = 0;
        for byte in &mut bytes {
            let current = (remainder << 8) | u16::from(*byte);
            *byte = u8::try_from(current / 10).unwrap_or(u8::MAX);
            remainder = current % 10;
        }
        digits.push(char::from(b'0' + u8::try_from(remainder).unwrap_or(0)));
    }
    if digits.is_empty() {
        return "0".to_owned();
    }
    digits.iter().rev().collect()
}

fn date(value: CqlDate) -> String {
    let (year, month, day) = civil_from_days(i64::from(value.0) - DATE_EPOCH_OFFSET);
    format!("{year:04}-{month:02}-{day:02}")
}

fn time(value: CqlTime) -> String {
    let nanos = value.0;
    let seconds = nanos / NANOS_PER_SECOND;
    format!(
        "{:02}:{:02}:{:02}.{:09}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        nanos % NANOS_PER_SECOND
    )
}

/// An instant in UTC, as `2026-11-01 09:30:00.000+0000`, which CQL reads back as the same
/// timestamp.
fn timestamp(value: CqlTimestamp) -> String {
    let millis = value.0;
    let (year, month, day) = civil_from_days(millis.div_euclid(MILLIS_PER_DAY));
    let of_day = millis.rem_euclid(MILLIS_PER_DAY);
    let seconds = of_day / 1000;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}+0000",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        of_day % 1000
    )
}

/// The year, month and day of the day `days` after 1970-01-01, in the proleptic Gregorian
/// calendar. Howard Hinnant's `civil_from_days`
/// (<https://howardhinnant.github.io/date_algorithms.html#civil_from_days>): the days are
/// counted from 0000-03-01, so that a leap day ends each year, in eras of 400 years.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A duration as CQL writes one, `1y2mo3d4h5m6s7ms8us9ns` with the parts that are zero left
/// out. Its months, days and nanoseconds all have the duration's sign.
fn duration(value: CqlDuration) -> String {
    let negative = value.months < 0 || value.days < 0 || value.nanoseconds < 0;
    let months = i64::from(value.months).unsigned_abs();
    let days = i64::from(value.days).unsigned_abs();
    let nanos = value.nanoseconds.unsigned_abs();
    let parts = [
        (months / 12, "y"),
        (months % 12, "mo"),
        (days, "d"),
        (nanos / 3_600_000_000_000, "h"),
        (nanos / 60_000_000_000 % 60, "m"),
        (nanos / 1_000_000_000 % 60, "s"),
        (nanos / 1_000_000 % 1000, "ms"),
        (nanos / 1000 % 1000, "us"),
        (nanos % 1000, "ns"),
    ];
    let mut text = String::from(if negative { "-" } else { "" });
    for (amount, unit) in parts {
        if amount > 0 {
            let _ = write!(text, "{amount}{unit}");
        }
    }
    if text.is_empty() || text == "-" {
        return "0s".to_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use scylla::value::{Counter, CqlTimeuuid, CqlVarint};

    use super::*;

    #[test]
    fn writes_scalars_as_cql_reads_them() {
        let cases = [
            (CqlValue::Text("it's".to_owned()), "it's"),
            (CqlValue::Boolean(true), "true"),
            (CqlValue::Blob(vec![0xca, 0xfe, 0x01]), "0xcafe01"),
            (CqlValue::Empty, ""),
            (CqlValue::Counter(Counter(-3)), "-3"),
            (CqlValue::BigInt(i64::MIN), "-9223372036854775808"),
            (CqlValue::Float(1.5), "1.5"),
            (CqlValue::Double(f64::NAN), "NaN"),
            (CqlValue::Double(f64::NEG_INFINITY), "-Infinity"),
            (CqlValue::Date(CqlDate(1 << 31)), "1970-01-01"),
            (CqlValue::Date(CqlDate((1 << 31) + 20_393)), "2025-11-01"),
            (CqlValue::Date(CqlDate((1 << 31) - 1)), "1969-12-31"),
            (
                CqlValue::Time(CqlTime(9 * 3_600_000_000_000 + 30 * 60_000_000_000 + 5)),
                "09:30:00.000000005",
            ),
            (
                CqlValue::Timestamp(CqlTimestamp(1_793_525_400_123)),
                "2026-11-01 09:30:00.123+0000",
            ),
            (
                CqlValue::Timestamp(CqlTimestamp(-1)),
                "1969-12-31 23:59:59.999+0000",
            ),
            (
                CqlValue::Duration(CqlDuration {
                    months: 14,
                    days: 3,
                    nanoseconds: 5_400_000_000_000,
                }),
                "1y2mo3d1h30m",
            ),
            (
                CqlValue::Duration(CqlDuration {
                    months: 0,
                    days: 0,
                    nanoseconds: -1_500_000,
                }),
                "-1ms500us",
            ),
            (
                CqlValue::Inet(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                "10.0.0.1",
            ),
            (
                CqlValue::Timeuuid(CqlTimeuuid::from_bytes([0; 16])),
                "00000000-0000-0000-0000-000000000000",
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(text(&value), expected, "{value:?}");
        }
    }

    #[test]
    fn writes_every_digit_of_varints_and_decimals() {
        let varint = |bytes: &[u8]| {
            text(&CqlValue::Varint(CqlVarint::from_signed_bytes_be_slice(
                bytes,
            )))
        };
        assert_eq!(varint(&[0x00]), "0");
        assert_eq!(varint(&[0xff]), "-1");
        assert_eq!(varint(&[0x80]), "-128");
        assert_eq!(
            varint(&[0x01, 0, 0, 0, 0, 0, 0, 0, 0]),
            "18446744073709551616"
        );
        let decimal = |bytes: &[u8], scale| {
            text(&CqlValue::Decimal(
                CqlDecimal::from_signed_be_bytes_slice_and_exponent(bytes, scale),
            ))
        };
        assert_eq!(decimal(&[0x04, 0xe2], 2), "12.50");
        assert_eq!(decimal(&[0xfb, 0x1e], 2), "-12.50");
        assert_eq!(decimal(&[0x05], 3), "0.005");
        assert_eq!(decimal(&[0x05], -2), "500");
        assert_eq!(decimal(&[0x00], 0), "0");
    }

    #[test]
    fn writes_collections_and_user_types_as_json() {
        let map = CqlValue::Map(vec![
            (CqlValue::Text("a\"b".to_owned()), CqlValue::Int(1)),
            (
                CqlValue::Text("c".to_owned()),
                CqlValue::List(vec![CqlValue::Double(f64::INFINITY)]),
            ),
        ]);
        assert_eq!(text(&map), r#"{"a\"b": 1, "c": ["Infinity"]}"#);
        let address = CqlValue::UserDefinedType {
            keyspace: "shop".to_owned(),
            name: "address".to_owned(),
            fields: vec![
                ("city".to_owned(), Some(CqlValue::Text("Oviedo".to_owned()))),
                ("zip".to_owned(), None),
            ],
        };
        assert_eq!(text(&address), r#"{"city": "Oviedo", "zip": null}"#);
        let tuple = CqlValue::Tuple(vec![Some(CqlValue::Boolean(false)), None]);
        assert_eq!(text(&tuple), "[false, null]");
        let keyed_by_number =
            CqlValue::Map(vec![(CqlValue::Int(7), CqlValue::Text("x".to_owned()))]);
        assert_eq!(text(&keyed_by_number), r#"{"7": "x"}"#);
    }

    #[test]
    fn names_types_as_cql_writes_them() {
        let list = ColumnType::Collection {
            frozen: true,
            typ: CollectionType::List(Box::new(ColumnType::Native(NativeType::Int))),
        };
        let map = ColumnType::Collection {
            frozen: false,
            typ: CollectionType::Map(
                Box::new(ColumnType::Native(NativeType::Text)),
                Box::new(list),
            ),
        };
        assert_eq!(type_name(&map), "map<text, frozen<list<int>>>");
        let tuple = ColumnType::Tuple(vec![
            ColumnType::Native(NativeType::Uuid),
            ColumnType::Native(NativeType::Varint),
        ]);
        assert_eq!(type_name(&tuple), "tuple<uuid, varint>");
    }
}
