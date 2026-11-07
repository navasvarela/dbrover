//! Laying a chart out: from its points and a size to the marks that draw it, in points, the
//! same marks whether they are painted in the tab or written to a file.

use crate::ChartKind;
use crate::data::{ChartData, XValues};
use crate::ticks::Ticks;
use crate::time::TimeTicks;

/// Room around everything, and between a label and what it labels.
const PADDING: f32 = 8.0;
const LABEL_GAP: f32 = 6.0;
/// About one tick per this many points up the side, and along the bottom for numbers and times,
/// whose labels are wider.
const Y_TICK_SPACING: f32 = 48.0;
const X_TICK_SPACING: f32 = 90.0;
/// How far points sit in from the plot's sides on axes placed by value, so a point at either
/// end isn't cut in half.
const INSET: f32 = 8.0;
/// The share of a category's slot its bars fill; the rest is the gap between categories.
const BAR_GROUP_SHARE: f32 = 0.8;
/// Below this width a bar has no gap beside the next one, which would hide it.
const MIN_BAR_GAP_WIDTH: f32 = 4.0;
/// A dot is 8 points across, big enough to see and to point at.
pub const POINT_RADIUS: f32 = 4.0;
/// Lines show a dot on each point while they have this many or fewer, to show where the values
/// are; past it the dots would merge into the line.
const MAX_DOTTED_POINTS: usize = 60;
/// How near a scatter's point the pointer must be to hover it.
const HOVER_DISTANCE: f32 = 10.0;
/// Categories' labels are cut to this many characters, with an ellipsis.
const MAX_LABEL_CHARS: usize = 20;
/// A legend's colour square.
const SWATCH: f32 = 10.0;
/// Smaller than this, a plot has no room for its points, and nothing is drawn.
const MIN_PLOT_SIDE: f32 = 30.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Rect {
    /// The rectangle between two corners, whichever they are.
    pub fn spanning(a: Point, b: Point) -> Self {
        Self {
            min: Point::new(a.x.min(b.x), a.y.min(b.y)),
            max: Point::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn contains(&self, point: Point) -> bool {
        (self.min.x..=self.max.x).contains(&point.x) && (self.min.y..=self.max.y).contains(&point.y)
    }
}

/// What a mark is coloured as. The painter picks the colours, so one layout serves the dark
/// theme on screen and the light one in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    /// The series at this index among the chart's.
    Series(usize),
    /// The axis line, and the zero line when values go below it.
    Axis,
    /// Lines across the plot at each tick.
    Grid,
    /// Ticks' labels and the legend's names.
    Label,
    /// The columns' names.
    Title,
}

/// Which point of a text its position is: its start, middle or end, at its vertical middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Rect {
        rect: Rect,
        paint: Paint,
        /// Its corners may be rounded: a bar standing alone, not a piece of a stack, whose
        /// rounded corners would leave notches between the pieces.
        rounded: bool,
    },
    Line {
        from: Point,
        to: Point,
        paint: Paint,
    },
    Polyline {
        points: Vec<Point>,
        paint: Paint,
    },
    Circle {
        centre: Point,
        radius: f32,
        paint: Paint,
    },
    Text {
        at: Point,
        text: String,
        anchor: Anchor,
        paint: Paint,
    },
}

/// How wide a text is, and how tall a line of it, in the one size every label uses: the tab
/// measures with its font, tests and files with an estimate.
pub trait TextMeasure {
    fn width(&self, text: &str) -> f32;
    fn line_height(&self) -> f32;
}

/// What a chart spec sets of its look: titles, and the Y axis's start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutOptions {
    /// A line above the chart.
    pub title: Option<String>,
    /// In place of the X column's name under the axis.
    pub x_title: Option<String>,
    /// In place of the Y column's name above the plot, or before the legend of several.
    pub y_title: Option<String>,
    /// Start the Y axis at zero for lines and scatters, as bars always do.
    pub y_zero: bool,
}

/// A chart laid out: its marks in drawing order, and where each point went, for hovering.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    pub size: Size,
    /// Where the points are drawn, inside the axes.
    pub plot: Rect,
    pub marks: Vec<Mark>,
    kind: ChartKind,
    /// Categories were placed in slots of this width; `None` when placed by value.
    slot: Option<f32>,
    /// Each point's X, and its Y in each series, in points.
    xs: Vec<f32>,
    ys: Vec<Vec<Option<f32>>>,
}

