//! A chart's points, read from the rows of a result.

use std::collections::HashMap;

use dbrover_core::Row;
use jiff::civil::DateTime;
use jiff::tz::Offset;

use crate::values::{self, WrittenTime};
use crate::{ChartKind, Choice, Column, Role, STARTING_SERIES};

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
    /// The name of the column whose values the series are, when the chart is split by one.
    pub split_by: Option<String>,
    /// Each point's row, as an index into the rows the chart was read from: the first row with
    /// its X when the chart is split by a column.
    pub rows: Vec<usize>,
    /// The bars are stacked rather than side by side.
    pub stacked: bool,
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

/// One series' values, point by point: a Y column's, or the one Y's in the rows of one value of
/// the column the chart is split by.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// `None` where the point has no number: a null, a value that isn't a number, or, split by
    /// a column, no row for this series there.
    pub values: Vec<Option<f64>>,
    /// The row each value was read from, as an index into the rows the chart was read from;
    /// `None` where the series has no row at the point.
    pub rows: Vec<Option<usize>>,
}

impl Series {
    /// Points with no number: a null, a value that isn't a number, or no row.
    pub fn gaps(&self) -> usize {
        self.values.iter().filter(|value| value.is_none()).count()
    }
}

/// What was charted out of the rows given, for the line under the chart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub rows: usize,
    /// Rows past the limit, which aren't charted: the limit, when it was reached.
    pub limit: Option<usize>,
    /// Rows left out because their X is null or doesn't read as the column's role.
    pub without_x: usize,
    /// Split by a column, its values past the eighth, in the order first seen, whose rows
    /// aren't charted: a series each would need more colours than the palette has.
    pub series_left_out: Vec<String>,
    /// Split by a column, rows left out because an earlier row had the same X and series.
    pub repeated: usize,
}

/// What a split chart's series column holds where it is null, as the grid shows it.
const NULL_SERIES: &str = "NULL";

