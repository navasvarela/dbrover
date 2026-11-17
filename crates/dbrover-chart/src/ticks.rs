//! Axes of numbers: a range widened to round steps, the ticks in it, and their labels.

/// The most decimals a label shows, however small the step: past this the values are noise in
/// a double's last digits.
const MAX_DECIMALS: usize = 10;

/// The most ticks an axis gets, whatever its range: a guard for ranges so wide that their
/// span overflows and no round step fits.
const MAX_TICKS: usize = 100;

/// A log axis gets 2 and 5 inside each decade while it spans at most this many of them. Three
/// ticks a decade over more than six decades is twenty labels, which collide on any axis a
/// chart gets, so past it only the decades are labelled.
const DECADES_WITH_STEPS: i32 = 6;

/// A range of an axis, and the round values marked along it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ticks {
    pub(crate) low: f64,
    pub(crate) high: f64,
    /// How far apart the ticks are on an even axis, which is also how many decimals their
    /// labels need. `None` on a log axis, where the gap changes decade by decade, so each
    /// label's decimals come from its own value instead.
    step: Option<f64>,
    pub(crate) values: Vec<f64>,
}

impl Ticks {
    /// About `target` ticks at 1, 2 or 5 times a power of ten, over `low` to `high` widened to
    /// whole steps. A range of one value is widened around it first, so it has a height.
    pub(crate) fn nice(low: f64, high: f64, target: usize) -> Self {
        let (low, high) = widened(low, high);
        let step = nice_step((high - low) / target.max(1) as f64);
        let first = (low / step).floor();
        let last = (high / step).ceil();
        let values = (0..)
            .map(|k| (first + f64::from(k)) * step)
            .take_while(|value| *value <= last * step + step / 2.0)
            .take(MAX_TICKS)
            .collect();
        Self {
            low: first * step,
            high: last * step,
            step: Some(step),
            values,
        }
    }

    /// Decades over `low` to `high`: the axis runs from the power of ten at or below `low` to
    /// the one at or above `high`, with 2 and 5 inside each decade while they fit.
    ///
    /// `fits` is how many labels the axis has room for, not a comfortable target as
    /// [`Self::nice`] takes: an even axis can choose any step, so it aims for one tick per so
    /// many points, while a log axis's granularity is the decade and the only question is
    /// whether the labels collide. Three ticks a decade need three times the room, and past
    /// [`DECADES_WITH_STEPS`] decades they would be too many labels for any axis a chart gets;
    /// past that the decades alone are shown, and where even those won't fit, every so many of
    /// them, as crowded category names are thinned.
    ///
    /// Only exponents are worked out here, through [`decimal_exponent`], so no `log10` is
    /// involved and the ticks are the same on every platform. `low` must be above zero, which
    /// is where a log axis starts; a range that isn't becomes one plain decade, for a chart
    /// with nothing left to place.
    pub(crate) fn decades(low: f64, high: f64, fits: usize) -> Self {
        let (low, high) = if low > 0.0 && high >= low {
            (low, high)
        } else {
            (1.0, 10.0)
        };
        let first = decimal_exponent(low);
        let above = decimal_exponent(high);
        let last = if power_of_ten(above) < high {
            above + 1
        } else {
            above
        };
        let top = power_of_ten(last);
        let decades: Vec<f64> = (first..=last).map(power_of_ten).collect();
        // At least a couple of labels, however little room there is: an axis with none says
        // nothing about what it measures.
        let fits = fits.max(2);
        let detailed = last - first <= DECADES_WITH_STEPS && decades.len() * 3 <= fits;
        let values: Vec<f64> = if detailed {
            decades
                .iter()
                .flat_map(|decade| [1.0, 2.0, 5.0].map(|step| step * decade))
                .filter(|value| *value <= top)
                .take(MAX_TICKS)
                .collect()
        } else {
            let every = decades.len().div_ceil(fits).max(1);
            decades
                .iter()
                .copied()
                .step_by(every)
                .take(MAX_TICKS)
                .collect()
        };
        Self {
            low: power_of_ten(first),
            high: top,
            step: None,
            values,
        }
    }

