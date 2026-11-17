//! Lines computed from the points a chart already drew (`docs/charts-as-code.md`, CH5g).
//!
//! Each is a statement about **those points**, which are visible: a trend through them, a moving
//! average of them, a line at their mean or median, or one at a value the spec names. Nothing
//! here reads a row the chart didn't draw, and the line under the chart says so — there is no
//! wording in which an overlay appears and its basis doesn't.

use crate::values;

/// The fewest points a moving average can be over: one point is the point.
pub const MIN_AVERAGE: usize = 2;

/// A line drawn from a series' own points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    /// A least-squares straight line through the series' points.
    Trend,
    /// A moving average over this many points, ending at each one.
    Average(usize),
    Mean,
    Median,
    /// A line across the plot at the value the spec names: the plain reference line.
    Line(f64),
}

impl Overlay {
    /// `"trend"`, `"average 7"`, `"mean"`, `"median"` or `"line 100"`, as a spec writes one.
    ///
    /// One key taking a small phrase, as `sort = "net desc"` already does, so the next overlay
    /// anyone asks for is a phrase in a list the reader already understands rather than another
    /// key in a spec whose growth is this plan's own named risk.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (word, rest) = match text.split_once(char::is_whitespace) {
            Some((word, rest)) => (word, rest.trim()),
            None => (text, ""),
        };
        match (word.to_ascii_lowercase().as_str(), rest) {
            ("trend", "") => Some(Self::Trend),
            ("mean", "") => Some(Self::Mean),
            ("median", "") => Some(Self::Median),
            ("average", points) => points
                .parse()
                .ok()
                .filter(|points| *points >= MIN_AVERAGE)
                .map(Self::Average),
            ("line", value) => values::number(value).map(Self::Line),
            _ => None,
        }
    }

    /// The way a spec writes it back.
    pub fn written(&self) -> String {
        match self {
            Self::Trend => "trend".to_owned(),
            Self::Average(points) => format!("average {points}"),
            Self::Mean => "mean".to_owned(),
            Self::Median => "median".to_owned(),
            Self::Line(value) => format!("line {}", values::number_text(*value)),
        }
    }

    /// How the line under the chart names it: "Trend", "A 7-point average", "Mean".
    pub fn named(&self) -> String {
        match self {
            Self::Trend => "Trend".to_owned(),
            Self::Average(points) => format!("A {points}-point average"),
            Self::Mean => "Mean".to_owned(),
            Self::Median => "Median".to_owned(),
            Self::Line(value) => format!("A line at {}", values::number_text(*value)),
        }
    }

    /// Whether `points` is enough for it: a moving average needs a window's worth, and the
    /// others need only the points they read.
    ///
    /// A window longer than the chart isn't refused when the keys are read — how many points
    /// there are isn't known until the rows arrive — so the overlay simply isn't drawn, and the
    /// line under the chart says why rather than leaving a key with no effect.
    pub fn fits(&self, points: usize) -> bool {
        match self {
            Self::Average(window) => *window <= points,
            Self::Trend | Self::Mean | Self::Median | Self::Line(_) => true,
        }
    }

    /// Why it wasn't drawn, when [`Self::fits`] says it couldn't be.
    pub fn too_short(&self, points: usize) -> Option<String> {
        match self {
            Self::Average(window) if *window > points => Some(format!(
                "{} needs {window} points; the chart has {points}",
                self.named()
            )),
            _ => None,
        }
    }

    /// Whether it reads a series' own values, and so sits where nothing was drawn when the
    /// series are piled on each other.
    ///
    /// A line at an absolute value is read against the axis, and a stack's total is read against
    /// the axis too, so that one is still honest where the others aren't.
    pub fn reads_a_series(&self) -> bool {
        !matches!(self, Self::Line(_))
    }

    /// The overlay's value at each point, from `values`, the series' own; `None` where it has
    /// none there.
    ///
    /// Gaps stay gaps. A trend and an average skip them and say they did; a moving average whose
    /// window holds one gives a gap, not a shorter window, since a window that quietly changes
    /// width changes what the line means halfway along.
    pub(crate) fn over(&self, xs: &[f64], values: &[Option<f64>]) -> Vec<Option<f64>> {
        match self {
            Self::Trend => trend(xs, values),
            Self::Average(points) => moving_average(*points, values),
            Self::Mean => flat(mean(values), values.len()),
            Self::Median => flat(median(values), values.len()),
            Self::Line(value) => flat(Some(*value), values.len()),
        }
    }
}