impl Drawing {
    /// The point under `at`: the category it is over, on lines the nearest point along the
    /// bottom, on a scatter the nearest point within reach.
    pub fn hit(&self, at: Point) -> Option<usize> {
        if !self.plot.contains(at) || self.xs.is_empty() {
            return None;
        }
        if let Some(slot) = self.slot {
            let index = ((at.x - self.plot.min.x) / slot).floor() as usize;
            return (index < self.xs.len()).then_some(index);
        }
        match self.kind {
            ChartKind::Scatter => self.nearest_point(at),
            ChartKind::Bars | ChartKind::Lines => self.nearest_x(at.x),
        }
    }

    /// The X, in points, of the point at `index`.
    pub fn x_of(&self, index: usize) -> Option<f32> {
        self.xs.get(index).copied()
    }

    fn nearest_x(&self, x: f32) -> Option<usize> {
        self.xs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (*a - x).abs().total_cmp(&(*b - x).abs()))
            .map(|(index, _)| index)
    }

    fn nearest_point(&self, at: Point) -> Option<usize> {
        let distance = |index: usize, y: f32| {
            let x = self.xs.get(index).copied().unwrap_or(f32::INFINITY);
            (x - at.x).hypot(y - at.y)
        };
        self.ys
            .iter()
            .flat_map(|series| series.iter().enumerate())
            .filter_map(|(index, y)| y.map(|y| (index, distance(index, y))))
            .filter(|(_, distance)| *distance <= HOVER_DISTANCE)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }
}

/// Lays `data` out in `size` as `options` say, measuring text with `measure`.
pub fn layout(
    data: &ChartData,
    options: &LayoutOptions,
    size: Size,
    measure: &impl TextMeasure,
) -> Drawing {
    let line = measure.line_height();
    let title_room = if options.title.is_some() {
        line + LABEL_GAP
    } else {
        0.0
    };
    let top = PADDING + title_room + line + LABEL_GAP;
    let bottom = size.height - PADDING - line - LABEL_GAP - line - LABEL_GAP;
    let from_zero = options.y_zero || data.kind == ChartKind::Bars;
    let y_ticks = Ticks::nice(
        y_low(data, from_zero),
        y_high(data, from_zero),
        ((bottom - top) / Y_TICK_SPACING) as usize,
    );
    let y_labels: Vec<String> = y_ticks.values.iter().map(|v| y_ticks.label(*v)).collect();
    let widest = y_labels
        .iter()
        .map(|label| measure.width(label))
        .fold(0.0, f32::max);
    let plot = Rect {
        min: Point::new(PADDING + widest + LABEL_GAP, top),
        max: Point::new(size.width - PADDING, bottom),
    };
    let mut drawing = Drawing {
        size,
        plot,
        marks: Vec::new(),
        kind: data.kind,
        slot: None,
        xs: Vec::new(),
        ys: Vec::new(),
    };
    if plot.width() < MIN_PLOT_SIDE || plot.height() < MIN_PLOT_SIDE {
        return drawing;
    }
    let y_scale = Scale::new(y_ticks.low, y_ticks.high, plot.max.y, plot.min.y);
    y_axis(&mut drawing.marks, plot, &y_ticks, &y_labels, y_scale);
    x_axis(&mut drawing, data, measure);
    drawing.ys = data
        .series
        .iter()
        .map(|series| {
            series
                .values
                .iter()
                .map(|value| value.map(|value| y_scale.at(value)))
                .collect()
        })
        .collect();
    series_marks(&mut drawing, data, y_scale);
    titles(&mut drawing.marks, data, options, size, measure, plot);
    drawing
}

/// Lays `data` out for a file, as [`layout`] does, with `note`, the line under the chart, in its
/// last line. A chart with no points says `note` in place of its marks, under its title.
pub fn layout_file(
    data: &ChartData,
    options: &LayoutOptions,
    size: Size,
    measure: &impl TextMeasure,
    note: &str,
) -> Drawing {
    let line = measure.line_height();
    if data.is_empty() {
        return without_points(data.kind, options, size, line, note);
    }
    let chart = Size {
        height: size.height - line - LABEL_GAP,
        ..size
    };
    let mut drawing = layout(data, options, chart, measure);
    drawing.size = size;
    drawing.marks.push(Mark::Text {
        at: Point::new(PADDING, size.height - PADDING - line / 2.0),
        text: note.to_owned(),
        anchor: Anchor::Start,
        paint: Paint::Label,
    });
    drawing
}

