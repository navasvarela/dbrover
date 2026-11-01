//! MongoDB's Extended JSON: the objects that stand for one BSON value (`{"$oid": "…"}`), and
//! how the shell writes each.
//!
//! The forms are those of the specification, canonical and relaxed:
//! <https://github.com/mongodb/specifications/blob/master/source/extended-json/extended-json.md>.
//! A special object has exactly the fields its form names; anything else is an ordinary object.
//!
//! DynamoDB's sets, which BSON has no type for, are DbRover's own: `{"$stringSet": […]}`,
//! `{"$numberSet": […]}` and `{"$binarySet": […]}`, shown as PartiQL writes them, so an edited
//! set goes back a set (`docs/nosql.md`, N5).

use data_encoding::BASE64;

use crate::write::write_string;
use crate::{Shown, Syntax, Value};

/// A value's type, as the tree shows it beside the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    Null,
    Boolean,
    Number,
    String,
    Object,
    Array,
    ObjectId,
    Date,
    Int32,
    Int64,
    Double,
    Decimal128,
    Binary,
    Uuid,
    Timestamp,
    RegularExpression,
    MinKey,
    MaxKey,
    Code,
    Symbol,
    Undefined,
    DbPointer,
    StringSet,
    NumberSet,
    BinarySet,
}

impl ValueType {
    /// The type's name: JSON's own names in lowercase, BSON's as the shell names them.
    pub fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Number => "number",
            Self::String => "string",
            Self::Object => "object",
            Self::Array => "array",
            Self::ObjectId => "ObjectId",
            Self::Date => "Date",
            Self::Int32 => "Int32",
            Self::Int64 => "Long",
            Self::Double => "Double",
            Self::Decimal128 => "Decimal128",
            Self::Binary => "Binary",
            Self::Uuid => "UUID",
            Self::Timestamp => "Timestamp",
            Self::RegularExpression => "RegExp",
            Self::MinKey => "MinKey",
            Self::MaxKey => "MaxKey",
            Self::Code => "Code",
            Self::Symbol => "Symbol",
            Self::Undefined => "undefined",
            Self::DbPointer => "DBPointer",
            Self::StringSet => "StringSet",
            Self::NumberSet => "NumberSet",
            Self::BinarySet => "BinarySet",
        }
    }
}

/// The BSON subtype of a UUID in `$binary`, written as two hex digits.
const UUID_SUBTYPE: &str = "04";
const UUID_BYTES: usize = 16;
const MILLIS_PER_DAY: i64 = 86_400_000;

pub(crate) fn is_special(value: &Value) -> bool {
    special(value).is_some()
}

pub(crate) fn shown(value: &Value, syntax: Syntax) -> Shown {
    if syntax == Syntax::ExtendedJson
        && let Some(special) = special(value)
    {
        return Shown {
            value_type: special.value_type(),
            text: special.shell_text(),
        };
    }
    let (value_type, text) = match value {
        Value::Null => (ValueType::Null, "null".to_owned()),
        Value::Bool(flag) => (ValueType::Boolean, flag.to_string()),
        Value::Number(text) => (ValueType::Number, text.clone()),
        Value::String(text) => (ValueType::String, text.clone()),
        Value::Array(items) => (ValueType::Array, count(items.len(), "[", "item", "]")),
        Value::Object(fields) => (ValueType::Object, count(fields.len(), "{", "field", "}")),
    };
    Shown { value_type, text }
}

/// `{1 field}`, `[12 items]`.
fn count(n: usize, open: &str, noun: &str, close: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{open}{n} {noun}{plural}{close}")
}

