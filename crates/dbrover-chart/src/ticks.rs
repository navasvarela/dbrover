//! Axes of numbers: a range widened to round steps, the ticks in it, and their labels.

/// The most decimals a label shows, however small the step: past this the values are noise in
/// a double's last digits.
const MAX_DECIMALS: usize = 10;

/// The most ticks an axis gets, whatever its range: a guard for ranges so wide that their
/// span overflows and no round step fits.
const MAX_TICKS: usize = 100;

/// A range of an axis, and the round values marked along it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ticks {
    pub(crate) low: f64,
    pub(crate) high: f64,
    pub(crate) step: f64,
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
            step,
            values,
        }
    }

    /// `value` as its tick's label: with a thousands separator and the decimals the step needs.
    pub(crate) fn label(&self, value: f64) -> String {
        number_label(value, self.step)
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
    let magnitude = 10f64.powf(raw.log10().floor());
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
    let decimals = if step > 0.0 && step < 1.0 {
        (-step.log10()).ceil() as usize
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
    fn widens_the_range_to_whole_steps() {
        let ticks = Ticks::nice(3.0, 97.0, 5);
        assert_eq!(ticks.step, 20.0);
        assert_eq!((ticks.low, ticks.high), (0.0, 100.0));
        assert_eq!(ticks.values, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
    }

    #[test]
    fn negative_ranges_have_ticks_too() {
        let ticks = Ticks::nice(-12.0, 7.0, 4);
        assert_eq!(ticks.step, 5.0);
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
}