/// A file's chart with nothing to draw: its title, and `note` where the plot would be.
fn without_points(
    kind: ChartKind,
    options: &LayoutOptions,
    size: Size,
    line: f32,
    note: &str,
) -> Drawing {
    let mut marks = Vec::new();
    if let Some(title) = &options.title {
        marks.push(Mark::Text {
            at: Point::new(size.width / 2.0, PADDING + line / 2.0),
            text: title.clone(),
            anchor: Anchor::Middle,
            paint: Paint::Title,
        });
    }
    let middle = Point::new(size.width / 2.0, size.height / 2.0);
    marks.push(Mark::Text {
        at: middle,
        text: note.to_owned(),
        anchor: Anchor::Middle,
        paint: Paint::Label,
    });
    Drawing {
        size,
        plot: Rect {
            min: middle,
            max: middle,
        },
        marks,
        kind,
        slot: None,
        xs: Vec::new(),
        ys: Vec::new(),
    }
}

/// A linear map from values to points.
#[derive(Debug, Clone, Copy)]
struct Scale {
    low: f64,
    high: f64,
    from: f32,
    to: f32,
}

impl Scale {
    fn new(low: f64, high: f64, from: f32, to: f32) -> Self {
        Self {
            low,
            high,
            from,
            to,
        }
    }

    fn at(&self, value: f64) -> f32 {
        let span = self.high - self.low;
        let share = if span > 0.0 {
            (value - self.low) / span
        } else {
            0.5
        };
        self.from + (self.to - self.from) * share as f32
    }
}

fn values(data: &ChartData) -> impl Iterator<Item = f64> + '_ {
    data.series
        .iter()
        .flat_map(|series| series.values.iter().flatten().copied())
}

/// The values the Y axis must reach: each value, or for stacked bars the top and bottom of each
/// stack.
fn reach(data: &ChartData) -> Vec<f64> {
    if !data.stacked {
        return values(data).collect();
    }
    (0..data.len())
        .flat_map(|point| {
            let (up, down) = stack_ends(data, point);
            [up, down]
        })
        .collect()
}

/// How far a stack's pieces reach at `point`: the positive values summed up from zero, and the
/// negative ones down from it.
fn stack_ends(data: &ChartData, point: usize) -> (f64, f64) {
    data.series
        .iter()
        .filter_map(|series| series.values.get(point).copied().flatten())
        .fold((0.0, 0.0), |(up, down), value| {
            if value >= 0.0 {
                (up + value, down)
            } else {
                (up, down + value)
            }
        })
}

/// The bottom of the Y axis: the lowest value, and never above zero `from_zero`, as for bars,
/// which grow from it.
fn y_low(data: &ChartData, from_zero: bool) -> f64 {
    let low = reach(data).into_iter().fold(f64::INFINITY, f64::min);
    match (from_zero, low.is_finite()) {
        (_, false) => 0.0,
        (true, true) => low.min(0.0),
        (false, true) => low,
    }
}

fn y_high(data: &ChartData, from_zero: bool) -> f64 {
    let high = reach(data).into_iter().fold(f64::NEG_INFINITY, f64::max);
    match (from_zero, high.is_finite()) {
        (_, false) => 1.0,
        (true, true) => high.max(0.0),
        (false, true) => high,
    }
}

fn y_axis(marks: &mut Vec<Mark>, plot: Rect, ticks: &Ticks, labels: &[String], scale: Scale) {
    for (value, label) in ticks.values.iter().zip(labels) {
        let y = scale.at(*value);
        marks.push(Mark::Line {
            from: Point::new(plot.min.x, y),
            to: Point::new(plot.max.x, y),
            paint: Paint::Grid,
        });
        marks.push(Mark::Text {
            at: Point::new(plot.min.x - LABEL_GAP, y),
            text: label.clone(),
            anchor: Anchor::End,
            paint: Paint::Label,
        });
    }
    if ticks.low < 0.0 && ticks.high > 0.0 {
        let zero = scale.at(0.0);
        marks.push(Mark::Line {
            from: Point::new(plot.min.x, zero),
            to: Point::new(plot.max.x, zero),
            paint: Paint::Axis,
        });
    }
    marks.push(Mark::Line {
        from: Point::new(plot.min.x, plot.max.y),
        to: Point::new(plot.max.x, plot.max.y),
        paint: Paint::Axis,
    });
}

