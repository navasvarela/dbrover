//! What every SVG writer shares, so ER diagrams and charts look alike: the fonts they name,
//! where text sits on its line, colours, numbers and escaped text.

use std::fmt::{self, Write as _};

/// The fonts pictures name, each followed by the platform's, for a browser without them. A
/// PNG is drawn with the bundled ones ([`crate::Fonts::bundled`]).
pub const SANS: &str = "Inter, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif";
pub const MONO: &str = "'JetBrains Mono', Menlo, Consolas, monospace";

/// From a line of text's middle down to its baseline, as a share of its size: about where
/// Inter's and JetBrains Mono's lower-case letters sit. Not `dominant-baseline`, which resvg and
/// browsers read differently.
pub const BASELINE_DROP: f32 = 0.35;

/// A colour, as a picture writes it: `#2a78d6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// A length or coordinate in points, written to a hundredth of a point with no trailing zeros:
/// `12.5`, `300`, `-0.25`.
///
/// Rounded, rather than written as the float's shortest form, so a picture drawn from the same
/// rows is the same file on every platform: layout's arithmetic is IEEE's everywhere, but a
/// maths library's last bit isn't, and a hundredth of a point hides it. Rounded in integers, so
/// the text never depends on how a platform prints a float.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Points(pub f32);

/// Hundredths of a point in a point.
const HUNDREDTHS: f64 = 100.0;

impl fmt::Display for Points {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.is_finite() {
            return f.write_str("0");
        }
        // Finite f32s times 100 are far inside i64's range.
        let hundredths = (f64::from(self.0) * HUNDREDTHS).round() as i64;
        let sign = if hundredths < 0 { "-" } else { "" };
        let magnitude = hundredths.unsigned_abs();
        let (whole, fraction) = (magnitude / 100, magnitude % 100);
        match fraction {
            0 => write!(f, "{sign}{whole}"),
            fraction if fraction % 10 == 0 => write!(f, "{sign}{whole}.{}", fraction / 10),
            fraction => write!(f, "{sign}{whole}.{fraction:02}"),
        }
    }
}

/// Text as XML character data: `&`, `<` and `>` escaped, and the control characters XML
/// can't hold dropped.
pub struct Escaped<'a>(pub &'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                c if c.is_control() && !matches!(c, '\t' | '\n') => {}
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_colour_as_hex() {
        assert_eq!(Rgb(0x2a, 0x78, 0xd6).to_string(), "#2a78d6");
        assert_eq!(Rgb(0, 0, 0).to_string(), "#000000");
    }

    #[test]
    fn writes_points_to_a_hundredth() {
        let cases = [
            (300.0, "300"),
            (12.5, "12.5"),
            (12.345_678, "12.35"),
            (0.004, "0"),
            (-0.004, "0"),
            (-0.25, "-0.25"),
            (7.1, "7.1"),
            (7.05, "7.05"),
            (f32::NAN, "0"),
        ];
        for (points, written) in cases {
            assert_eq!(Points(points).to_string(), written, "{points}");
        }
    }

    #[test]
    fn escapes_markup_and_drops_control_characters() {
        assert_eq!(
            Escaped("<Q3> & co\u{7}\tok").to_string(),
            "&lt;Q3&gt; &amp; co\tok"
        );
    }
}
