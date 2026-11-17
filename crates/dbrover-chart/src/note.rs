//! The line under a chart, on screen and in a file: what was charted, and what was left out and
//! why, so a picture never shows part of the data without saying so.

use crate::overlay::Overlay;
use crate::{ChartData, LayoutOptions, STARTING_SERIES, Summary, XValues, YScale};

impl ChartData {
    /// What was charted, rows left out and gaps, as one line: "Charting 12 rows · 2 gaps in
    /// net: null or not a number".
    ///
    /// `options` are the layout's: the value axis's scale, since a log one can't place a value
    /// at or below zero and those are gaps of their own, and the overlays, each of which is a
    /// claim about the points drawn and so has to say what it was computed over.
    pub fn note(&self, options: &LayoutOptions) -> String {
        let summary = &self.summary;
        if summary.rows == 0 {
            return "No rows to chart".to_owned();
        }
        let mut parts = vec![match summary.limit {
            Some(limit) => format!(
                "Charting the first {limit} of {} rows; group them in the query to chart them all",
                summary.rows
            ),
            None => format!("Charting {}", rows(summary.rows)),
        }];
        parts.extend(self.overlaid(&options.overlay));
        parts.extend(self.left_out(options.y_scale));
        parts.join(" · ")
    }

    /// What each overlay was computed over, so there is no wording in which one appears and its
    /// basis doesn't: how many of the rows charted it read, what it read them against, and the
    /// gaps it skipped.
    ///
    /// Overlays that read the same points share a sentence — "Trend and mean over those 12
    /// rows, by month" — since repeating the basis once an overlay reads nothing differently.
    fn overlaid(&self, overlays: &[Overlay]) -> Vec<String> {
        if overlays.is_empty() || self.is_empty() {
            return Vec::new();
        }
        let points = self.len();
        let gaps: usize = self
            .series
            .iter()
            .map(|series| series.gaps())
            .max()
            .unwrap_or_default();
        let read = points - gaps.min(points);
        let against = match &self.x {
            // Uniform slots make the index the distance, and a reader has to know it was the
            // slot rather than a value.
            XValues::Categories(_) => "by position".to_owned(),
            XValues::Numbers(_) | XValues::Times(_) => format!("by {}", self.x_name),
        };
        let (drawn, short): (Vec<&Overlay>, Vec<&Overlay>) =
            overlays.iter().partition(|overlay| overlay.fits(points));
        let mut parts: Vec<String> = short
            .iter()
            .filter_map(|overlay| overlay.too_short(points))
            .collect();
        if drawn.is_empty() {
            return parts;
        }
        let named: Vec<String> = drawn.iter().map(|overlay| overlay.named()).collect();
        let over = if gaps > 0 {
            format!(
                "over {read} of the {} charted, {against}: {}",
                rows(points),
                gaps_counted(gaps)
            )
        } else {
            format!("over those {}, {against}", rows(points))
        };
        parts.insert(0, format!("{} {over}", joined(&named)));
        parts
    }

    /// Each kind of row or value the chart couldn't draw, a sentence each: rows without an X,
    /// a split's values past the colours, repeated rows, each series' gaps, and on a log scale
    /// the values at or below zero, which have no place on one.
    pub fn left_out(&self, scale: YScale) -> Vec<String> {
        let summary = &self.summary;
        let mut parts = Vec::new();
        if summary.without_x > 0 {
            parts.push(format!(
                "{} left out: no {} value",
                rows(summary.without_x),
                self.x_name
            ));
        }
        if let Some(split_by) = &self.split_by {
            parts.extend(split_left_out(summary, split_by, &self.x_name));
        }
        let why = match self.split_by {
            Some(_) => "no row, null or not a number",
            None => "null or not a number",
        };
        for series in &self.series {
            let gaps = series.gaps();
            if gaps > 0 {
                parts.push(format!("{} in {}: {why}", gaps_counted(gaps), series.name));
            }
            if scale == YScale::Log {
                let unplaceable = series
                    .values
                    .iter()
                    .flatten()
                    .filter(|value| **value <= 0.0)
                    .count();
                if unplaceable > 0 {
                    parts.push(format!(
                        "{} in {}: zero or negative on a log scale",
                        gaps_counted(unplaceable),
                        series.name
                    ));
                }
            }
        }
        parts
    }
}

