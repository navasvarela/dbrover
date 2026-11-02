//! Decimal numbers as their digits, so that reading, rounding and writing one never passes
//! through `f64`: an 18-decimal token amount keeps every digit.

use std::fmt;

/// The largest exponent a value may have (`1e400`), so a typo can't make a string of a
/// million zeros. Far beyond any SQL engine's decimals, PostgreSQL's 1,000 digits apart.
const MAX_EXPONENT: i64 = 400;

/// A decimal number: its digits, with no leading zeros, and how many of them are after the
/// point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decimal {
    negative: bool,
    /// ASCII digits, the last `scale` of them after the point. Never empty: zero is `"0"`
    /// with scale 0, or `"000"` with scale 2 for `0.00`.
    digits: Vec<u8>,
    scale: usize,
}

impl Decimal {
    /// `-12.50`, `+3`, `0.5`, `1.5E+3`, `-2e-4`: an optional sign, digits with an optional
    /// fraction, and an optional exponent. Not `.5`, `5.`, `1,5`, `NaN` or `Infinity`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (negative, unsigned) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, Some(exponent)),
            None => (unsigned, None),
        };
        let (whole, fraction) = match mantissa.split_once('.') {
            Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
            Some(_) => return None,
            None => (mantissa, ""),
        };
        let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || !all_digits(whole) || !all_digits(fraction) {
            return None;
        }
        let exponent: i64 = match exponent {
            Some(exponent) => exponent.parse().ok()?,
            None => 0,
        };
        if exponent.abs() > MAX_EXPONENT {
            return None;
        }
        let mut digits: Vec<u8> = whole.bytes().chain(fraction.bytes()).collect();
        let mut scale = fraction.len() as i64 - exponent;
        if scale < 0 {
            digits.extend(std::iter::repeat_n(b'0', scale.unsigned_abs() as usize));
            scale = 0;
        }
        let scale = scale.unsigned_abs() as usize;
        if digits.len() <= scale {
            let missing = scale + 1 - digits.len();
            digits.splice(0..0, std::iter::repeat_n(b'0', missing));
        }
        let decimal = Self {
            negative,
            digits,
            scale,
        };
        Some(decimal.trimmed())
    }

    /// Without leading zeros before the point, and never negative zero.
    fn trimmed(mut self) -> Self {
        let whole_digits = self.digits.len() - self.scale;
        let leading = self.digits[..whole_digits.saturating_sub(1)]
            .iter()
            .take_while(|&&digit| digit == b'0')
            .count();
        self.digits.drain(..leading);
        if self.is_zero() {
            self.negative = false;
        }
        self
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.digits.iter().all(|&digit| digit == b'0')
    }

    /// How many digits it has before the point; `0.5` has one.
    pub(crate) fn whole_digits(&self) -> usize {
        self.digits.len() - self.scale
    }

    /// How many digits before the point count towards a precision: none for `0.5`, whose
    /// zero is only there to be read.
    pub(crate) fn whole_digits_needed(&self) -> usize {
        match &self.digits[..self.whole_digits()] {
            [b'0'] => 0,
            whole => whole.len(),
        }
    }

    /// Whether every digit after the point is zero.
    pub(crate) fn is_whole(&self) -> bool {
        self.digits[self.whole_digits()..]
            .iter()
            .all(|&digit| digit == b'0')
    }

    /// Its digits from the first that isn't zero, trailing zeros included: `0.000120` has
    /// three, `1200` four. What the Excel writer counts against the 15 digits a spreadsheet's
    /// number holds exactly (`xlsx::typing`), so a value counted here past 15 is one it writes
    /// as text.
    pub(crate) fn significant_digits(&self) -> usize {
        self.digits
            .iter()
            .position(|&digit| digit != b'0')
            .map_or(0, |first| self.digits.len() - first)
    }

    /// Rounded to `scale` places, half away from zero (`2.5` to `3`, `-2.5` to `-3`), as
    /// PostgreSQL's `round(numeric)` and Excel's `ROUND` do; padded with zeros if it has
    /// fewer.
    pub(crate) fn round(&self, scale: usize) -> Self {
        let mut digits = self.digits.clone();
        if scale >= self.scale {
            digits.extend(std::iter::repeat_n(b'0', scale - self.scale));
            return Self {
                negative: self.negative,
                digits,
                scale,
            };
        }
        let cut = digits.len() - (self.scale - scale);
        let rounds_up = digits[cut] >= b'5';
        digits.truncate(cut);
        if rounds_up {
            carry(&mut digits);
        }
        if digits.len() <= scale {
            digits.insert(0, b'0');
        }
        Self {
            negative: self.negative,
            digits,
            scale,
        }
        .trimmed()
    }
}

/// Adds one to the last digit, carrying left, with a new leading `1` if every digit was 9.
fn carry(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            return;
        }
    }
    digits.insert(0, b'1');
}

/// Plain notation, with exactly its scale's digits after the point: `-1500`, `0.050`.
impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        let (whole, fraction) = self.digits.split_at(self.whole_digits());
        // Every byte is an ASCII digit, by construction in `parse` and `round`.
        f.write_str(&String::from_utf8_lossy(whole))?;
        if !fraction.is_empty() {
            f.write_str(".")?;
            f.write_str(&String::from_utf8_lossy(fraction))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or_else(|| panic!("{text:?} should parse"))
    }

    #[test]
    fn reads_and_writes_plain_notation() {
        let cases = [
            ("12.50", "12.50"),
            ("+3", "3"),
            ("-0.5", "-0.5"),
            ("007", "7"),
            ("000.250", "0.250"),
            ("-0", "0"),
            ("-0.00", "0.00"),
            ("1.5E+3", "1500"),
            ("1.5e3", "1500"),
            ("-2e-4", "-0.0002"),
            ("12345e-2", "123.45"),
            (
                "123456789012345678.123456789012345678",
                "123456789012345678.123456789012345678",
            ),
        ];
        for (text, written) in cases {
            assert_eq!(decimal(text).to_string(), written, "{text}");
        }
        for invalid in [
            "", "-", ".5", "5.", "1,5", "1.2.3", "NaN", "Infinity", "1e", "1e400000", "0x10", " 1",
            "1 ",
        ] {
            assert_eq!(Decimal::parse(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn rounds_half_away_from_zero_on_the_digits() {
        let cases = [
            ("2.5", 0, "3"),
            ("-2.5", 0, "-3"),
            ("2.49", 0, "2"),
            ("1.005", 2, "1.01"),
            ("0.125", 2, "0.13"),
            ("9.995", 2, "10.00"),
            ("-9.995", 2, "-10.00"),
            ("999.9", 0, "1000"),
            ("0.004", 2, "0.00"),
            ("-0.004", 2, "0.00"),
            ("12", 2, "12.00"),
            ("0.1", 18, "0.100000000000000000"),
            ("1234567890.123456789012345678", 8, "1234567890.12345679"),
        ];
        for (text, scale, rounded) in cases {
            assert_eq!(
                decimal(text).round(scale).to_string(),
                rounded,
                "{text} to {scale}"
            );
        }
    }

    #[test]
    fn counts_digits_as_a_spreadsheet_holds_them() {
        let cases = [
            ("0.000120", 1, 3),
            ("1200", 4, 4),
            ("0", 1, 0),
            ("123456789012345.6", 15, 16),
        ];
        for (text, whole, significant) in cases {
            let value = decimal(text);
            assert_eq!(value.whole_digits(), whole, "{text}");
            assert_eq!(value.significant_digits(), significant, "{text}");
        }
        assert!(decimal("12.000").is_whole());
        assert!(!decimal("12.001").is_whole());
    }
}
