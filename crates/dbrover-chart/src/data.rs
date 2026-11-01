//! A chart's points, read from the rows of a result.

use dbrover_core::Row;
use jiff::civil::DateTime;
use jiff::tz::Offset;

use crate::values::{self, WrittenTime};
use crate::{ChartKind, Choice, Column, Role};

/// Bars charted at most. Past this a bar is narrower than a point on a wide screen and its
/// labels say nothing; a first guess, to be confirmed by measuring the chart tab.
pub const MAX_BARS: usize = 250;

/// Points charted at most on lines and scatters: a hundred of the grid's pages of 500 rows. A
/// first guess, to be confirmed by measuring the frame time of a scatter this big.
pub const MAX_POINTS: usize = 50_000;

/// What a chart draws: one X value per point, and each series' value there.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartData {
    pub kind: ChartKind,
    pub x_name: String,
    pub x: XValues,
    pub series: Vec<Series>,
    /// Each point's row, as an index into the rows the chart was read from.
    pub rows: Vec<usize>,
    pub summary: Summary,
}

/// Where the points sit along the X axis.
#[derive(Debug, Clone, PartialEq)]
pub enum XValues {
    /// One slot per point, in the rows' order, named by its value as the database wrote it.
    Categories(Vec<String>),
    /// Placed by value, in the order the points are drawn.
    Numbers(Vec<f64>),
    /// Placed by when they are, as seconds since 1970 of the date and time seen in `offset`.
    Times(Times),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Times {
    pub seconds: Vec<f64>,
    /// The offset of the first value that names one, which every value is seen in. `None`
    /// when no value names one: the times are then placed as they are written.
    pub offset: Option<Offset>,
    /// Some value had a time of day, so labels may need one.
    pub has_time: bool,
}

/// One Y column's values, point by point; `None` where the row has no number there.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

impl Series {
    /// Points with no number: a null, or a value that isn't a number.
    pub fn gaps(&self) -> usize {
        self.values.iter().filter(|value| value.is_none()).count()
    }
}

/// What was charted out of the rows given, for the line under the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub rows: usize,
    /// Rows past the limit, which aren't charted: the limit, when it was reached.
    pub limit: Option<usize>,
    /// Rows left out because their X is null or doesn't read as the column's role.
    pub without_x: usize,
}