/// What a chart split by `split_by` left out: values past the palette's colours, and rows whose
/// X and series an earlier row had.
fn split_left_out(summary: &Summary, split_by: &str, x_name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    if !summary.series_left_out.is_empty() {
        let count = summary.series_left_out.len();
        let noun = if count == 1 { "value" } else { "values" };
        parts.push(format!(
            "{count} {noun} of {split_by} left out, past {STARTING_SERIES} colours: {}",
            summary.series_left_out.join(", ")
        ));
    }
    if summary.repeated > 0 {
        parts.push(format!(
            "{} left out: an earlier row has the same {x_name} and {split_by}",
            rows(summary.repeated)
        ));
    }
    parts
}

/// "Trend", "Trend and mean", "Trend, mean and median": the overlays as one phrase, the first
/// capitalised as [`Overlay::named`] writes it and the rest not, so the sentence reads as one.
fn joined(named: &[String]) -> String {
    let lowered: Vec<String> = named
        .iter()
        .enumerate()
        .map(|(position, name)| {
            if position == 0 {
                name.clone()
            } else {
                lowercase_first(name)
            }
        })
        .collect();
    match lowered.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn lowercase_first(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn gaps_counted(count: usize) -> String {
    let noun = if count == 1 { "gap" } else { "gaps" };
    format!("{count} {noun}")
}

fn rows(count: usize) -> String {
    match count {
        1 => "1 row".to_owned(),
        count => format!("{count} rows"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{ChartKind, Series, XValues};

    use super::*;

    fn data(summary: Summary, gaps: usize) -> ChartData {
        ChartData {
            kind: ChartKind::Bars,
            x_name: "month".to_owned(),
            x: XValues::Categories(Vec::new()),
            series: vec![Series {
                name: "total".to_owned(),
                values: vec![None; gaps],
                rows: vec![None; gaps],
            }],
            split_by: None,
            rows: Vec::new(),
            stacked: false,
            summary,
        }
    }

    #[test]
    fn says_what_was_charted_and_what_was_left_out() {
        let all = Summary {
            rows: 12,
            ..Summary::default()
        };
        assert_eq!(
            data(all, 0).note(&LayoutOptions::default()),
            "Charting 12 rows"
        );
        let some = Summary {
            rows: 1200,
            limit: Some(250),
            without_x: 1,
            ..Summary::default()
        };
        assert_eq!(
            data(some, 2).note(&LayoutOptions::default()),
            "Charting the first 250 of 1200 rows; group them in the query to chart them all \
             · 1 row left out: no month value · 2 gaps in total: null or not a number"
        );
        assert_eq!(
            data(Summary::default(), 0).note(&LayoutOptions::default()),
            "No rows to chart"
        );
    }

    #[test]
    fn says_what_a_split_left_out() {
        let split = ChartData {
            split_by: Some("category".to_owned()),
            ..data(
                Summary {
                    rows: 30,
                    series_left_out: vec!["helmets".to_owned(), "tyres".to_owned()],
                    repeated: 1,
                    ..Summary::default()
                },
                1,
            )
        };
        assert_eq!(
            split.note(&LayoutOptions::default()),
            "Charting 30 rows · 2 values of category left out, past 8 colours: helmets, tyres \
             · 1 row left out: an earlier row has the same month and category \
             · 1 gap in total: no row, null or not a number"
        );
        assert_eq!(split.left_out(YScale::Linear).len(), 3);
    }

    /// There is no wording in which an overlay appears and its basis doesn't.
    #[test]
    fn names_every_overlay_and_what_it_was_computed_over() {
        let overlaid = |overlays: Vec<Overlay>| LayoutOptions {
            overlay: overlays,
            ..LayoutOptions::default()
        };
        let months = |count: usize| {
            XValues::Categories(
                (1..=count)
                    .map(|month| format!("2026-{month:02}"))
                    .collect(),
            )
        };
        let chart = |x: XValues, values: Vec<Option<f64>>, summary: Summary| ChartData {
            kind: crate::ChartKind::Lines,
            x_name: "month".to_owned(),
            x,
            series: vec![crate::Series {
                name: "net".to_owned(),
                rows: (0..values.len()).map(Some).collect(),
                values,
            }],
            split_by: None,
            rows: (0..summary.rows.min(12)).collect(),
            stacked: false,
            summary,
        };

        // Over categories the fit is by position, and a reader has to know it.
        let twelve = chart(
            months(12),
            vec![Some(1.0); 12],
            Summary {
                rows: 12,
                ..Summary::default()
            },
        );
        assert_eq!(
            twelve.note(&overlaid(vec![Overlay::Trend, Overlay::Mean])),
            "Charting 12 rows · Trend and mean over those 12 rows, by position"
        );
        assert_eq!(
            twelve.note(&overlaid(vec![Overlay::Trend])),
            "Charting 12 rows · Trend over those 12 rows, by position"
        );
        assert_eq!(
            twelve.note(&overlaid(vec![
                Overlay::Trend,
                Overlay::Mean,
                Overlay::Line(100.0)
            ])),
            "Charting 12 rows · Trend, mean and a line at 100 over those 12 rows, by position"
        );

        // Over numbers and times it is by the X column, named.
        let numbers = chart(
            XValues::Numbers((0..12).map(f64::from).collect()),
            vec![Some(1.0); 12],
            Summary {
                rows: 12,
                ..Summary::default()
            },
        );
        assert_eq!(
            numbers.note(&overlaid(vec![Overlay::Average(7)])),
            "Charting 12 rows · A 7-point average over those 12 rows, by month"
        );

        // The clause that says the rows were cut reads straight into the overlay's basis.
        let cut = chart(
            months(12),
            vec![Some(1.0); 12],
            Summary {
                rows: 1_204,
                limit: Some(12),
                ..Summary::default()
            },
        );
        assert_eq!(
            cut.note(&overlaid(vec![Overlay::Trend, Overlay::Mean])),
            "Charting the first 12 of 1204 rows; group them in the query to chart them all · \
             Trend and mean over those 12 rows, by position"
        );

        // Gaps stay gaps, and the overlay says how many it skipped.
        let mut values = vec![Some(1.0); 12];
        values[3] = None;
        values[7] = None;
        let gappy = chart(
            months(12),
            values,
            Summary {
                rows: 12,
                ..Summary::default()
            },
        );
        assert_eq!(
            gappy.note(&overlaid(vec![Overlay::Trend])),
            "Charting 12 rows · Trend over 10 of the 12 rows charted, by position: 2 gaps · \
             2 gaps in net: null or not a number"
        );

        // A window longer than the chart isn't drawn, and the note says why rather than
        // leaving the key looking as though it did nothing.
        assert_eq!(
            twelve.note(&overlaid(vec![Overlay::Average(30)])),
            "Charting 12 rows · A 30-point average needs 30 points; the chart has 12"
        );
        assert_eq!(
            twelve.note(&overlaid(vec![Overlay::Trend, Overlay::Average(30)])),
            "Charting 12 rows · Trend over those 12 rows, by position · A 30-point average \
             needs 30 points; the chart has 12"
        );

        // No overlay, no sentence.
        assert_eq!(twelve.note(&LayoutOptions::default()), "Charting 12 rows");
    }
}