    /// `value` as its tick's label: with a thousands separator and the decimals its axis needs
    /// — the one step on an even axis, the value's own magnitude on a log one.
    pub(crate) fn label(&self, value: f64) -> String {
        number_label(value, self.step.unwrap_or_else(|| value.abs()))
    }
}

/// A range with room in it: one value becomes a band around it.
fn widened(low: f64, high: f64) -> (f64, f64) {
    if high > low {
        return (low, high);
    }
    let margin = if low == 0.0 { 1.0 } else { low.abs() / 10.0 };
    (low - margin, high + margin)
}

/// The round step nearest above `raw`: 1, 2 or 5 times a power of ten.
pub(crate) fn nice_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let magnitude = power_of_ten(decimal_exponent(raw));
    let normalised = raw / magnitude;
    let round = match normalised {
        n if n <= 1.0 => 1.0,
        n if n <= 2.0 => 2.0,
        n if n <= 5.0 => 5.0,
        _ => 10.0,
    };
    round * magnitude
}

/// `value` with as many decimals as `step` has, and commas between thousands.
pub(crate) fn number_label(value: f64, step: f64) -> String {
    // A step is 1, 2 or 5 times a power of ten, so it needs as many decimals as its exponent
    // is below zero.
    let decimals = if step > 0.0 && step < 1.0 {
        decimal_exponent(step).unsigned_abs() as usize
    } else {
        0
    }
    .min(MAX_DECIMALS);
    let text = format!("{value:.decimals$}");
    let (sign, unsigned) = match text.strip_prefix('-') {
        Some(unsigned) if unsigned.bytes().any(|byte| matches!(byte, b'1'..=b'9')) => {
            ("-", unsigned)
        }
        Some(unsigned) => ("", unsigned),
        None => ("", text.as_str()),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let grouped = thousands(whole);
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// The power of ten of `value`'s leading digit: 2 for 120.5, -1 for 0.5.
///
/// Read from the number as Rust writes it in scientific form rather than with `log10`, whose
/// last bit differs between platforms' maths libraries: on a power of ten such as 1,000 one
/// library's 2.9999999999999996 would floor to another step, and a report's chart, compared byte
/// for byte with a checked-in file, would differ by platform. Rust's own formatting is the same
/// everywhere.
pub(crate) fn decimal_exponent(value: f64) -> i32 {
    let written = format!("{value:e}");
    written
        .rsplit_once('e')
        .and_then(|(_, exponent)| exponent.parse().ok())
        .unwrap_or(0)
}

/// Ten to the power `exponent`, the double nearest it, as parsing `1e{exponent}` makes it on
/// every platform.
pub(crate) fn power_of_ten(exponent: i32) -> f64 {
    format!("1e{exponent}").parse().unwrap_or(1.0)
}

fn thousands(digits: &str) -> String {
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_one_two_or_five_times_a_power_of_ten() {
        let cases = [
            (0.7, 1.0),
            (1.0, 1.0),
            (1.3, 2.0),
            (3.0, 5.0),
            (7.0, 10.0),
            (230.0, 500.0),
            (0.013, 0.02),
            (0.0, 1.0),
            (f64::NAN, 1.0),
        ];
        for (raw, step) in cases {
            let found = nice_step(raw);
            assert!((found - step).abs() < 1e-12, "{raw}: {found} != {step}");
        }
    }

    #[test]
    fn reads_the_power_of_ten_of_a_leading_digit_exactly() {
        let cases = [
            (1000.0, 3),
            (999.999_999_999_999_9, 2),
            (120.5, 2),
            (1.0, 0),
            (0.5, -1),
            (0.1, -1),
            (0.05, -2),
            (1e-12, -12),
        ];
        for (value, exponent) in cases {
            assert_eq!(decimal_exponent(value), exponent, "{value}");
        }
        assert_eq!(power_of_ten(3), 1000.0);
        assert_eq!(power_of_ten(-1), 0.1);
    }

    #[test]
    fn widens_the_range_to_whole_steps() {
        let ticks = Ticks::nice(3.0, 97.0, 5);
        assert_eq!(ticks.step, Some(20.0));
        assert_eq!((ticks.low, ticks.high), (0.0, 100.0));
        assert_eq!(ticks.values, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
    }

    #[test]
    fn negative_ranges_have_ticks_too() {
        let ticks = Ticks::nice(-12.0, 7.0, 4);
        assert_eq!(ticks.step, Some(5.0));
        assert_eq!((ticks.low, ticks.high), (-15.0, 10.0));
        assert_eq!(ticks.values.first(), Some(&-15.0));
        assert_eq!(ticks.values.last(), Some(&10.0));
    }

    #[test]
    fn one_value_gets_a_range_around_it() {
        let zero = Ticks::nice(0.0, 0.0, 4);
        assert!(zero.low < 0.0 && zero.high > 0.0);
        let hundred = Ticks::nice(100.0, 100.0, 4);
        assert!(hundred.low < 100.0 && hundred.high > 100.0);
    }

    #[test]
    fn labels_have_separators_and_the_decimals_the_step_needs() {
        let cases = [
            (1_234_567.0, 1000.0, "1,234,567"),
            (-1_234.0, 500.0, "-1,234"),
            (0.25, 0.05, "0.25"),
            (0.2, 0.1, "0.2"),
            (12.0, 0.5, "12.0"),
            (-0.0, 1.0, "0"),
            (-0.000_000_000_01, 0.1, "0.0"),
            (999.0, 1.0, "999"),
        ];
        for (value, step, label) in cases {
            assert_eq!(number_label(value, step), label, "{value} by {step}");
        }
    }

    #[test]
    fn decades_tick_by_powers_of_ten() {
        // Plenty of room: 1, 2 and 5 inside each decade.
        let one = Ticks::decades(1.0, 9.0, 12);
        assert_eq!((one.low, one.high), (1.0, 10.0));
        assert_eq!(one.values, vec![1.0, 2.0, 5.0, 10.0]);

        // The axis rounds out to the decade below the lowest value and above the highest.
        let prices = Ticks::decades(12.0, 1290.0, 12);
        assert_eq!((prices.low, prices.high), (10.0, 10_000.0));
        assert_eq!(prices.values.first(), Some(&10.0));
        assert_eq!(prices.values.last(), Some(&10_000.0));
        assert_eq!(prices.values.len(), 10, "{:?}", prices.values);

        // Room for fewer labels than that, so the decades alone.
        let tight = Ticks::decades(12.0, 1290.0, 7);
        assert_eq!(tight.values, vec![10.0, 100.0, 1_000.0, 10_000.0]);

        // Room for fewer than the decades, so every other one, as crowded names are thinned.
        let cramped = Ticks::decades(12.0, 1290.0, 2);
        assert_eq!(cramped.values, vec![10.0, 1_000.0]);
        assert_eq!(
            (cramped.low, cramped.high),
            (10.0, 10_000.0),
            "thinning the labels doesn't move the axis"
        );

        // Below one the decades run the other way, and the labels carry the decimals.
        let small = Ticks::decades(0.02, 0.5, 12);
        assert_eq!((small.low, small.high), (0.01, 1.0));
        assert_eq!(small.label(0.02), "0.02");
        assert_eq!(small.label(0.5), "0.5");

        // Past six decades only the decades are labelled, however much room there is.
        let wide = Ticks::decades(1.0, 1e9, 100);
        assert_eq!(wide.values.len(), 10);
        assert!(
            wide.values
                .windows(2)
                .all(|pair| (pair[1] / pair[0] - 10.0).abs() < 1e-9),
            "{:?}",
            wide.values
        );
        assert_eq!(wide.label(1_000.0), "1,000");

        // However wide the range, the count is capped.
        let absurd = Ticks::decades(1e-300, 1e300, 1_000);
        assert!(absurd.values.len() <= MAX_TICKS, "{}", absurd.values.len());

        // A range a log axis can't start from becomes one plain decade.
        let nothing = Ticks::decades(f64::INFINITY, f64::NEG_INFINITY, 12);
        assert_eq!((nothing.low, nothing.high), (1.0, 10.0));
    }
}