/// One BSON value written as an object, its parts borrowed from it.
#[derive(Debug, PartialEq, Eq)]
enum Special<'a> {
    ObjectId(&'a str),
    /// Milliseconds since the Unix epoch, as the canonical form writes a date.
    DateMillis(i64),
    /// An ISO 8601 date, as the relaxed form writes one between 1970 and 9999.
    DateText(&'a str),
    Int32(&'a str),
    Int64(&'a str),
    Double(&'a str),
    Decimal128(&'a str),
    Binary {
        base64: &'a str,
        subtype: &'a str,
    },
    Timestamp {
        seconds: &'a str,
        increment: &'a str,
    },
    RegularExpression {
        pattern: &'a str,
        options: &'a str,
    },
    MinKey,
    MaxKey,
    Code(&'a str),
    Symbol(&'a str),
    Undefined,
    DbPointer {
        collection: &'a str,
        id: &'a str,
    },
    /// A DynamoDB set of strings, of numbers, or of binary values written in base64.
    Set(SetKind, &'a [Value]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetKind {
    String,
    Number,
    Binary,
}

fn special(value: &Value) -> Option<Special<'_>> {
    let Value::Object(fields) = value else {
        return None;
    };
    let [(name, inner)] = fields.as_slice() else {
        return None;
    };
    Some(match (name.as_str(), inner) {
        ("$oid", Value::String(hex)) => Special::ObjectId(hex),
        ("$date", Value::String(text)) => Special::DateText(text),
        ("$date", Value::Number(millis)) => Special::DateMillis(millis.parse().ok()?),
        ("$date", inner) => Special::DateMillis(string_field(inner, "$numberLong")?.parse().ok()?),
        ("$numberInt", Value::String(text)) => Special::Int32(text),
        ("$numberLong", Value::String(text)) => Special::Int64(text),
        ("$numberDouble", Value::String(text)) => Special::Double(text),
        ("$numberDecimal", Value::String(text)) => Special::Decimal128(text),
        ("$binary", Value::Object(parts)) => Special::Binary {
            base64: string_part(parts, "base64")?,
            subtype: string_part(parts, "subType")?,
        },
        ("$timestamp", Value::Object(parts)) => Special::Timestamp {
            seconds: number_part(parts, "t")?,
            increment: number_part(parts, "i")?,
        },
        ("$regularExpression", Value::Object(parts)) => Special::RegularExpression {
            pattern: string_part(parts, "pattern")?,
            options: string_part(parts, "options")?,
        },
        ("$minKey", Value::Number(one)) if one == "1" => Special::MinKey,
        ("$maxKey", Value::Number(one)) if one == "1" => Special::MaxKey,
        ("$code", Value::String(code)) => Special::Code(code),
        ("$symbol", Value::String(text)) => Special::Symbol(text),
        ("$undefined", Value::Bool(true)) => Special::Undefined,
        ("$stringSet", Value::Array(items)) if items.iter().all(is_string) => {
            Special::Set(SetKind::String, items)
        }
        ("$numberSet", Value::Array(items)) if items.iter().all(is_number) => {
            Special::Set(SetKind::Number, items)
        }
        ("$binarySet", Value::Array(items)) if items.iter().all(is_string) => {
            Special::Set(SetKind::Binary, items)
        }
        ("$dbPointer", Value::Object(parts)) => Special::DbPointer {
            collection: string_part(parts, "$ref")?,
            id: match parts.iter().find(|(name, _)| name == "$id") {
                Some((_, id)) => string_field(id, "$oid")?,
                None => return None,
            },
        },
        _ => return None,
    })
}

fn is_string(value: &Value) -> bool {
    matches!(value, Value::String(_))
}

fn is_number(value: &Value) -> bool {
    matches!(value, Value::Number(_))
}

/// The text of `value` if it is an object with the one field `name`, holding a string.
fn string_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    let Value::Object(fields) = value else {
        return None;
    };
    match fields.as_slice() {
        [(field, Value::String(text))] if field == name => Some(text),
        _ => None,
    }
}

fn part<'a>(parts: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    // Every part is required, and nothing else may be there.
    if parts.len() != 2 {
        return None;
    }
    parts
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value)
}

fn string_part<'a>(parts: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    match part(parts, name)? {
        Value::String(text) => Some(text),
        _ => None,
    }
}

fn number_part<'a>(parts: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    match part(parts, name)? {
        Value::Number(text) => Some(text),
        _ => None,
    }
}

