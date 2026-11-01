//! A grid value as a CQL literal of its column's type. CQL converts nothing implicitly: a string
//! literal in an `int` column is an error, so each value is written as its type reads it.
//!
//! A value written bare (a number, a UUID, a blob, a duration) is first checked to be one, and
//! is written as a string otherwise, so the server refuses it as the wrong type rather than read
//! text from the grid as CQL.

use dbrover_core::Quoting;

/// `text` as a literal of the CQL type `cql_type`, as the driver names types (`int`,
/// `map<text, int>`, `frozen<address>`).
pub(crate) fn literal(cql_type: &str, text: &str) -> String {
    let string = || Quoting::STANDARD.string_literal(text);
    let bare_if = |valid: bool| if valid { text.to_owned() } else { string() };
    match cql_type {
        "ascii" | "text" | "varchar" | "inet" | "date" | "time" | "timestamp" => string(),
        "tinyint" | "smallint" | "int" | "bigint" | "varint" | "counter" => {
            bare_if(is_integer(text))
        }
        "decimal" => bare_if(is_number(text)),
        "float" | "double" => bare_if(is_number(text) || is_special_float(text)),
        "boolean" => {
            bare_if(text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false"))
        }
        "uuid" | "timeuuid" => bare_if(is_uuid(text)),
        "blob" => bare_if(is_blob(text)),
        "duration" => bare_if(is_duration(text)),
        // Collections, tuples, vectors and user-defined types, which the grid shows as JSON.
        _ => format!("fromJson({})", string()),
    }
}

fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// Digits with an optional sign, fraction and exponent: `-12.50`, `1e-3`.
fn is_number(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => is_digits(whole) && is_digits(fraction),
        None => is_digits(mantissa),
    };
    let exponent_ok =
        exponent.is_none_or(|exponent| is_integer(exponent.strip_prefix('+').unwrap_or(exponent)));
    mantissa_ok && exponent_ok
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_special_float(text: &str) -> bool {
    matches!(text, "NaN" | "Infinity" | "-Infinity")
}

/// `123e4567-e89b-12d3-a456-426614174000`: hex digits in groups of 8, 4, 4, 4 and 12.
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// `0x` and hex digits, two a byte.
fn is_blob(text: &str) -> bool {
    text.strip_prefix("0x")
        .is_some_and(|hex| hex.len() % 2 == 0 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// CQL's duration format, as the driver writes one: an optional `-`, then amounts each with
/// its unit, `1y2mo3d4h5m6s7ms8us9ns`.
fn is_duration(text: &str) -> bool {
    const UNITS: &[&str] = &["mo", "ms", "us", "µs", "ns", "y", "w", "d", "h", "m", "s"];
    let mut rest = text.strip_prefix('-').unwrap_or(text);
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        let Some(unit) = UNITS.iter().find(|unit| rest.starts_with(**unit)) else {
            return false;
        };
        rest = &rest[unit.len()..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_type_as_its_literal() {
        let cases = [
            ("text", "it's", "'it''s'"),
            (
                "timestamp",
                "2026-11-01 09:30:00.123+0000",
                "'2026-11-01 09:30:00.123+0000'",
            ),
            ("int", "-42", "-42"),
            ("int", "42; DROP TABLE t", "'42; DROP TABLE t'"),
            ("bigint", "1.5", "'1.5'"),
            ("decimal", "12.50", "12.50"),
            ("double", "1e-3", "1e-3"),
            ("double", "NaN", "NaN"),
            ("float", "1,5", "'1,5'"),
            ("boolean", "True", "True"),
            ("boolean", "yes", "'yes'"),
            (
                "uuid",
                "123e4567-e89b-12d3-a456-426614174000",
                "123e4567-e89b-12d3-a456-426614174000",
            ),
            ("timeuuid", "not-a-uuid", "'not-a-uuid'"),
            ("blob", "0xCAFE", "0xCAFE"),
            ("blob", "0xCAF", "'0xCAF'"),
            ("duration", "1h30m", "1h30m"),
            ("duration", "-1mo2d", "-1mo2d"),
            ("duration", "1 hour", "'1 hour'"),
            ("map<text, int>", r#"{"a": 1}"#, r#"fromJson('{"a": 1}')"#),
            (
                "frozen<address>",
                r#"{"city": "O'Hare"}"#,
                r#"fromJson('{"city": "O''Hare"}')"#,
            ),
        ];
        for (cql_type, text, expected) in cases {
            assert_eq!(literal(cql_type, text), expected, "{cql_type}: {text}");
        }
    }
}