impl ChartData {
    /// The points of `choice`, read from `rows` in their order. `roles` are `columns`' roles.
    pub fn read(columns: &[Column], roles: &[Role], rows: &[&Row], choice: &Choice) -> Self {
        let limit = match choice.kind {
            ChartKind::Bars => MAX_BARS,
            ChartKind::Lines | ChartKind::Scatter => MAX_POINTS,
        };
        let placement = Placement::of(choice.kind, roles.get(choice.x).copied());
        let mut points: Vec<Point> = Vec::new();
        let mut without_x = 0;
        for (index, row) in rows.iter().take(limit).enumerate() {
            let x_text = row.get(choice.x).and_then(Option::as_deref);
            let Some(x) = x_text.and_then(|text| placement.read(text)) else {
                without_x += 1;
                continue;
            };
            let ys = choice
                .ys
                .iter()
                .map(|&y| {
                    row.get(y)
                        .and_then(Option::as_deref)
                        .and_then(values::number)
                })
                .collect();
            points.push(Point { row: index, x, ys });
        }
        if choice.kind == ChartKind::Lines && placement != Placement::Categories {
            points.sort_by(|a, b| a.x.position().total_cmp(&b.x.position()));
        }
        let name = |index: usize| {
            columns
                .get(index)
                .map(|column| column.name.clone())
                .unwrap_or_default()
        };
        let series = choice
            .ys
            .iter()
            .enumerate()
            .map(|(position, &y)| Series {
                name: name(y),
                values: points
                    .iter()
                    .map(|point| point.ys.get(position).copied().flatten())
                    .collect(),
            })
            .collect();
        Self {
            kind: choice.kind,
            x_name: name(choice.x),
            x: x_values(&points),
            series,
            rows: points.iter().map(|point| point.row).collect(),
            summary: Summary {
                rows: rows.len(),
                limit: (rows.len() > limit).then_some(limit),
                without_x,
            },
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// How X values are placed: as categories, or by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Categories,
    Numbers,
    Times,
}

impl Placement {
    /// Bars are always categories, as is text; numbers and times are placed by value.
    fn of(kind: ChartKind, x: Option<Role>) -> Self {
        match (kind, x) {
            (ChartKind::Bars, _) | (_, Some(Role::Text) | None) => Self::Categories,
            (_, Some(Role::Number)) => Self::Numbers,
            (_, Some(Role::Time)) => Self::Times,
        }
    }

    fn read(self, text: &str) -> Option<X> {
        match self {
            Self::Categories => Some(X::Category(text.to_owned())),
            Self::Numbers => values::number(text).map(X::Number),
            Self::Times => values::time(text).map(X::Time),
        }
    }
}

struct Point {
    row: usize,
    x: X,
    ys: Vec<Option<f64>>,
}

enum X {
    Category(String),
    Number(f64),
    Time(WrittenTime),
}

impl X {
    /// Where the point sorts along the axis. Times sort by the instant they name, one written
    /// without an offset as if it were in UTC.
    fn position(&self) -> f64 {
        match self {
            Self::Category(_) => 0.0,
            Self::Number(number) => *number,
            Self::Time(time) => seconds(time.in_offset(Offset::UTC)),
        }
    }
}

/// Every point's X, all of one placement since one [`Placement`] read them.
fn x_values(points: &[Point]) -> XValues {
    let Some(first) = points.first() else {
        return XValues::Categories(Vec::new());
    };
    match first.x {
        X::Category(_) => XValues::Categories(
            points
                .iter()
                .filter_map(|point| match &point.x {
                    X::Category(name) => Some(name.clone()),
                    X::Number(_) | X::Time(_) => None,
                })
                .collect(),
        ),
        X::Number(_) => XValues::Numbers(
            points
                .iter()
                .filter_map(|point| match point.x {
                    X::Number(number) => Some(number),
                    X::Category(_) | X::Time(_) => None,
                })
                .collect(),
        ),
        X::Time(_) => XValues::Times(times(points)),
    }
}

fn times(points: &[Point]) -> Times {
    let written: Vec<WrittenTime> = points
        .iter()
        .filter_map(|point| match point.x {
            X::Time(time) => Some(time),
            X::Category(_) | X::Number(_) => None,
        })
        .collect();
    let offset = written.iter().find_map(|time| time.offset);
    let seen_in = offset.unwrap_or(Offset::UTC);
    Times {
        seconds: written
            .iter()
            .map(|time| seconds(time.in_offset(seen_in)))
            .collect(),
        offset,
        has_time: written.iter().any(|time| time.has_time),
    }
}

/// Seconds since 1970 of `date_time`, read as if it were in UTC: a position on the axis, which
/// [`crate::time`] turns back into the same date and time for labels.
pub(crate) fn seconds(date_time: DateTime) -> f64 {
    Offset::UTC
        .to_timestamp(date_time)
        .map(|instant| instant.as_second() as f64 + f64::from(instant.subsec_nanosecond()) / 1e9)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use dbrover_core::ValueKind;

    use super::*;

    fn columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|name| Column {
                name: (*name).to_owned(),
                kind: ValueKind::Undeclared,
            })
            .collect()
    }

    fn rows(values: &[&[Option<&str>]]) -> Vec<Row> {
        values
            .iter()
            .map(|row| row.iter().map(|value| value.map(str::to_owned)).collect())
            .collect()
    }

    fn read(roles: &[Role], rows: &[Row], choice: &Choice) -> ChartData {
        let names: Vec<String> = (0..roles.len()).map(|index| format!("c{index}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let borrowed: Vec<&Row> = rows.iter().collect();
        ChartData::read(&columns(&names), roles, &borrowed, choice)
    }

    #[test]
    fn bars_are_categories_in_the_rows_order() {
        let rows = rows(&[
            &[Some("2026-02"), Some("20")],
            &[Some("2026-01"), Some("10")],
        ]);
        let choice = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Text, Role::Number], &rows, &choice);
        assert_eq!(
            data.x,
            XValues::Categories(vec!["2026-02".to_owned(), "2026-01".to_owned()])
        );
        assert_eq!(data.series[0].values, vec![Some(20.0), Some(10.0)]);
        assert_eq!(data.series[0].name, "c1");
        assert_eq!(data.x_name, "c0");
        assert_eq!(data.rows, vec![0, 1]);
    }