impl Special<'_> {
    fn value_type(&self) -> ValueType {
        match self {
            Self::ObjectId(_) => ValueType::ObjectId,
            Self::DateMillis(_) | Self::DateText(_) => ValueType::Date,
            Self::Int32(_) => ValueType::Int32,
            Self::Int64(_) => ValueType::Int64,
            Self::Double(_) => ValueType::Double,
            Self::Decimal128(_) => ValueType::Decimal128,
            Self::Binary { .. } if self.uuid().is_some() => ValueType::Uuid,
            Self::Binary { .. } => ValueType::Binary,
            Self::Timestamp { .. } => ValueType::Timestamp,
            Self::RegularExpression { .. } => ValueType::RegularExpression,
            Self::MinKey => ValueType::MinKey,
            Self::MaxKey => ValueType::MaxKey,
            Self::Code(_) => ValueType::Code,
            Self::Symbol(_) => ValueType::Symbol,
            Self::Undefined => ValueType::Undefined,
            Self::DbPointer { .. } => ValueType::DbPointer,
            Self::Set(SetKind::String, _) => ValueType::StringSet,
            Self::Set(SetKind::Number, _) => ValueType::NumberSet,
            Self::Set(SetKind::Binary, _) => ValueType::BinarySet,
        }
    }

    /// The value as the shell writes it.
    fn shell_text(&self) -> String {
        match self {
            Self::ObjectId(hex) => call("ObjectId", hex),
            Self::DateMillis(millis) => iso_date(*millis).map_or_else(
                || format!("new Date({millis})"),
                |date| call("ISODate", &date),
            ),
            Self::DateText(text) => call("ISODate", text),
            Self::Int32(text) | Self::Double(text) => (*text).to_owned(),
            Self::Int64(text) => format!("NumberLong({text})"),
            Self::Decimal128(text) => call("NumberDecimal", text),
            Self::Binary { base64, subtype } => match self.uuid() {
                Some(uuid) => call("UUID", &uuid),
                None => {
                    let subtype = u8::from_str_radix(subtype, 16)
                        .map_or_else(|_| (*subtype).to_owned(), |number| number.to_string());
                    let mut quoted = String::new();
                    write_string(&mut quoted, base64);
                    format!("BinData({subtype}, {quoted})")
                }
            },
            Self::Timestamp { seconds, increment } => format!("Timestamp({seconds}, {increment})"),
            Self::RegularExpression { pattern, options } => format!("/{pattern}/{options}"),
            Self::MinKey => "MinKey".to_owned(),
            Self::MaxKey => "MaxKey".to_owned(),
            Self::Code(code) => call("Code", code),
            Self::Symbol(text) => call("Symbol", text),
            Self::Undefined => "undefined".to_owned(),
            Self::DbPointer { collection, id } => {
                let mut quoted = String::new();
                write_string(&mut quoted, collection);
                format!("DBPointer({quoted}, {})", call("ObjectId", id))
            }
            Self::Set(_, items) => partiql_set(items),
        }
    }

    /// A UUID's usual form, `8-4-4-4-12` hex digits, if this is binary data of the UUID
    /// subtype and the right length.
    fn uuid(&self) -> Option<String> {
        let Self::Binary { base64, subtype } = self else {
            return None;
        };
        if *subtype != UUID_SUBTYPE {
            return None;
        }
        let bytes = BASE64.decode(base64.as_bytes()).ok()?;
        if bytes.len() != UUID_BYTES {
            return None;
        }
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Some(format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ))
    }
}

/// A set as PartiQL writes it, `<<'a', 'b'>>` or `<<1, 2>>`: strings, and binary values'
/// base64, between single quotes, a quote in one written twice.
fn partiql_set(items: &[Value]) -> String {
    let items: Vec<String> = items
        .iter()
        .map(|item| match item {
            Value::String(text) => format!("'{}'", text.replace('\'', "''")),
            other => other.to_json(),
        })
        .collect();
    format!("<<{}>>", items.join(", "))
}