/// Places each point along the bottom, and labels the axis.
fn x_axis(drawing: &mut Drawing, data: &ChartData, measure: &impl TextMeasure) {
    let plot = drawing.plot;
    let label_y = plot.max.y + LABEL_GAP + measure.line_height() / 2.0;
    let target = (plot.width() / X_TICK_SPACING) as usize;
    let inner = Scale::new(0.0, 1.0, plot.min.x + INSET, plot.max.x - INSET);
    let mut label = |x: f32, text: String| {
        drawing.marks.push(Mark::Text {
            at: Point::new(x, label_y),
            text,
            anchor: Anchor::Middle,
            paint: Paint::Label,
        });
    };
    match &data.x {
        XValues::Categories(names) => {
            let slot = plot.width() / names.len().max(1) as f32;
            let names: Vec<String> = names.iter().map(|name| shortened(name)).collect();
            let widest = names
                .iter()
                .map(|name| measure.width(name))
                .fold(0.0, f32::max);
            let every = ((widest + LABEL_GAP) / slot).ceil().max(1.0) as usize;
            let centre = |index: usize| plot.min.x + slot * (index as f32 + 0.5);
            for (index, name) in names.into_iter().enumerate().step_by(every) {
                label(centre(index), name);
            }
            drawing.slot = Some(slot);
            drawing.xs = (0..data.len()).map(centre).collect();
        }
        XValues::Numbers(numbers) => {
            let (low, high) = range(numbers);
            let ticks = Ticks::nice(low, high, target);
            let scale = Scale::new(ticks.low, ticks.high, inner.at(0.0), inner.at(1.0));
            for value in &ticks.values {
                label(scale.at(*value), ticks.label(*value));
            }
            drawing.xs = numbers.iter().map(|number| scale.at(*number)).collect();
        }
        XValues::Times(times) => {
            let (low, high) = range(&times.seconds);
            let (low, high) = if high > low {
                (low, high)
            } else {
                let day = 86_400.0;
                (low - day, high + day)
            };
            let ticks = TimeTicks::new(low, high, target, !times.has_time);
            let scale = Scale::new(low, high, inner.at(0.0), inner.at(1.0));
            for value in &ticks.values {
                label(scale.at(*value), ticks.label(*value));
            }
            drawing.xs = times.seconds.iter().map(|time| scale.at(*time)).collect();
        }
    }
}

fn range(values: &[f64]) -> (f64, f64) {
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if low.is_finite() && high.is_finite() {
        (low, high)
    } else {
        (0.0, 1.0)
    }
}

fn shortened(name: &str) -> String {
    if name.chars().count() <= MAX_LABEL_CHARS {
        return name.to_owned();
    }
    let kept: String = name.chars().take(MAX_LABEL_CHARS - 1).collect();
    format!("{kept}…")
}

fn series_marks(drawing: &mut Drawing, data: &ChartData, y_scale: Scale) {
    match data.kind {
        ChartKind::Bars if data.stacked => stacked_bars(drawing, data, y_scale),
        ChartKind::Bars => bars(drawing, y_scale),
        ChartKind::Lines => lines(drawing),
        ChartKind::Scatter => dots(drawing),
    }
}

fn bars(drawing: &mut Drawing, y_scale: Scale) {
    let slot = drawing.slot.unwrap_or_default();
    let group = slot * BAR_GROUP_SHARE;
    let width = group / drawing.ys.len().max(1) as f32;
    let gap = if width >= MIN_BAR_GAP_WIDTH { 1.0 } else { 0.0 };
    let zero = y_scale.at(0.0);
    for (series, ys) in drawing.ys.iter().enumerate() {
        for (index, y) in ys.iter().enumerate() {
            let (Some(y), Some(centre)) = (y, drawing.xs.get(index)) else {
                continue;
            };
            let left = centre - group / 2.0 + width * series as f32;
            drawing.marks.push(Mark::Rect {
                rect: Rect::spanning(Point::new(left, zero), Point::new(left + width - gap, *y)),
                paint: Paint::Series(series),
                rounded: true,
            });
        }
    }
}