/// The same value at every point, or nothing at all.
fn flat(value: Option<f64>, points: usize) -> Vec<Option<f64>> {
    vec![value; points]
}

/// The least-squares straight line through the points that have a value, read at every point.
///
/// `xs` is the X as drawn: the slot's index for categories, the value for numbers and times.
/// Uniform slots make the index the distance, so the fit matches what the eye sees, and a trend
/// over months that `strftime` made text works rather than being refused on a technicality.
fn trend(xs: &[f64], values: &[Option<f64>]) -> Vec<Option<f64>> {
    let pairs: Vec<(f64, f64)> = xs
        .iter()
        .zip(values)
        .filter_map(|(x, value)| value.map(|value| (*x, value)))
        .collect();
    let count = pairs.len() as f64;
    if pairs.len() < 2 {
        return vec![None; values.len()];
    }
    let mean_x = pairs.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = pairs.iter().map(|(_, y)| y).sum::<f64>() / count;
    let spread: f64 = pairs.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    if spread <= 0.0 {
        // Every point at one X: a line through them has no direction, so there is none to draw.
        return vec![None; values.len()];
    }
    let slope: f64 = pairs
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>()
        / spread;
    xs.iter()
        .map(|x| Some(mean_y + slope * (x - mean_x)))
        .collect()
}

/// The mean of each window of `points` values ending at each point; `None` until there are
/// enough, and `None` wherever the window holds a gap.
fn moving_average(points: usize, values: &[Option<f64>]) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|at| {
            let from = (at + 1).checked_sub(points)?;
            let window = values.get(from..=at)?;
            let sum: f64 = window
                .iter()
                .copied()
                .collect::<Option<Vec<f64>>>()?
                .iter()
                .sum();
            Some(sum / points as f64)
        })
        .collect()
}

fn mean(values: &[Option<f64>]) -> Option<f64> {
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    (!present.is_empty()).then(|| present.iter().sum::<f64>() / present.len() as f64)
}