/// `name("text")`, the text quoted and escaped as a JSON string.
fn call(name: &str, text: &str) -> String {
    let mut out = format!("{name}(");
    write_string(&mut out, text);
    out.push(')');
    out
}

/// `millis` since the Unix epoch as ISO 8601 in UTC, to the millisecond, as the shell writes
/// a date: `2024-01-02T03:04:05.678Z`. `None` outside the years 0 to 9999, which ISO 8601
/// writes with a sign and more digits, and the shell doesn't write as `ISODate`.
fn iso_date(millis: i64) -> Option<String> {
    let days = millis.div_euclid(MILLIS_PER_DAY);
    let of_day = millis.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return None;
    }
    let (seconds, millisecond) = (of_day / 1000, of_day % 1000);
    let (hour, minute, second) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z"
    ))
}

/// The proleptic Gregorian date `days` after 1970-01-01, as (year, month, day).
///
/// Howard Hinnant's `civil_from_days`, unchanged:
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>. It counts in eras of
/// 400 years (146,097 days), each starting on 1 March, so the leap day falls at the end of a
/// year; months are counted from March, then turned back into January-based ones.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shown_as(text: &str, syntax: Syntax) -> (ValueType, String) {
        let shown = Value::parse(text).unwrap().shown(syntax);
        (shown.value_type, shown.text)
    }

    #[test]
    fn shows_each_bson_type_as_the_shell_writes_it() {
        let cases = [
            (
                r#"{"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}"#,
                ValueType::ObjectId,
                r#"ObjectId("5f1d7f0e2b5c3a0001a1b2c3")"#,
            ),
            (
                r#"{"$date": {"$numberLong": "1704164645678"}}"#,
                ValueType::Date,
                r#"ISODate("2024-01-02T03:04:05.678Z")"#,
            ),
            (
                r#"{"$date": {"$numberLong": "-1"}}"#,
                ValueType::Date,
                r#"ISODate("1969-12-31T23:59:59.999Z")"#,
            ),
            (
                r#"{"$date": {"$numberLong": "-62198755200001"}}"#,
                ValueType::Date,
                "new Date(-62198755200001)",
            ),
            (
                r#"{"$date": "2024-01-02T03:04:05Z"}"#,
                ValueType::Date,
                r#"ISODate("2024-01-02T03:04:05Z")"#,
            ),
            (
                r#"{"$date": 0}"#,
                ValueType::Date,
                r#"ISODate("1970-01-01T00:00:00.000Z")"#,
            ),
            (r#"{"$numberInt": "7"}"#, ValueType::Int32, "7"),
            (r#"{"$numberLong": "7"}"#, ValueType::Int64, "NumberLong(7)"),
            (r#"{"$numberDouble": "1.0"}"#, ValueType::Double, "1.0"),
            (
                r#"{"$numberDouble": "-Infinity"}"#,
                ValueType::Double,
                "-Infinity",
            ),
            (
                r#"{"$numberDecimal": "1.10"}"#,
                ValueType::Decimal128,
                r#"NumberDecimal("1.10")"#,
            ),
            (
                r#"{"$binary": {"base64": "c//SZESzTGmQ6OfR38A11A==", "subType": "04"}}"#,
                ValueType::Uuid,
                r#"UUID("73ffd264-44b3-4c69-90e8-e7d1dfc035d4")"#,
            ),
            (
                r#"{"$binary": {"base64": "AQID", "subType": "80"}}"#,
                ValueType::Binary,
                r#"BinData(128, "AQID")"#,
            ),
            (
                r#"{"$binary": {"base64": "AQID", "subType": "04"}}"#,
                ValueType::Binary,
                r#"BinData(4, "AQID")"#,
            ),
            (
                r#"{"$timestamp": {"t": 1, "i": 2}}"#,
                ValueType::Timestamp,
                "Timestamp(1, 2)",
            ),
            (
                r#"{"$regularExpression": {"pattern": "^a.c$", "options": "i"}}"#,
                ValueType::RegularExpression,
                "/^a.c$/i",
            ),
            (r#"{"$minKey": 1}"#, ValueType::MinKey, "MinKey"),
            (r#"{"$maxKey": 1}"#, ValueType::MaxKey, "MaxKey"),
            (
                r#"{"$code": "return 1"}"#,
                ValueType::Code,
                r#"Code("return 1")"#,
            ),
            (r#"{"$symbol": "s"}"#, ValueType::Symbol, r#"Symbol("s")"#),
            (r#"{"$undefined": true}"#, ValueType::Undefined, "undefined"),
            (
                r#"{"$dbPointer": {"$ref": "orders", "$id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}}}"#,
                ValueType::DbPointer,
                r#"DBPointer("orders", ObjectId("5f1d7f0e2b5c3a0001a1b2c3"))"#,
            ),
        ];
        for (text, value_type, shell) in cases {
            assert_eq!(
                shown_as(text, Syntax::ExtendedJson),
                (value_type, shell.to_owned()),
                "{text}"
            );
            assert_eq!(shown_as(text, Syntax::Json).0, ValueType::Object, "{text}");
        }
    }

    #[test]
    fn shows_dynamodbs_sets_as_partiql_writes_them() {
        let cases = [
            (
                r#"{"$stringSet": ["red", "it's"]}"#,
                ValueType::StringSet,
                "<<'red', 'it''s'>>",
            ),
            (
                r#"{"$numberSet": [1, 2.50, -3e2]}"#,
                ValueType::NumberSet,
                "<<1, 2.50, -3e2>>",
            ),
            (
                r#"{"$binarySet": ["AQID"]}"#,
                ValueType::BinarySet,
                "<<'AQID'>>",
            ),
        ];
        for (text, value_type, shown) in cases {
            assert_eq!(
                shown_as(text, Syntax::ExtendedJson),
                (value_type, shown.to_owned()),
                "{text}"
            );
        }
    }

    #[test]
    fn an_object_with_other_fields_is_only_an_object() {
        for text in [
            r#"{"$oid": "a", "name": "b"}"#,
            r#"{"$oid": 1}"#,
            r#"{"$numberLong": 7}"#,
            r#"{"$binary": {"base64": "AQID"}}"#,
            r#"{"$binary": {"base64": "AQID", "subType": "00", "x": 1}}"#,
            r#"{"$date": {"$numberLong": "soon"}}"#,
            r#"{"$minKey": 2}"#,
            r#"{"$dbPointer": {"$ref": "orders", "x": 1}}"#,
            r#"{"$stringSet": ["a", 1]}"#,
            r#"{"$numberSet": ["1"]}"#,
            r#"{"$binarySet": "AQID"}"#,
        ] {
            assert_eq!(
                shown_as(text, Syntax::ExtendedJson).0,
                ValueType::Object,
                "{text}"
            );
        }
    }

    #[test]
    fn shows_json_values_and_sizes() {
        let cases = [
            ("null", ValueType::Null, "null"),
            ("true", ValueType::Boolean, "true"),
            ("1.10", ValueType::Number, "1.10"),
            (r#""a \"b\"""#, ValueType::String, r#"a "b""#),
            ("[1]", ValueType::Array, "[1 item]"),
            ("[]", ValueType::Array, "[0 items]"),
            (
                r#"{"a": 1, "b": 2, "c": 3}"#,
                ValueType::Object,
                "{3 fields}",
            ),
            (r#"{"a": 1}"#, ValueType::Object, "{1 field}"),
        ];
        for (text, value_type, shown) in cases {
            for syntax in [Syntax::Json, Syntax::ExtendedJson] {
                assert_eq!(
                    shown_as(text, syntax),
                    (value_type, shown.to_owned()),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn counts_days_across_eras_and_leap_years() {
        let cases = [
            (0, (1970, 1, 1)),
            (-1, (1969, 12, 31)),
            (11_016, (2000, 2, 29)),
            (11_017, (2000, 3, 1)),
            (-719_468, (0, 3, 1)),
            (2_932_896, (9999, 12, 31)),
        ];
        for (days, date) in cases {
            assert_eq!(civil_from_days(days), date, "{days}");
        }
    }
}