    #[test]
    fn lines_over_numbers_or_times_are_drawn_in_x_order() {
        let rows = rows(&[
            &[Some("3"), Some("30")],
            &[Some("1"), Some("10")],
            &[Some("2"), Some("20")],
        ]);
        let choice = Choice {
            kind: ChartKind::Lines,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Number, Role::Number], &rows, &choice);
        assert_eq!(data.x, XValues::Numbers(vec![1.0, 2.0, 3.0]));
        assert_eq!(
            data.series[0].values,
            vec![Some(10.0), Some(20.0), Some(30.0)]
        );
        assert_eq!(data.rows, vec![1, 2, 0]);
    }

    #[test]
    fn a_scatter_keeps_the_rows_order() {
        let rows = rows(&[&[Some("3"), Some("30")], &[Some("1"), Some("10")]]);
        let choice = Choice {
            kind: ChartKind::Scatter,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Number, Role::Number], &rows, &choice);
        assert_eq!(data.x, XValues::Numbers(vec![3.0, 1.0]));
    }

    #[test]
    fn times_are_seen_in_the_first_offset_named() {
        let rows = rows(&[
            &[Some("2026-03-01 00:00:00+01"), Some("1")],
            &[Some("2026-03-01 00:00:00+00"), Some("2")],
        ]);
        let choice = Choice {
            kind: ChartKind::Lines,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Time, Role::Number], &rows, &choice);
        let XValues::Times(times) = data.x else {
            panic!("expected times, got {:?}", data.x);
        };
        assert_eq!(times.offset, Some(Offset::constant(1)));
        assert_eq!(times.seconds[1] - times.seconds[0], 3600.0);
        assert!(times.has_time);
    }

    #[test]
    fn a_row_without_x_is_left_out_and_counted() {
        let rows = rows(&[
            &[None, Some("1")],
            &[Some("a"), Some("2")],
            &[Some("not a date"), Some("3")],
        ]);
        let bars = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Text, Role::Number], &rows, &bars);
        assert_eq!(data.len(), 2);
        assert_eq!(data.summary.without_x, 1);

        let lines = Choice {
            kind: ChartKind::Lines,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Time, Role::Number], &rows, &lines);
        assert!(data.is_empty());
        assert_eq!(data.summary.without_x, 3);
    }

    #[test]
    fn a_missing_or_unreadable_y_is_a_gap() {
        let rows = rows(&[
            &[Some("a"), None],
            &[Some("b"), Some("NaN")],
            &[Some("c"), Some("4")],
        ]);
        let choice = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Text, Role::Number], &rows, &choice);
        assert_eq!(data.series[0].values, vec![None, None, Some(4.0)]);
        assert_eq!(data.series[0].gaps(), 2);
    }

    #[test]
    fn bars_stop_at_their_limit() {
        let many: Vec<Row> = (0..MAX_BARS + 10)
            .map(|index| vec![Some(index.to_string()), Some("1".to_owned())])
            .collect();
        let choice = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Text, Role::Number], &many, &choice);
        assert_eq!(data.len(), MAX_BARS);
        assert_eq!(data.summary.rows, MAX_BARS + 10);
        assert_eq!(data.summary.limit, Some(MAX_BARS));

        let lines = Choice {
            kind: ChartKind::Lines,
            ..choice
        };
        let data = read(&[Role::Number, Role::Number], &many, &lines);
        assert_eq!(data.len(), MAX_BARS + 10);
        assert_eq!(data.summary.limit, None);
    }

    #[test]
    fn text_on_x_is_placed_as_categories_whatever_the_kind() {
        let rows = rows(&[&[Some("b"), Some("1")], &[Some("a"), Some("2")]]);
        let choice = Choice {
            kind: ChartKind::Lines,
            x: 0,
            ys: vec![1],
        };
        let data = read(&[Role::Text, Role::Number], &rows, &choice);
        assert_eq!(
            data.x,
            XValues::Categories(vec!["b".to_owned(), "a".to_owned()])
        );
    }
}