/// A bar per point as wide as a group of bars, its series' values stacked on each other in the
/// series' order: positive values up from zero, negative ones down from it.
fn stacked_bars(drawing: &mut Drawing, data: &ChartData, y_scale: Scale) {
    let width = drawing.slot.unwrap_or_default() * BAR_GROUP_SHARE;
    for (point, centre) in drawing.xs.iter().enumerate() {
        let left = centre - width / 2.0;
        let (mut up, mut down) = (0.0, 0.0);
        for (series, values) in data.series.iter().enumerate() {
            let Some(value) = values.values.get(point).copied().flatten() else {
                continue;
            };
            let end = if value >= 0.0 { &mut up } else { &mut down };
            let from = *end;
            *end += value;
            drawing.marks.push(Mark::Rect {
                rect: Rect::spanning(
                    Point::new(left, y_scale.at(from)),
                    Point::new(left + width, y_scale.at(*end)),
                ),
                paint: Paint::Series(series),
                rounded: false,
            });
        }
    }
}

/// A line per series, broken where a value is missing; a point on its own between two gaps is
/// a dot, since a line needs two.
fn lines(drawing: &mut Drawing) {
    let dotted = drawing.xs.len() <= MAX_DOTTED_POINTS;
    for (series, ys) in drawing.ys.iter().enumerate() {
        let paint = Paint::Series(series);
        let runs = ys
            .iter()
            .zip(&drawing.xs)
            .map(|(y, x)| y.map(|y| Point::new(*x, y)))
            .collect::<Vec<_>>();
        for run in runs.split(Option::is_none) {
            let points: Vec<Point> = run.iter().flatten().copied().collect();
            if points.len() > 1 {
                drawing.marks.push(Mark::Polyline {
                    points: points.clone(),
                    paint,
                });
            }
            if dotted || points.len() == 1 {
                drawing
                    .marks
                    .extend(points.into_iter().map(|centre| Mark::Circle {
                        centre,
                        radius: POINT_RADIUS,
                        paint,
                    }));
            }
        }
    }
}

fn dots(drawing: &mut Drawing) {
    for (series, ys) in drawing.ys.iter().enumerate() {
        for (y, x) in ys.iter().zip(&drawing.xs) {
            if let Some(y) = y {
                drawing.marks.push(Mark::Circle {
                    centre: Point::new(*x, *y),
                    radius: POINT_RADIUS,
                    paint: Paint::Series(series),
                });
            }
        }
    }
}

/// The title above everything; the X column's name, or `x_title`, under the axis; and above the
/// plot the Y column's name, or `y_title`, then a legend when there are several series.
fn titles(
    marks: &mut Vec<Mark>,
    data: &ChartData,
    options: &LayoutOptions,
    size: Size,
    measure: &impl TextMeasure,
    plot: Rect,
) {
    let line = measure.line_height();
    let mut y = PADDING + line / 2.0;
    if let Some(title) = &options.title {
        marks.push(Mark::Text {
            at: Point::new(size.width / 2.0, y),
            text: title.clone(),
            anchor: Anchor::Middle,
            paint: Paint::Title,
        });
        y += line + LABEL_GAP;
    }
    marks.push(Mark::Text {
        at: Point::new(
            (plot.min.x + plot.max.x) / 2.0,
            size.height - PADDING - line / 2.0,
        ),
        text: options
            .x_title
            .clone()
            .unwrap_or_else(|| data.x_name.clone()),
        anchor: Anchor::Middle,
        paint: Paint::Title,
    });
    let mut x = plot.min.x;
    let y_name = match (&options.y_title, data.series.as_slice()) {
        (Some(title), _) => Some(title.clone()),
        (None, [only]) => Some(only.name.clone()),
        (None, _) => None,
    };
    if let Some(name) = y_name {
        x += measure.width(&name) + 2.0 * LABEL_GAP;
        marks.push(Mark::Text {
            at: Point::new(plot.min.x, y),
            text: name,
            anchor: Anchor::Start,
            paint: Paint::Title,
        });
    }
    if data.series.len() > 1 {
        legend(marks, data, x, y, plot.max.x, measure);
    }
}

