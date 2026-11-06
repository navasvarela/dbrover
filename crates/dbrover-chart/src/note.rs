//! The line under a chart, on screen and in a file: what was charted, and what was left out and
//! why, so a picture never shows part of the data without saying so.

use crate::{ChartData, STARTING_SERIES, Summary};

impl ChartData {
    /// What was charted, rows left out and gaps, as one line: "Charting 12 rows · 2 gaps in
    /// net: null or not a number".
    pub fn note(&self) -> String {
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
        parts.extend(self.left_out());
        parts.join(" · ")
    }

    /// Each kind of row or value the chart couldn't draw, a sentence each: rows without an X,
    /// a split's values past the colours and repeated rows, and each series' gaps.
    pub fn left_out(&self) -> Vec<String> {
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
                let noun = if gaps == 1 { "gap" } else { "gaps" };
                parts.push(format!("{gaps} {noun} in {}: {why}", series.name));
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
        assert_eq!(data(all, 0).note(), "Charting 12 rows");
        let some = Summary {
            rows: 1200,
            limit: Some(250),
            without_x: 1,
            ..Summary::default()
        };
        assert_eq!(
            data(some, 2).note(),
            "Charting the first 250 of 1200 rows; group them in the query to chart them all \
             · 1 row left out: no month value · 2 gaps in total: null or not a number"
        );
        assert_eq!(data(Summary::default(), 0).note(), "No rows to chart");
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
            split.note(),
            "Charting 30 rows · 2 values of category left out, past 8 colours: helmets, tyres \
             · 1 row left out: an earlier row has the same month and category \
             · 1 gap in total: no row, null or not a number"
        );
        assert_eq!(split.left_out().len(), 3);
    }
}