impl ChartData {
    /// The points of `choice`, read from `rows` in their order. `roles` are `columns`' roles.
    pub fn read(columns: &[Column], roles: &[Role], rows: &[&Row], choice: &Choice) -> Self {
        let limit = match choice.kind {
            ChartKind::Bars => MAX_BARS,
            ChartKind::Lines | ChartKind::Scatter => MAX_POINTS,
        };
        let placement = Placement::of(choice.kind, roles.get(choice.x).copied());
        let name = |index: usize| {
            columns
                .get(index)
                .map(|column| column.name.clone())
                .unwrap_or_default()
        };
        let mut reader = match choice.series {
            Some(series) => Reader::split(series, choice.ys.first().copied()),
            None => Reader::columns(choice.ys.iter().map(|&y| (y, name(y))).collect()),
        };
        let mut without_x = 0;
        for (index, row) in rows.iter().take(limit).enumerate() {
            let x_text = row.get(choice.x).and_then(Option::as_deref);
            match x_text.and_then(|text| placement.read(text)) {
                Some(x) => reader.add(index, row, x),
                None => without_x += 1,
            }
        }
        let Reader {
            mut points,
            names,
            left_out,
            repeated,
            ..
        } = reader;
        if choice.kind == ChartKind::Lines && placement != Placement::Categories {
            points.sort_by(|a, b| a.x.position().total_cmp(&b.x.position()));
        }
        let series = names
            .into_iter()
            .enumerate()
            .map(|(position, name)| {
                let cell = |point: &Point| point.cells.get(position).copied().flatten();
                Series {
                    name,
                    values: points
                        .iter()
                        .map(|point| cell(point).and_then(|cell| cell.value))
                        .collect(),
                    rows: points
                        .iter()
                        .map(|point| cell(point).map(|cell| cell.row))
                        .collect(),
                }
            })
            .collect();
        Self {
            kind: choice.kind,
            x_name: name(choice.x),
            x: x_values(&points),
            series,
            split_by: choice.series.map(name),
            rows: points.iter().map(|point| point.row).collect(),
            stacked: choice.stacked(),
            summary: Summary {
                rows: rows.len(),
                limit: (rows.len() > limit).then_some(limit),
                without_x,
                series_left_out: left_out,
                repeated,
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

/// Reads rows into points: a point per row, each Y column a series; or, split by a column, a
/// point per X value, each value of the series column a series.
struct Reader {
    shape: Shape,
    points: Vec<Point>,
    /// The series' names, in the order their cells are kept in.
    names: Vec<String>,
    left_out: Vec<String>,
    repeated: usize,
}

enum Shape {
    /// Each Y column, by its index, is a series.
    Columns(Vec<usize>),
    Split {
        series: usize,
        y: Option<usize>,
        /// Where each X value's point is.
        points: HashMap<XKey, usize>,
    },
}

impl Reader {
    fn columns(ys: Vec<(usize, String)>) -> Self {
        let (ys, names) = ys.into_iter().unzip();
        Self {
            shape: Shape::Columns(ys),
            points: Vec::new(),
            names,
            left_out: Vec::new(),
            repeated: 0,
        }
    }

    fn split(series: usize, y: Option<usize>) -> Self {
        Self {
            shape: Shape::Split {
                series,
                y,
                points: HashMap::new(),
            },
            points: Vec::new(),
            names: Vec::new(),
            left_out: Vec::new(),
            repeated: 0,
        }
    }

    fn add(&mut self, index: usize, row: &Row, x: X) {
        let number = |column: usize| {
            row.get(column)
                .and_then(Option::as_deref)
                .and_then(values::number)
        };
        match &mut self.shape {
            Shape::Columns(ys) => {
                let cells = ys
                    .iter()
                    .map(|&y| {
                        Some(Cell {
                            value: number(y),
                            row: index,
                        })
                    })
                    .collect();
                self.points.push(Point {
                    row: index,
                    x,
                    cells,
                });
            }
            Shape::Split { series, y, points } => {
                let name = row
                    .get(*series)
                    .and_then(Option::as_deref)
                    .unwrap_or(NULL_SERIES);
                let Some(position) = series_position(&mut self.names, &mut self.left_out, name)
                else {
                    return;
                };
                let point = *points.entry(x.key()).or_insert_with(|| {
                    self.points.push(Point {
                        row: index,
                        x,
                        cells: Vec::new(),
                    });
                    self.points.len() - 1
                });
                let Some(point) = self.points.get_mut(point) else {
                    return;
                };
                if point.cells.len() <= position {
                    point.cells.resize(position + 1, None);
                }
                match &mut point.cells[position] {
                    Some(_) => self.repeated += 1,
                    empty @ None => {
                        *empty = Some(Cell {
                            value: y.and_then(number),
                            row: index,
                        });
                    }
                }
            }
        }
    }
}

/// Where `name` is among the series, added if it is new and there is a colour left for it;
/// `None`, and `name` noted in `left_out`, when there isn't.
fn series_position(
    names: &mut Vec<String>,
    left_out: &mut Vec<String>,
    name: &str,
) -> Option<usize> {
    if let Some(position) = names.iter().position(|known| known == name) {
        return Some(position);
    }
    if names.len() < STARTING_SERIES {
        names.push(name.to_owned());
        return Some(names.len() - 1);
    }
    if !left_out.iter().any(|known| known == name) {
        left_out.push(name.to_owned());
    }
    None
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
    /// Each series' value here, by the series' position; missing or `None` where it has no row.
    cells: Vec<Option<Cell>>,
}

/// A series' value at a point, and the row it was read from.
#[derive(Debug, Clone, Copy)]
struct Cell {
    value: Option<f64>,
    row: usize,
}

/// An X value, to find a point by: its text as a category, or its position on the axis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum XKey {
    Category(String),
    Position(u64),
}

enum X {
    Category(String),
    Number(f64),
    Time(WrittenTime),
}

impl X {
    fn key(&self) -> XKey {
        match self {
            Self::Category(name) => XKey::Category(name.clone()),
            Self::Number(_) | Self::Time(_) => XKey::Position(self.position().to_bits()),
        }
    }

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
        let choice = Choice::of(ChartKind::Bars, 0, vec![1]);
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
        let choice = Choice::of(ChartKind::Lines, 0, vec![1]);
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
        let choice = Choice::of(ChartKind::Scatter, 0, vec![1]);
        let data = read(&[Role::Number, Role::Number], &rows, &choice);
        assert_eq!(data.x, XValues::Numbers(vec![3.0, 1.0]));
    }

    #[test]
    fn times_are_seen_in_the_first_offset_named() {
        let rows = rows(&[
            &[Some("2026-03-01 00:00:00+01"), Some("1")],
            &[Some("2026-03-01 00:00:00+00"), Some("2")],
        ]);
        let choice = Choice::of(ChartKind::Lines, 0, vec![1]);
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
        let bars = Choice::of(ChartKind::Bars, 0, vec![1]);
        let data = read(&[Role::Text, Role::Number], &rows, &bars);
        assert_eq!(data.len(), 2);
        assert_eq!(data.summary.without_x, 1);

        let lines = Choice::of(ChartKind::Lines, 0, vec![1]);
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
        let choice = Choice::of(ChartKind::Bars, 0, vec![1]);
        let data = read(&[Role::Text, Role::Number], &rows, &choice);
        assert_eq!(data.series[0].values, vec![None, None, Some(4.0)]);
        assert_eq!(data.series[0].gaps(), 2);
    }

    #[test]
    fn bars_stop_at_their_limit() {
        let many: Vec<Row> = (0..MAX_BARS + 10)
            .map(|index| vec![Some(index.to_string()), Some("1".to_owned())])
            .collect();
        let choice = Choice::of(ChartKind::Bars, 0, vec![1]);
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

    fn split(kind: ChartKind) -> Choice {
        Choice {
            series: Some(1),
            ..Choice::of(kind, 0, vec![2])
        }
    }

    #[test]
    fn a_series_column_splits_one_y_in_first_seen_order() {
        let rows = rows(&[
            &[Some("2026-09"), Some("bikes"), Some("10")],
            &[Some("2026-09"), Some("parts"), Some("3")],
            &[Some("2026-10"), Some("parts"), Some("4")],
            &[Some("2026-10"), Some("bikes"), None],
            &[Some("2026-11"), Some("bikes"), Some("12")],
        ]);
        let data = read(
            &[Role::Text, Role::Text, Role::Number],
            &rows,
            &split(ChartKind::Bars),
        );
        assert_eq!(
            data.x,
            XValues::Categories(vec![
                "2026-09".to_owned(),
                "2026-10".to_owned(),
                "2026-11".to_owned()
            ])
        );
        let named: Vec<(&str, &[Option<f64>])> = data
            .series
            .iter()
            .map(|series| (series.name.as_str(), series.values.as_slice()))
            .collect();
        assert_eq!(
            named,
            vec![
                ("bikes", &[Some(10.0), None, Some(12.0)][..]),
                ("parts", &[Some(3.0), Some(4.0), None][..]),
            ]
        );
        assert_eq!(data.series[0].rows, vec![Some(0), Some(3), Some(4)]);
        assert_eq!(data.series[1].rows, vec![Some(1), Some(2), None]);
        assert_eq!(data.series[1].gaps(), 1, "no row is a gap");
        assert_eq!(data.rows, vec![0, 2, 4]);
    }

    #[test]
    fn a_split_keeps_eight_series_and_the_first_row_of_each() {
        let mut values: Vec<Vec<Option<String>>> = (0..10)
            .map(|index| {
                vec![
                    Some("2026-09".to_owned()),
                    Some(format!("s{index}")),
                    Some("1".to_owned()),
                ]
            })
            .collect();
        values.push(vec![
            Some("2026-09".to_owned()),
            Some("s0".to_owned()),
            Some("9".to_owned()),
        ]);
        values.push(vec![Some("2026-09".to_owned()), None, Some("2".to_owned())]);
        let data = read(
            &[Role::Text, Role::Text, Role::Number],
            &values,
            &split(ChartKind::Bars),
        );
        assert_eq!(data.series.len(), STARTING_SERIES);
        assert_eq!(data.series[0].values, vec![Some(1.0)], "the first row wins");
        assert_eq!(
            data.summary.series_left_out,
            vec!["s8".to_owned(), "s9".to_owned(), NULL_SERIES.to_owned()]
        );
        assert_eq!(data.summary.repeated, 1);
    }

    #[test]
    fn split_lines_over_times_meet_at_each_time() {
        let rows = rows(&[
            &[Some("2026-10-01"), Some("a"), Some("2")],
            &[Some("2026-09-01"), Some("a"), Some("1")],
            &[Some("2026-09-01"), Some("b"), Some("5")],
        ]);
        let data = read(
            &[Role::Time, Role::Text, Role::Number],
            &rows,
            &split(ChartKind::Lines),
        );
        assert_eq!(data.len(), 2);
        assert_eq!(data.series[0].values, vec![Some(1.0), Some(2.0)]);
        assert_eq!(data.series[1].values, vec![Some(5.0), None]);
    }

    #[test]
    fn text_on_x_is_placed_as_categories_whatever_the_kind() {
        let rows = rows(&[&[Some("b"), Some("1")], &[Some("a"), Some("2")]]);
        let choice = Choice::of(ChartKind::Lines, 0, vec![1]);
        let data = read(&[Role::Text, Role::Number], &rows, &choice);
        assert_eq!(
            data.x,
            XValues::Categories(vec!["b".to_owned(), "a".to_owned()])
        );
    }
}