fn median(values: &[Option<f64>]) -> Option<f64> {
    let mut present: Vec<f64> = values.iter().flatten().copied().collect();
    if present.is_empty() {
        return None;
    }
    present.sort_by(f64::total_cmp);
    let middle = present.len() / 2;
    if present.len().is_multiple_of(2) {
        Some((present[middle - 1] + present[middle]) / 2.0)
    } else {
        present.get(middle).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_phrases_a_spec_writes_and_refuses_the_rest() {
        let cases = [
            ("trend", Some(Overlay::Trend)),
            ("mean", Some(Overlay::Mean)),
            ("median", Some(Overlay::Median)),
            ("MEAN", Some(Overlay::Mean)),
            ("  trend  ", Some(Overlay::Trend)),
            ("average 7", Some(Overlay::Average(7))),
            ("average  30", Some(Overlay::Average(30))),
            ("line 100", Some(Overlay::Line(100.0))),
            ("line -2.5", Some(Overlay::Line(-2.5))),
            // A window of one point is that point.
            ("average 1", None),
            ("average 0", None),
            ("average", None),
            ("average half", None),
            ("line", None),
            ("line high", None),
            ("regression", None),
            ("trend line", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(Overlay::parse(text), expected, "{text:?}");
        }
    }

    #[test]
    fn writes_back_what_it_read() {
        for text in ["trend", "average 7", "mean", "median", "line 100"] {
            let parsed = Overlay::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(parsed.written(), text);
        }
    }

    #[test]
    fn a_trend_is_the_least_squares_line_through_the_points_it_has() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        // Exactly on a line: the fit is that line.
        let on_a_line = Overlay::Trend.over(&xs, &[Some(1.0), Some(3.0), Some(5.0), Some(7.0)]);
        assert_eq!(on_a_line, vec![Some(1.0), Some(3.0), Some(5.0), Some(7.0)]);

        // A gap is skipped, and the line still reaches every point.
        let with_a_gap = Overlay::Trend.over(&xs, &[Some(1.0), None, Some(5.0), Some(7.0)]);
        let values: Vec<f64> = with_a_gap.iter().flatten().copied().collect();
        assert_eq!(values.len(), 4, "the fit is read at every point");
        assert!(values[1] > values[0] && values[2] > values[1]);

        // Fewer than two points, or every point at one X: no direction, so no line.
        assert_eq!(
            Overlay::Trend.over(&xs, &[Some(1.0), None, None, None]),
            vec![None; 4]
        );
        assert_eq!(
            Overlay::Trend.over(&[2.0, 2.0], &[Some(1.0), Some(5.0)]),
            vec![None, None]
        );
    }

    #[test]
    fn a_moving_average_waits_for_its_window_and_a_gap_in_it_is_a_gap() {
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let values = [Some(2.0), Some(4.0), Some(6.0), None, Some(10.0)];

        let over_two = Overlay::Average(2).over(&xs, &values);

        assert_eq!(
            over_two,
            vec![
                // No window yet.
                None,
                Some(3.0),
                Some(5.0),
                // The window holds the gap, and so does the one after it.
                None,
                None,
            ]
        );
        // A window longer than the points leaves nothing to draw.
        assert_eq!(Overlay::Average(9).over(&xs, &values), vec![None; 5]);
    }

    #[test]
    fn a_mean_and_a_median_are_flat_and_ignore_gaps() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let values = [Some(1.0), Some(2.0), None, Some(9.0)];

        assert_eq!(Overlay::Mean.over(&xs, &values), vec![Some(4.0); 4]);
        assert_eq!(Overlay::Median.over(&xs, &values), vec![Some(2.0); 4]);
        // An even count of values takes the middle two.
        assert_eq!(
            Overlay::Median.over(&[0.0, 1.0], &[Some(1.0), Some(4.0)]),
            vec![Some(2.5); 2]
        );
        assert_eq!(Overlay::Mean.over(&xs, &[None; 4]), vec![None; 4]);
        // A reference line is where it says, whatever the values.
        assert_eq!(Overlay::Line(7.5).over(&xs, &values), vec![Some(7.5); 4]);
    }

    #[test]
    fn an_average_longer_than_the_chart_says_so_rather_than_drawing_nothing_quietly() {
        assert!(Overlay::Average(7).fits(7));
        assert!(Overlay::Average(7).fits(12));
        assert!(!Overlay::Average(30).fits(12));
        assert_eq!(
            Overlay::Average(30).too_short(12).as_deref(),
            Some("A 30-point average needs 30 points; the chart has 12")
        );
        assert_eq!(Overlay::Average(7).too_short(12), None);
        for overlay in [Overlay::Trend, Overlay::Mean, Overlay::Line(1.0)] {
            assert!(overlay.fits(2), "{overlay:?}");
            assert_eq!(overlay.too_short(2), None);
        }
    }

    #[test]
    fn says_which_overlays_read_a_series_own_values() {
        assert!(Overlay::Trend.reads_a_series());
        assert!(Overlay::Average(7).reads_a_series());
        assert!(Overlay::Mean.reads_a_series());
        assert!(Overlay::Median.reads_a_series());
        assert!(
            !Overlay::Line(100.0).reads_a_series(),
            "an absolute value is read against the axis, as a stack's total is"
        );
    }

    #[test]
    fn names_itself_for_the_line_under_the_chart() {
        assert_eq!(Overlay::Trend.named(), "Trend");
        assert_eq!(Overlay::Average(7).named(), "A 7-point average");
        assert_eq!(Overlay::Mean.named(), "Mean");
        assert_eq!(Overlay::Line(100.0).named(), "A line at 100");
    }
}
