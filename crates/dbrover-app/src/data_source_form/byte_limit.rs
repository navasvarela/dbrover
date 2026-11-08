//! A data source's byte limit (C0e's `ask_above_bytes`) as the dialog shows it: gigabytes as
//! cloud bills count them, for Athena's and BigQuery's limits.

use dbrover_core::ByteCount;

/// Bytes in a gigabyte, decimal, as cloud bills count them.
const GIGABYTE: u64 = 1_000_000_000;

/// The digits a gigabyte has after its point, to the byte.
const GIGABYTE_DIGITS: usize = 9;

/// `1.5` for 1,500,000,000 bytes: as few digits as say it exactly.
pub(super) fn gigabytes(bytes: ByteCount) -> String {
    let whole = bytes.0 / GIGABYTE;
    let fraction = bytes.0 % GIGABYTE;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:0width$}", width = GIGABYTE_DIGITS);
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// A number of gigabytes, such as `10` or `0.5`, read exactly; `None` for anything else,
/// zero, or more than a byte's precision.
pub(super) fn parse_gigabytes(text: &str) -> Option<ByteCount> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if whole.is_empty() && fraction.is_empty()
        || !digits(whole)
        || !digits(fraction)
        || fraction.len() > GIGABYTE_DIGITS
    {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction: u64 = format!("{fraction:0<width$}", width = GIGABYTE_DIGITS)
        .parse()
        .ok()?;
    let bytes = whole.checked_mul(GIGABYTE)?.checked_add(fraction)?;
    (bytes > 0).then_some(ByteCount(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gigabytes_are_read_and_written_to_the_byte() {
        for (text, bytes) in [
            ("10", Some(10_000_000_000)),
            ("0.5", Some(500_000_000)),
            (".25", Some(250_000_000)),
            ("1.000000001", Some(1_000_000_001)),
            ("1.0000000001", None),
            ("0", None),
            ("-1", None),
            ("1e3", None),
            (".", None),
        ] {
            assert_eq!(parse_gigabytes(text), bytes.map(ByteCount), "{text}");
        }
        assert_eq!(gigabytes(ByteCount(10_000_000_000)), "10");
        assert_eq!(gigabytes(ByteCount(1_000_000_001)), "1.000000001");
    }
}