/// A swatch and a name for each series, from `x` along the line at `y`, cut with an ellipsis
/// at `right`.
fn legend(
    marks: &mut Vec<Mark>,
    data: &ChartData,
    mut x: f32,
    y: f32,
    right: f32,
    measure: &impl TextMeasure,
) {
    for (index, series) in data.series.iter().enumerate() {
        let width = SWATCH + LABEL_GAP + measure.width(&series.name);
        if x + width > right {
            marks.push(Mark::Text {
                at: Point::new(x, y),
                text: "…".to_owned(),
                anchor: Anchor::Start,
                paint: Paint::Label,
            });
            return;
        }
        marks.push(Mark::Rect {
            rect: Rect::spanning(
                Point::new(x, y - SWATCH / 2.0),
                Point::new(x + SWATCH, y + SWATCH / 2.0),
            ),
            paint: Paint::Series(index),
            rounded: true,
        });
        marks.push(Mark::Text {
            at: Point::new(x + SWATCH + LABEL_GAP / 2.0, y),
            text: series.name.clone(),
            anchor: Anchor::Start,
            paint: Paint::Label,
        });
        x += width + 2.0 * LABEL_GAP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Series, Summary, Times};

    /// Every character 7 points wide, lines 14 tall: near enough Inter at 11 points.
    struct Fixed;

    impl TextMeasure for Fixed {
        fn width(&self, text: &str) -> f32 {
            text.chars().count() as f32 * 7.0
        }

        fn line_height(&self) -> f32 {
            14.0
        }
    }

    const SIZE: Size = Size {
        width: 600.0,
        height: 300.0,
    };

    fn data(kind: ChartKind, x: XValues, series: &[(&str, &[Option<f64>])]) -> ChartData {
        let len = match &x {
            XValues::Categories(names) => names.len(),
            XValues::Numbers(numbers) => numbers.len(),
            XValues::Times(times) => times.seconds.len(),
        };
        ChartData {
            kind,
            x_name: "month".to_owned(),
            x,
            series: series
                .iter()
                .map(|(name, values)| Series {
                    name: (*name).to_owned(),
                    values: values.to_vec(),
                    rows: (0..values.len()).map(Some).collect(),
                })
                .collect(),
            split_by: None,
            rows: (0..len).collect(),
            stacked: false,
            summary: Summary {
                rows: len,
                ..Summary::default()
            },
        }
    }

    fn months(count: usize) -> XValues {
        XValues::Categories(
            (1..=count)
                .map(|month| format!("2026-{month:02}"))
                .collect(),
        )
    }

    /// The bars: rectangles in a series' colour inside the plot, which the legend's swatches
    /// aren't.
    fn rects(drawing: &Drawing) -> Vec<(Rect, Paint)> {
        drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Rect {
                    rect,
                    paint: paint @ Paint::Series(_),
                    ..
                } if rect.min.y >= drawing.plot.min.y => Some((*rect, *paint)),
                _ => None,
            })
            .collect()
    }

    fn texts(drawing: &Drawing, wanted: Paint) -> Vec<String> {
        drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Text { text, paint, .. } if *paint == wanted => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bars_grow_from_zero_in_their_slots() {
        let chart = data(
            ChartKind::Bars,
            months(3),
            &[("total", &[Some(10.0), Some(30.0), Some(20.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert_eq!(bars.len(), 3);
        let bottom = drawing.plot.max.y;
        for (rect, _) in &bars {
            assert!(
                (rect.max.y - bottom).abs() < 0.01,
                "{rect:?} starts at zero"
            );
        }
        assert!(bars[1].0.height() > bars[2].0.height());
        assert!(bars[2].0.height() > bars[0].0.height());
        assert!(bars[0].0.max.x < bars[1].0.min.x);
        assert_eq!(texts(&drawing, Paint::Label)[..1], ["0".to_owned()]);
        assert_eq!(texts(&drawing, Paint::Title), ["month", "total"]);
    }

    #[test]
    fn negative_bars_hang_from_the_zero_line() {
        let chart = data(
            ChartKind::Bars,
            months(2),
            &[("net", &[Some(-5.0), Some(5.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert!((bars[0].0.min.y - bars[1].0.max.y).abs() < 0.01);
        assert!(drawing.marks.iter().any(|mark| matches!(
            mark,
            Mark::Line { from, paint: Paint::Axis, .. } if (from.y - bars[0].0.min.y).abs() < 0.01
        )));
    }

    #[test]
    fn several_series_sit_side_by_side_with_a_legend() {
        let chart = data(
            ChartKind::Bars,
            months(2),
            &[
                ("orders", &[Some(1.0), Some(2.0)]),
                ("refunds", &[Some(1.0), None]),
            ],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert_eq!(bars.len(), 3, "a gap draws no bar");
        let (first, second) = (bars[0], bars[2]);
        assert_eq!(first.1, Paint::Series(0));
        assert_eq!(second.1, Paint::Series(1));
        assert!(first.0.max.x <= second.0.min.x);
        let legend = texts(&drawing, Paint::Label);
        assert!(legend.contains(&"orders".to_owned()) && legend.contains(&"refunds".to_owned()));
    }

    #[test]
    fn crowded_category_labels_are_thinned() {
        let chart = data(ChartKind::Bars, months(12), &[("total", &[Some(1.0); 12])]);
        let narrow = Size {
            width: 300.0,
            height: 300.0,
        };
        let drawing = layout(&chart, &LayoutOptions::default(), narrow, &Fixed);
        let labels: Vec<String> = texts(&drawing, Paint::Label)
            .into_iter()
            .filter(|label| label.starts_with("2026"))
            .collect();
        assert!(labels.len() < 12 && labels.len() > 1, "{labels:?}");
        assert_eq!(labels[0], "2026-01");
    }

    #[test]
    fn lines_break_at_gaps() {
        let chart = data(
            ChartKind::Lines,
            XValues::Numbers(vec![1.0, 2.0, 3.0, 4.0, 5.0]),
            &[("total", &[Some(1.0), Some(2.0), None, Some(4.0), Some(5.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let lines: Vec<usize> = drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Polyline { points, .. } => Some(points.len()),
                _ => None,
            })
            .collect();
        assert_eq!(lines, vec![2, 2]);
    }

    #[test]
    fn lines_over_time_label_their_ticks_as_dates() {
        let day = 86_400.0;
        let start = 1_767_225_600.0; // 2026-01-01
        let chart = data(
            ChartKind::Lines,
            XValues::Times(Times {
                seconds: (0..4)
                    .map(|month| start + f64::from(month) * 31.0 * day)
                    .collect(),
                offset: None,
                has_time: false,
            }),
            &[("total", &[Some(1.0), Some(3.0), Some(2.0), Some(4.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let labels = texts(&drawing, Paint::Label);
        assert!(labels.contains(&"Jan 2026".to_owned()), "{labels:?}");
    }

    #[test]
    fn hovering_finds_the_point_under_the_pointer() {
        let bars = layout(
            &data(ChartKind::Bars, months(4), &[("total", &[Some(1.0); 4])]),
            &LayoutOptions::default(),
            SIZE,
            &Fixed,
        );
        let plot = bars.plot;
        let third = Point::new(plot.min.x + plot.width() * 0.6, plot.max.y - 1.0);
        assert_eq!(bars.hit(third), Some(2));
        assert_eq!(bars.hit(Point::new(1.0, 1.0)), None);

        let scatter = layout(
            &data(
                ChartKind::Scatter,
                XValues::Numbers(vec![0.0, 10.0]),
                &[("total", &[Some(0.0), Some(10.0)])],
            ),
            &LayoutOptions::default(),
            SIZE,
            &Fixed,
        );
        let second = Point::new(scatter.x_of(1).unwrap(), scatter.ys[0][1].unwrap());
        assert_eq!(scatter.hit(second), Some(1));
        let far = Point::new(
            (scatter.plot.min.x + scatter.plot.max.x) / 2.0,
            scatter.plot.min.y + 2.0,
        );
        assert_eq!(scatter.hit(far), None);
    }

    #[test]
    fn a_tiny_chart_draws_nothing() {
        let chart = data(
            ChartKind::Bars,
            months(2),
            &[("total", &[Some(1.0), Some(2.0)])],
        );
        let drawing = layout(
            &chart,
            &LayoutOptions::default(),
            Size {
                width: 40.0,
                height: 40.0,
            },
            &Fixed,
        );
        assert!(drawing.marks.is_empty());
    }

    #[test]
    fn stacked_bars_pile_up_from_zero_and_hang_below_it() {
        let mut chart = data(
            ChartKind::Bars,
            months(2),
            &[
                ("bikes", &[Some(10.0), Some(-4.0)]),
                ("parts", &[Some(5.0), Some(6.0)]),
                ("clothing", &[None, Some(-2.0)]),
            ],
        );
        chart.stacked = true;
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert_eq!(bars.len(), 5, "a piece per value, none for a gap");
        // Point by point, each point's pieces in the series' order.
        let (bikes, parts) = (bars[0].0, bars[1].0);
        assert!(
            (bikes.min.y - parts.max.y).abs() < 0.01,
            "parts sit on bikes"
        );
        assert!(
            (bikes.min.x - parts.min.x).abs() < 0.01,
            "one bar per month"
        );
        assert!(bikes.height() > parts.height());
        let (negative_bikes, negative_clothing) = (bars[2].0, bars[4].0);
        assert!(
            (negative_bikes.max.y - negative_clothing.min.y).abs() < 0.01,
            "clothing hangs below bikes"
        );
        let labels = texts(&drawing, Paint::Label);
        assert!(
            labels.contains(&"20".to_owned()) && labels.contains(&"-10".to_owned()),
            "the axis reaches 15 up and 6 down: {labels:?}"
        );
        assert!(
            drawing.marks.iter().all(|mark| !matches!(
                mark,
                Mark::Rect { rounded: true, rect, .. } if rect.min.y >= drawing.plot.min.y
            )),
            "stacked pieces aren't rounded"
        );
    }

    #[test]
    fn a_specs_titles_take_the_columns_names_places() {
        let chart = data(
            ChartKind::Bars,
            months(2),
            &[("net", &[Some(1.0), Some(2.0)])],
        );
        let options = LayoutOptions {
            title: Some("Net sales by month".to_owned()),
            x_title: Some("Month".to_owned()),
            y_title: Some("Euros".to_owned()),
            y_zero: false,
        };
        let plain = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let titled = layout(&chart, &options, SIZE, &Fixed);
        assert_eq!(
            texts(&titled, Paint::Title),
            ["Net sales by month", "Month", "Euros"]
        );
        assert!(
            titled.plot.min.y > plain.plot.min.y,
            "the title takes a line above the plot"
        );
    }

    #[test]
    fn a_y_title_comes_before_the_legend_of_several_series() {
        let chart = data(
            ChartKind::Bars,
            months(2),
            &[
                ("orders", &[Some(1.0), Some(2.0)]),
                ("refunds", &[Some(1.0), Some(0.0)]),
            ],
        );
        let options = LayoutOptions {
            y_title: Some("Count".to_owned()),
            ..LayoutOptions::default()
        };
        let drawing = layout(&chart, &options, SIZE, &Fixed);
        let at = |wanted: &str| {
            drawing.marks.iter().find_map(|mark| match mark {
                Mark::Text { at, text, .. } if text == wanted => Some(at.x),
                _ => None,
            })
        };
        let (Some(title), Some(orders)) = (at("Count"), at("orders")) else {
            panic!("expected a Y title and a legend: {:?}", drawing.marks);
        };
        assert!(title < orders);
    }

    #[test]
    fn y_zero_starts_lines_at_zero() {
        let chart = data(
            ChartKind::Lines,
            XValues::Numbers(vec![1.0, 2.0]),
            &[("total", &[Some(100.0), Some(120.0)])],
        );
        let lowest = |options: &LayoutOptions| {
            texts(&layout(&chart, options, SIZE, &Fixed), Paint::Label)
                .first()
                .cloned()
        };
        assert_ne!(lowest(&LayoutOptions::default()).as_deref(), Some("0"));
        let from_zero = LayoutOptions {
            y_zero: true,
            ..LayoutOptions::default()
        };
        assert_eq!(lowest(&from_zero).as_deref(), Some("0"));
    }

    #[test]
    fn long_category_names_are_cut() {
        assert_eq!(shortened("short"), "short");
        let long = "a category name far too long to show";
        let cut = shortened(long);
        assert_eq!(cut.chars().count(), MAX_LABEL_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_files_chart_ends_with_its_note() {
        let data = data(
            ChartKind::Bars,
            months(3),
            &[("net", &[Some(1.0), Some(2.0), Some(3.0)])],
        );

        let drawing = layout_file(
            &data,
            &LayoutOptions::default(),
            SIZE,
            &Fixed,
            "Charting 3 rows",
        );

        assert_eq!(drawing.size, SIZE);
        assert!(drawing.plot.max.y < SIZE.height - 2.0 * 14.0);
        assert_eq!(
            drawing.marks.last(),
            Some(&Mark::Text {
                at: Point::new(PADDING, SIZE.height - PADDING - 7.0),
                text: "Charting 3 rows".to_owned(),
                anchor: Anchor::Start,
                paint: Paint::Label,
            })
        );
    }

    #[test]
    fn a_files_chart_without_points_says_so_under_its_title() {
        let empty = data(ChartKind::Bars, months(0), &[("net", &[])]);
        let options = LayoutOptions {
            title: Some("Net sales".to_owned()),
            ..LayoutOptions::default()
        };

        let drawing = layout_file(&empty, &options, SIZE, &Fixed, "No rows to chart");

        let texts: Vec<&str> = drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["Net sales", "No rows to chart"]);
        assert_eq!(drawing.marks.len(), 2);
    }
}
