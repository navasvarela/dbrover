//! Laying a chart out: from its points and a size to the marks that draw it, in points, the
//! same marks whether they are painted in the tab or written to a file.
//!
//! A chart has two axes, and which way each runs is the kind's business, not the marks'. One is
//! the **value** axis, which the Y columns are measured along; the other is the **category**
//! axis, which the X values are placed along. Upright, values run up the side and categories
//! along the bottom. Every mark is placed through a [`Frame`], which holds how far each axis
//! reaches and which way about they are, so a kind that lies the chart down swaps the two
//! without a second description of the picture.

use crate::ChartKind;
use crate::data::{ChartData, XValues};
use crate::overlay::Overlay;
use crate::palette::LINE_WIDTH;
use crate::ticks::{Ticks, decimal_exponent};
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
/// Categories' labels are cut to this many characters, with an ellipsis, where a slot along the
/// bottom is what holds them: 20 characters is about what a slot holds.
const MAX_LABEL_CHARS: usize = 20;
/// Lying down, the category names have the left gutter to themselves, and it grows to fit them
/// up to this share of the width. Long names are the reason for the kind, so the gutter gives
/// way first; past this share the plot would have less room than its labels.
const MAX_GUTTER_SHARE: f32 = 1.0 / 3.0;
/// A legend's colour square.
const SWATCH: f32 = 10.0;
/// Smaller than this, a plot has no room for its points, and nothing is drawn.
const MIN_PLOT_SIDE: f32 = 30.0;
/// An overlay's line, thinner than a series' own so it reads as derived from it.
const DERIVED_WIDTH: f32 = 1.5;
/// A dash of an overlay's line, and the gap after it.
const DERIVED_DASH: f32 = 4.0;

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
        stroke: Stroke,
    },
    /// A filled band between a series and the stack below it, as a closed ring: the series' own
    /// edge along the category axis, then the edge below it back again. Its length is even, and
    /// its first half is the series' edge in the points' order.
    ///
    /// One mark, whatever draws it. An SVG writes the ring as a single `<polygon>`; a painter
    /// that fills only convex shapes, as egui's does, splits it with [`band_quads`], which is
    /// exact rather than approximate — see that function.
    Band { points: Vec<Point>, paint: Paint },
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

/// How a line is drawn beyond its colour: solid for a series' own, dashed for one computed from
/// it, which reads as derived rather than measured. One mark drives both surfaces:
/// `stroke-dasharray` in a file, egui's dashed line on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub width: f32,
    /// The length of a dash, and of the gap after it; `None` for a solid line.
    pub dash: Option<f32>,
}

impl Stroke {
    /// A series' own line: solid, at the chart's line width.
    pub const SERIES: Self = Self {
        width: LINE_WIDTH,
        dash: None,
    };

    /// A line computed from a series: thinner and dashed, since the mean of two different
    /// measures is not a number worth drawing as boldly as the measures.
    pub const DERIVED: Self = Self {
        width: DERIVED_WIDTH,
        dash: Some(DERIVED_DASH),
    };
}

/// How wide a text is, and how tall a line of it, in the one size every label uses: the tab
/// measures with its font, tests and files with an estimate.
pub trait TextMeasure {
    fn width(&self, text: &str) -> f32;
    fn line_height(&self) -> f32;
}

/// How the value axis progresses: evenly, or by powers of ten.
///
/// A log scale draws lines and scatters only. A bar's length and a band's thickness are read as
/// ratios from the zero line, and one grown from a log axis's arbitrary baseline misstates
/// exactly that, so the bar kinds and an area refuse it rather than draw it wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum YScale {
    #[default]
    Linear,
    /// Powers of ten. A value at or below zero has no place on one, so it is a gap, counted
    /// and named in the line under the chart — never a clamped epsilon and never a dropped row
    /// nobody hears about.
    Log,
}

impl YScale {
    pub const ALL: [Self; 2] = [Self::Linear, Self::Log];

    /// The scale's name in a chart spec: `y_scale = "log"`.
    pub fn spec_name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Log => "log",
        }
    }

    pub fn from_spec_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scale| scale.spec_name() == name)
    }
}

/// What a chart spec sets of its look: titles, the Y axis, the series' colours and the lines
/// computed from their points.
///
/// Not `Eq`: `overlay`'s `line V` carries the value it is drawn at, and a float is not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutOptions {
    /// A line above the chart.
    pub title: Option<String>,
    /// In place of the X column's name under the axis.
    pub x_title: Option<String>,
    /// In place of the Y column's name above the plot, or before the legend of several.
    pub y_title: Option<String>,
    /// Start the Y axis at zero for lines and scatters, as bars always do.
    pub y_zero: bool,
    /// How the value axis progresses: evenly, or by powers of ten.
    pub y_scale: YScale,
    /// Lines computed from the points already drawn, each drawn per series and named under the
    /// chart.
    pub overlay: Vec<Overlay>,
    /// Colours for the series, from the first. A series past the end of the list keeps the
    /// palette's own hue, stepped for the surface it is drawn on. The layout never reads these
    /// — it emits only [`Paint::Series`] — so whoever paints the marks resolves them, which is
    /// how one named colour comes out the same hue on screen and in a file.
    pub colours: Vec<dbrover_picture::Rgb>,
}

/// How far an axis reaches in the drawing: the point its low value sits at, and the point its
/// high value sits at. `from` may be past `to`, as an upright chart's value axis runs from the
/// plot's bottom edge up to its top one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Extent {
    from: f32,
    to: f32,
}

impl Extent {
    const fn new(from: f32, to: f32) -> Self {
        Self { from, to }
    }

    /// How far it reaches, which is negative when it runs backwards.
    fn span(self) -> f32 {
        self.to - self.from
    }
}

/// Which way a chart's two axes run, and so which coordinate of a point each one drives.
///
/// Every mark goes through here. Upright, a value is a point's Y and a category its X; a kind
/// that lies the chart down swaps them, and nothing that places a mark has to know which it
/// was. The one thing that doesn't swap is where a label sits: an upright chart's value labels
/// are to the left of the plot and its category labels under it, and lying down they are under
/// it and to the left of it, so [`Frame::value_label`] and [`Frame::category_label`] are where
/// that is decided, once each.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// The axis the Y columns are measured along.
    value: Extent,
    /// The axis the X values are placed along.
    category: Extent,
    /// Values run across the drawing and categories down it, rather than the other way about.
    lying_down: bool,
}

impl Frame {
    /// Values up the side from the plot's bottom edge, categories along the bottom from its
    /// left edge.
    fn upright(plot: Rect) -> Self {
        Self {
            value: Extent::new(plot.max.y, plot.min.y),
            category: Extent::new(plot.min.x, plot.max.x),
            lying_down: false,
        }
    }

    /// Values rightwards from the plot's left edge, categories downwards from its top one, so
    /// the first category reads first.
    fn lying_down(plot: Rect) -> Self {
        Self {
            value: Extent::new(plot.min.x, plot.max.x),
            category: Extent::new(plot.min.y, plot.max.y),
            lying_down: true,
        }
    }

    /// The frame a chart of `kind` is drawn in.
    fn for_kind(kind: ChartKind, plot: Rect) -> Self {
        match kind {
            ChartKind::Hbars => Self::lying_down(plot),
            ChartKind::Bars | ChartKind::Lines | ChartKind::Scatter | ChartKind::Area => {
                Self::upright(plot)
            }
        }
    }

    /// How wide a category's label may be, lying down: the gutter between the drawing's edge
    /// and the value axis, which is all a name has. `None` upright, where a name is cut to a
    /// slot's worth of characters instead and no width bounds it.
    fn name_room(self) -> Option<f32> {
        self.lying_down
            .then(|| (self.value.from - LABEL_GAP - PADDING).max(0.0))
    }

    /// The point `value` along the value axis and `category` along the category axis.
    fn at(self, value: f32, category: f32) -> Point {
        if self.lying_down {
            Point::new(value, category)
        } else {
            Point::new(category, value)
        }
    }

    /// Where along each axis `point` is: the inverse of [`Self::at`].
    fn of(self, point: Point) -> (f32, f32) {
        if self.lying_down {
            (point.x, point.y)
        } else {
            (point.y, point.x)
        }
    }

    /// The ends of a line at `value` spanning the whole category axis: a grid line, the zero
    /// line, and the axis line itself.
    fn across(self, value: f32) -> (Point, Point) {
        (
            self.at(value, self.category.from),
            self.at(value, self.category.to),
        )
    }

    /// Where a value's tick label sits, and how it is anchored there: upright, left of the
    /// plot and ending at it; lying down, under the plot and centred on the tick.
    fn value_label(self, value: f32, line: f32) -> (Point, Anchor) {
        if self.lying_down {
            (
                self.at(value, self.category.to + LABEL_GAP + line / 2.0),
                Anchor::Middle,
            )
        } else {
            (self.at(value, self.category.from - LABEL_GAP), Anchor::End)
        }
    }

    /// Where a category's label sits, and how it is anchored there: upright, under the plot and
    /// centred on the slot; lying down, left of the plot and ending at it.
    fn category_label(self, category: f32, line: f32) -> (Point, Anchor) {
        if self.lying_down {
            (self.at(self.value.from - LABEL_GAP, category), Anchor::End)
        } else {
            (
                self.at(self.value.from + LABEL_GAP + line / 2.0, category),
                Anchor::Middle,
            )
        }
    }
}

/// A chart laid out: its marks in drawing order, and where each point went, for hovering.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    pub size: Size,
    /// Where the points are drawn, inside the axes.
    pub plot: Rect,
    pub marks: Vec<Mark>,
    kind: ChartKind,
    /// Which way the axes run, to read a pointer's position back into the axes' terms.
    frame: Frame,
    /// Categories were placed in slots of this width along the category axis; `None` when
    /// placed by value.
    slot: Option<f32>,
    /// Each point's place along the category axis.
    categories: Vec<f32>,
    /// Each series' value at each point, along the value axis.
    values: Vec<Vec<Option<f32>>>,
}

impl Drawing {
    /// The point under `at`: the category it is over, on lines the nearest point along the
    /// category axis, on a scatter the nearest point within reach.
    pub fn hit(&self, at: Point) -> Option<usize> {
        if !self.plot.contains(at) || self.categories.is_empty() {
            return None;
        }
        let (_, category) = self.frame.of(at);
        if let Some(slot) = self.slot {
            let index = ((category - self.frame.category.from) / slot).floor() as usize;
            return (index < self.categories.len()).then_some(index);
        }
        match self.kind {
            ChartKind::Scatter => self.nearest_point(at),
            ChartKind::Bars | ChartKind::Hbars | ChartKind::Lines | ChartKind::Area => {
                self.nearest_category(category)
            }
        }
    }

    /// The ends of the line to draw across the plot at the point at `index`, to show which
    /// point is hovered: down the plot upright, across it lying down.
    pub fn guide(&self, index: usize) -> Option<(Point, Point)> {
        let category = self.categories.get(index).copied()?;
        Some((
            self.frame.at(self.frame.value.from, category),
            self.frame.at(self.frame.value.to, category),
        ))
    }

    fn nearest_category(&self, category: f32) -> Option<usize> {
        self.categories
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (*a - category).abs().total_cmp(&(*b - category).abs()))
            .map(|(index, _)| index)
    }

    fn nearest_point(&self, at: Point) -> Option<usize> {
        let distance = |index: usize, value: f32| {
            let Some(category) = self.categories.get(index).copied() else {
                return f32::INFINITY;
            };
            let point = self.frame.at(value, category);
            (point.x - at.x).hypot(point.y - at.y)
        };
        self.values
            .iter()
            .flat_map(|series| series.iter().enumerate())
            .filter_map(|(index, value)| value.map(|value| (index, distance(index, value))))
            .filter(|(_, distance)| *distance <= HOVER_DISTANCE)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }
}

/// The value axis's ticks, their labels, and the left gutter they or the category names leave
/// for the plot.
///
/// This is the one thing the two orientations do in a different order. Upright, the value
/// labels are what the gutter has to hold, and the plot's height — which the gutter doesn't
/// change — is what decides how many ticks fit, so the ticks come first. Lying down, the
/// category names are in the gutter and the value labels are under the plot, so the gutter
/// comes first, and only then is the plot's width, and with it the number of ticks, known.
struct Measured {
    ticks: Ticks,
    labels: Vec<String>,
    gutter: f32,
    /// Room to leave at the right edge. Lying down, a value label is centred on its tick and
    /// the last tick is at the plot's right edge, so without this half of "2,000" falls outside
    /// the picture. Upright, the labels are in the gutter and none can overflow.
    right: f32,
}

impl Measured {
    /// Ticks for a plot `height` tall, and a gutter wide enough for the widest of their labels.
    fn upright(
        height: f32,
        low: f64,
        high: f64,
        scale: YScale,
        measure: &impl TextMeasure,
    ) -> Self {
        let line = measure.line_height();
        let ticks = ticks_over(
            low,
            high,
            scale,
            (height / Y_TICK_SPACING) as usize,
            (height / (line + LABEL_GAP)) as usize,
        );
        let labels = tick_labels(&ticks);
        let gutter = widest(&labels, measure);
        Self {
            ticks,
            labels,
            gutter,
            right: 0.0,
        }
    }

    /// A gutter for the category names, capped at [`MAX_GUTTER_SHARE`] of the width, and ticks
    /// for the plot's remaining width.
    ///
    /// The value labels lie side by side along the bottom here, where upright they are stacked
    /// up the side, so they take [`X_TICK_SPACING`]'s wider target: a horizontal chart gets
    /// fewer value ticks than the same chart standing up, which is what gives the labels room.
    fn lying_down(
        data: &ChartData,
        size: Size,
        low: f64,
        high: f64,
        measure: &impl TextMeasure,
    ) -> Self {
        let names = match &data.x {
            XValues::Categories(names) => names.as_slice(),
            // Bars, upright or lying down, always read their X as categories.
            XValues::Numbers(_) | XValues::Times(_) => &[],
        };
        let wanted = names
            .iter()
            .map(|name| measure.width(name))
            .fold(0.0, f32::max);
        let gutter = wanted.min(size.width * MAX_GUTTER_SHARE).max(0.0);
        let width = size.width - 2.0 * PADDING - gutter - LABEL_GAP;
        // Lying down is a bar kind, which refuses a log scale, so the ticks are always even.
        let ticks = Ticks::nice(low, high, (width / X_TICK_SPACING) as usize);
        let labels = tick_labels(&ticks);
        // Half the widest label, since the last tick sits at the plot's right edge and its
        // label is centred on it. The ticks were chosen for the width before this was taken
        // off, which moves the target by a label's width at most.
        let right = widest(&labels, measure) / 2.0;
        Self {
            ticks,
            labels,
            gutter,
            right,
        }
    }
}

/// The value axis's ticks: round steps over the range, or the decades it spans.
///
/// An even axis aims for a comfortable `target`, one tick per [`Y_TICK_SPACING`] points, since
/// it can pick any step. A log axis's granularity is the decade, so what it needs to know is
/// how many labels physically `fit` — a line's height apiece.
fn ticks_over(low: f64, high: f64, scale: YScale, target: usize, fits: usize) -> Ticks {
    match scale {
        YScale::Linear => Ticks::nice(low, high, target),
        YScale::Log => Ticks::decades(low, high, fits),
    }
}

fn tick_labels(ticks: &Ticks) -> Vec<String> {
    ticks
        .values
        .iter()
        .map(|value| ticks.label(*value))
        .collect()
}

fn widest(texts: &[String], measure: &impl TextMeasure) -> f32 {
    texts
        .iter()
        .map(|text| measure.width(text))
        .fold(0.0, f32::max)
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
    let from_zero = options.y_zero || data.kind.starts_at_zero();
    let (low, high) = value_range(data, from_zero, options.y_scale);
    let measured = match data.kind {
        ChartKind::Hbars => Measured::lying_down(data, size, low, high, measure),
        _ => Measured::upright(bottom - top, low, high, options.y_scale, measure),
    };
    let plot = Rect {
        min: Point::new(PADDING + measured.gutter + LABEL_GAP, top),
        max: Point::new(size.width - PADDING - measured.right, bottom),
    };
    let frame = Frame::for_kind(data.kind, plot);
    let value_ticks = measured.ticks;
    let value_labels = measured.labels;
    let mut drawing = Drawing {
        size,
        plot,
        marks: Vec::new(),
        kind: data.kind,
        frame,
        slot: None,
        categories: Vec::new(),
        values: Vec::new(),
    };
    if plot.width() < MIN_PLOT_SIDE || plot.height() < MIN_PLOT_SIDE {
        return drawing;
    }
    let value_scale = match options.y_scale {
        YScale::Linear => Scale::over(value_ticks.low, value_ticks.high, frame.value),
        YScale::Log => Scale::by_decades(value_ticks.low, value_ticks.high, frame.value),
    };
    value_axis(
        &mut drawing.marks,
        frame,
        &value_ticks,
        &value_labels,
        value_scale,
        line,
    );
    category_axis(&mut drawing, data, frame, measure, line);
    drawing.values = data
        .series
        .iter()
        .map(|series| {
            series
                .values
                .iter()
                .map(|value| value.and_then(|value| value_scale.at(value)))
                .collect()
        })
        .collect();
    series_marks(&mut drawing, data, frame, value_scale);
    overlay_marks(&mut drawing, data, frame, value_scale, &options.overlay);
    titles(
        &mut drawing.marks,
        data,
        options,
        size,
        measure,
        plot,
        frame,
    );
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
    let plot = Rect {
        min: middle,
        max: middle,
    };
    Drawing {
        size,
        plot,
        marks,
        kind,
        frame: Frame::upright(plot),
        slot: None,
        categories: Vec::new(),
        values: Vec::new(),
    }
}

/// A map from values to points, even or by powers of ten.
#[derive(Debug, Clone, Copy)]
struct Scale {
    low: f64,
    high: f64,
    from: f32,
    to: f32,
    progress: YScale,
}

impl Scale {
    fn new(low: f64, high: f64, from: f32, to: f32) -> Self {
        Self {
            low,
            high,
            from,
            to,
            progress: YScale::Linear,
        }
    }

    /// `low` to `high` laid along `extent`, whichever way it runs.
    fn over(low: f64, high: f64, extent: Extent) -> Self {
        Self::new(low, high, extent.from, extent.to)
    }

    /// `low` to `high` laid along `extent` by powers of ten. Both ends must be powers of ten,
    /// which [`Ticks::decades`] makes them.
    fn by_decades(low: f64, high: f64, extent: Extent) -> Self {
        Self {
            progress: YScale::Log,
            ..Self::over(low, high, extent)
        }
    }

    /// Where `value` sits along the extent, or `None` where the scale has no place for it:
    /// zero or below on a log scale, which the chart then draws as a gap.
    fn at(&self, value: f64) -> Option<f32> {
        let (low, high, value) = match self.progress {
            YScale::Linear => return Some(self.evenly_at(value)),
            // The axis's ends are powers of ten, and their exponents are read from Rust's own
            // formatting rather than from `log10`, whose last bit differs between platforms'
            // maths libraries: on 1,000 one library's 2.9999999999999996 would move every
            // coordinate on the axis. Only the value's own logarithm needs `log10`, and the
            // SVG's rounding to a hundredth of a point swallows a difference of that size.
            YScale::Log if value > 0.0 && self.low > 0.0 => (
                f64::from(decimal_exponent(self.low)),
                f64::from(decimal_exponent(self.high)),
                value.log10(),
            ),
            YScale::Log => return None,
        };
        Some(self.along(low, high, value))
    }

    /// Where `value` sits, reading the range evenly. The caller promises the scale is an even
    /// one, which every category axis is: X is placed by category, by number or by time, never
    /// by powers of ten (a log X is left for later).
    fn evenly_at(&self, value: f64) -> f32 {
        self.along(self.low, self.high, value)
    }

    fn along(&self, low: f64, high: f64, value: f64) -> f32 {
        let span = high - low;
        let share = if span > 0.0 {
            (value - low) / span
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

/// The values the value axis must reach: each value, or for stacked bars the top and bottom of
/// each stack.
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

/// The range the value axis must cover: every value the chart draws, or for stacked series the
/// top and bottom of each stack, and zero as well `from_zero`, as for bars, which grow from it.
///
/// On a log scale only values above zero count, since the others have no place on one and are
/// drawn as gaps; with nothing left to place the axis is one plain decade, and the chart draws
/// no points.
fn value_range(data: &ChartData, from_zero: bool, scale: YScale) -> (f64, f64) {
    let mut reached = reach(data);
    if scale == YScale::Log {
        reached.retain(|value| *value > 0.0);
        let low = reached.iter().copied().fold(f64::INFINITY, f64::min);
        let high = reached.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        return if low.is_finite() && high.is_finite() {
            (low, high)
        } else {
            (1.0, 10.0)
        };
    }
    let low = reached.iter().copied().fold(f64::INFINITY, f64::min);
    let high = reached.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let low = match (from_zero, low.is_finite()) {
        (_, false) => 0.0,
        (true, true) => low.min(0.0),
        (false, true) => low,
    };
    let high = match (from_zero, high.is_finite()) {
        (_, false) => 1.0,
        (true, true) => high.max(0.0),
        (false, true) => high,
    };
    (low, high)
}

/// A grid line and a label at each tick of the value axis, the zero line where the values fall
/// either side of it, and the axis line along the low end.
fn value_axis(
    marks: &mut Vec<Mark>,
    frame: Frame,
    ticks: &Ticks,
    labels: &[String],
    scale: Scale,
    line: f32,
) {
    for (value, label) in ticks.values.iter().zip(labels) {
        let Some(at) = scale.at(*value) else {
            continue;
        };
        let (from, to) = frame.across(at);
        marks.push(Mark::Line {
            from,
            to,
            paint: Paint::Grid,
        });
        let (at, anchor) = frame.value_label(at, line);
        marks.push(Mark::Text {
            at,
            text: label.clone(),
            anchor,
            paint: Paint::Label,
        });
    }
    if let Some(zero) = scale
        .at(0.0)
        .filter(|_| ticks.low < 0.0 && ticks.high > 0.0)
    {
        let (from, to) = frame.across(zero);
        marks.push(Mark::Line {
            from,
            to,
            paint: Paint::Axis,
        });
    }
    let (from, to) = frame.across(frame.value.from);
    marks.push(Mark::Line {
        from,
        to,
        paint: Paint::Axis,
    });
}

/// Places each point along the category axis, and labels it.
fn category_axis(
    drawing: &mut Drawing,
    data: &ChartData,
    frame: Frame,
    measure: &impl TextMeasure,
    line: f32,
) {
    let category = frame.category;
    let target = (category.span().abs() / X_TICK_SPACING) as usize;
    let room = frame.name_room();
    // Points placed by value sit in from the plot's sides, so one at either end isn't cut in
    // half.
    let (inset_from, inset_to) = (category.from + INSET, category.to - INSET);
    let mut label = |at: f32, text: String| {
        let (at, anchor) = frame.category_label(at, line);
        drawing.marks.push(Mark::Text {
            at,
            text,
            anchor,
            paint: Paint::Label,
        });
    };
    match &data.x {
        XValues::Categories(names) => {
            let slot = category.span() / names.len().max(1) as f32;
            // Upright, a slot along the bottom is what holds a name, and 20 characters is
            // about what a slot holds. Lying down, the gutter is the constraint, and a
            // measure is the honest test of whether a name fits it.
            let names: Vec<String> = match room {
                Some(room) => names
                    .iter()
                    .map(|name| shortened_to(name, room, measure))
                    .collect(),
                None => names.iter().map(|name| shortened(name)).collect(),
            };
            // What crowds the labels along the axis: their width upright, where they sit side
            // by side; a line's height lying down, where they are stacked.
            let thickness = if frame.lying_down {
                line
            } else {
                widest(&names, measure)
            };
            let every = ((thickness + LABEL_GAP) / slot).ceil().max(1.0) as usize;
            let centre = |index: usize| category.from + slot * (index as f32 + 0.5);
            for (index, name) in names.into_iter().enumerate().step_by(every) {
                label(centre(index), name);
            }
            drawing.slot = Some(slot);
            drawing.categories = (0..data.len()).map(centre).collect();
        }
        XValues::Numbers(numbers) => {
            let (low, high) = range(numbers);
            let ticks = Ticks::nice(low, high, target);
            let scale = Scale::new(ticks.low, ticks.high, inset_from, inset_to);
            for value in &ticks.values {
                label(scale.evenly_at(*value), ticks.label(*value));
            }
            drawing.categories = numbers
                .iter()
                .map(|number| scale.evenly_at(*number))
                .collect();
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
            let scale = Scale::new(low, high, inset_from, inset_to);
            for value in &ticks.values {
                label(scale.evenly_at(*value), ticks.label(*value));
            }
            drawing.categories = times
                .seconds
                .iter()
                .map(|time| scale.evenly_at(*time))
                .collect();
        }
    }
}

/// The four-sided pieces a band's ring splits into, one between each pair of neighbouring
/// points.
///
/// The pieces cover exactly the region the ring covers, rather than approximating it: between
/// two neighbouring points both of a band's edges are a straight segment, so the quad spanning
/// them covers exactly what the ring covers there, and the union of the quads is the ring. Each
/// piece has two parallel sides along the value axis, so it is convex, which is what a painter
/// that fills only convex shapes needs.
///
/// Empty for a ring of fewer than four points, which spans no width.
pub fn band_quads(ring: &[Point]) -> impl Iterator<Item = [Point; 4]> + '_ {
    let half = ring.len() / 2;
    let (edge, below) = ring.split_at(half);
    // `below` holds the lower edge backwards, so its point at `index` is at `half - 1 - index`.
    (0..half.saturating_sub(1)).filter_map(move |index| {
        Some([
            *edge.get(index)?,
            *edge.get(index + 1)?,
            *below.get(half - 2 - index)?,
            *below.get(half - 1 - index)?,
        ])
    })
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

/// `name` cut with an ellipsis to fit `room` points, measured rather than counted.
///
/// The characters are measured one at a time and summed, which ignores the kerning between a
/// pair, so a name may end a fraction of a point narrower than it could be. That errs towards
/// fitting, which is the way to err against a gutter, and it is the same arithmetic on every
/// platform, which a file compared byte for byte needs.
fn shortened_to(name: &str, room: f32, measure: &impl TextMeasure) -> String {
    if measure.width(name) <= room {
        return name.to_owned();
    }
    let mut kept = String::new();
    let mut taken = measure.width("…");
    for character in name.chars() {
        let wider = taken + measure.width(&character.to_string());
        if wider > room {
            break;
        }
        taken = wider;
        kept.push(character);
    }
    // A gutter with room for nothing still says a name was cut, rather than dropping it.
    format!("{kept}…")
}

fn series_marks(drawing: &mut Drawing, data: &ChartData, frame: Frame, value_scale: Scale) {
    match data.kind {
        // Stacking is worked out in the values, so only where a piece goes differs between the
        // two bar kinds, and the frame is what differs.
        ChartKind::Bars | ChartKind::Hbars if data.stacked => {
            stacked_bars(drawing, data, frame, value_scale);
        }
        ChartKind::Bars | ChartKind::Hbars => bars(drawing, frame, value_scale),
        ChartKind::Lines => lines(drawing, frame),
        ChartKind::Scatter => dots(drawing, frame),
        ChartKind::Area => bands(drawing, data, frame, value_scale),
    }
}

fn bars(drawing: &mut Drawing, frame: Frame, value_scale: Scale) {
    let slot = drawing.slot.unwrap_or_default();
    let group = slot * BAR_GROUP_SHARE;
    let width = group / drawing.values.len().max(1) as f32;
    let gap = if width >= MIN_BAR_GAP_WIDTH { 1.0 } else { 0.0 };
    // Bars refuse a log scale, so their zero always has a place; without one there is nothing
    // to grow from and nothing is drawn.
    let Some(zero) = value_scale.at(0.0) else {
        return;
    };
    for (series, values) in drawing.values.iter().enumerate() {
        for (index, value) in values.iter().enumerate() {
            let (Some(value), Some(centre)) = (value, drawing.categories.get(index)) else {
                continue;
            };
            let near = centre - group / 2.0 + width * series as f32;
            drawing.marks.push(Mark::Rect {
                rect: Rect::spanning(frame.at(zero, near), frame.at(*value, near + width - gap)),
                paint: Paint::Series(series),
                rounded: true,
            });
        }
    }
}

/// A bar per point as wide as a group of bars, its series' values stacked on each other in the
/// series' order: positive values up from zero, negative ones down from it.
fn stacked_bars(drawing: &mut Drawing, data: &ChartData, frame: Frame, value_scale: Scale) {
    let width = drawing.slot.unwrap_or_default() * BAR_GROUP_SHARE;
    for (point, centre) in drawing.categories.iter().enumerate() {
        let near = centre - width / 2.0;
        let (mut up, mut down) = (0.0, 0.0);
        for (series, values) in data.series.iter().enumerate() {
            let Some(value) = values.values.get(point).copied().flatten() else {
                continue;
            };
            let end = if value >= 0.0 { &mut up } else { &mut down };
            let from = *end;
            *end += value;
            let (Some(bottom), Some(top)) = (value_scale.at(from), value_scale.at(*end)) else {
                continue;
            };
            drawing.marks.push(Mark::Rect {
                rect: Rect::spanning(frame.at(bottom, near), frame.at(top, near + width)),
                paint: Paint::Series(series),
                rounded: false,
            });
        }
    }
}

/// A line per series, broken where a value is missing; a point on its own between two gaps is
/// a dot, since a line needs two.
fn lines(drawing: &mut Drawing, frame: Frame) {
    let dotted = drawing.categories.len() <= MAX_DOTTED_POINTS;
    for (series, values) in drawing.values.iter().enumerate() {
        let paint = Paint::Series(series);
        let runs = values
            .iter()
            .zip(&drawing.categories)
            .map(|(value, category)| value.map(|value| frame.at(value, *category)))
            .collect::<Vec<_>>();
        for run in runs.split(Option::is_none) {
            let points: Vec<Point> = run.iter().flatten().copied().collect();
            if points.len() > 1 {
                drawing.marks.push(Mark::Polyline {
                    points: points.clone(),
                    paint,
                    stroke: Stroke::SERIES,
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

/// A band per series: the region between the series' own edge and the stack below it, as a
/// closed ring.
///
/// The pieces pile on the running totals exactly as [`stacked_bars`] piles them — positives up
/// from zero, negatives down from it — so a result charted both ways puts the same values in
/// the same places. A gap breaks a band, as it breaks a line: a run of points the series has a
/// value at becomes a ring of its own, and a run of one draws nothing, since a band needs two
/// points to have width. The stack below still skips a gap, as a stacked bar's pieces do, so
/// the series above sit lower there.
fn bands(drawing: &mut Drawing, data: &ChartData, frame: Frame, value_scale: Scale) {
    let points = drawing.categories.len();
    // The tops of the positive stack and the bottoms of the negative one, point by point.
    let mut below = vec![(0.0_f64, 0.0_f64); points];
    for (series, values) in data.series.iter().enumerate() {
        let mut edges: Vec<Option<(Point, Point)>> = Vec::with_capacity(points);
        for (point, carried) in below.iter_mut().enumerate() {
            let (Some(value), Some(category)) = (
                values.values.get(point).copied().flatten(),
                drawing.categories.get(point).copied(),
            ) else {
                edges.push(None);
                continue;
            };
            let end = if value >= 0.0 {
                &mut carried.0
            } else {
                &mut carried.1
            };
            let from = *end;
            *end += value;
            let (Some(top), Some(bottom)) = (value_scale.at(*end), value_scale.at(from)) else {
                edges.push(None);
                continue;
            };
            edges.push(Some((frame.at(top, category), frame.at(bottom, category))));
        }
        for run in edges.split(Option::is_none) {
            let run: Vec<(Point, Point)> = run.iter().flatten().copied().collect();
            if run.len() < 2 {
                continue;
            }
            let mut ring: Vec<Point> = run.iter().map(|(edge, _)| *edge).collect();
            ring.extend(run.iter().rev().map(|(_, below)| *below));
            drawing.marks.push(Mark::Band {
                points: ring,
                paint: Paint::Series(series),
            });
        }
    }
}

fn dots(drawing: &mut Drawing, frame: Frame) {
    for (series, values) in drawing.values.iter().enumerate() {
        for (value, category) in values.iter().zip(&drawing.categories) {
            if let Some(value) = value {
                drawing.marks.push(Mark::Circle {
                    centre: frame.at(*value, *category),
                    radius: POINT_RADIUS,
                    paint: Paint::Series(series),
                });
            }
        }
    }
}

/// A dashed line per overlay per series, in that series' colour, from the series' own points.
///
/// Drawn after the series, so a trend reads over its points rather than under them, and from
/// the values rather than from where they landed: on a log axis the mean of the values is not
/// the mean of their positions, and it is the values the word means.
fn overlay_marks(
    drawing: &mut Drawing,
    data: &ChartData,
    frame: Frame,
    value_scale: Scale,
    overlays: &[Overlay],
) {
    if overlays.is_empty() || drawing.categories.is_empty() {
        return;
    }
    let xs = x_positions(data);
    for overlay in overlays {
        for (series, values) in data.series.iter().enumerate() {
            let placed: Vec<Option<Point>> = overlay
                .over(&xs, &values.values)
                .into_iter()
                .zip(&drawing.categories)
                .map(|(value, category)| {
                    let at = value.and_then(|value| value_scale.at(value))?;
                    Some(frame.at(at, *category))
                })
                .collect();
            for run in placed.split(Option::is_none) {
                let points: Vec<Point> = run.iter().flatten().copied().collect();
                if points.len() > 1 {
                    drawing.marks.push(Mark::Polyline {
                        points,
                        paint: Paint::Series(series),
                        stroke: Stroke::DERIVED,
                    });
                }
            }
        }
    }
}

/// The X of each point as an overlay reads it: the slot's index for categories, the value for
/// numbers, the instant for times.
///
/// Uniform slots make the index the distance, so a fit over categories matches what the eye
/// sees, and a trend over months that `strftime` made text works rather than being refused on a
/// technicality. The line under the chart says which it was.
fn x_positions(data: &ChartData) -> Vec<f64> {
    match &data.x {
        XValues::Categories(names) => (0..names.len()).map(|index| index as f64).collect(),
        XValues::Numbers(numbers) => numbers.clone(),
        XValues::Times(times) => times.seconds.clone(),
    }
}

/// The title above everything, and each axis's name beside that axis: the one along the bottom
/// under the plot, the one up the side above it, where a legend of several series follows it.
///
/// Upright that puts the X column's name, or `x_title`, under the chart and the Y column's, or
/// `y_title`, above the plot. Lying down the two axes have swapped, so the names swap with
/// them, and each still sits beside what it names. A value axis of several series has no one
/// name; the legend names them instead, and that slot is left empty.
fn titles(
    marks: &mut Vec<Mark>,
    data: &ChartData,
    options: &LayoutOptions,
    size: Size,
    measure: &impl TextMeasure,
    plot: Rect,
    frame: Frame,
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
    let category_name = options
        .x_title
        .clone()
        .unwrap_or_else(|| data.x_name.clone());
    let value_name = match (&options.y_title, data.series.as_slice()) {
        (Some(title), _) => Some(title.clone()),
        (None, [only]) => Some(only.name.clone()),
        (None, _) => None,
    };
    let (under, above) = if frame.lying_down {
        (value_name, Some(category_name))
    } else {
        (Some(category_name), value_name)
    };
    if let Some(name) = under {
        marks.push(Mark::Text {
            at: Point::new(
                (plot.min.x + plot.max.x) / 2.0,
                size.height - PADDING - line / 2.0,
            ),
            text: name,
            anchor: Anchor::Middle,
            paint: Paint::Title,
        });
    }
    let mut x = plot.min.x;
    if let Some(name) = above {
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

    /// The bands' rings, in the order they were emitted.
    fn rings(drawing: &Drawing) -> Vec<(Vec<Point>, Paint)> {
        drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Band { points, paint } => Some((points.clone(), *paint)),
                _ => None,
            })
            .collect()
    }

    /// A band's two edges from its ring: the series' own, and the stack below it, both in the
    /// points' order.
    fn band_edges(ring: &[Point]) -> (Vec<Point>, Vec<Point>) {
        let half = ring.len() / 2;
        let (edge, below) = ring.split_at(half);
        (edge.to_vec(), below.iter().rev().copied().collect())
    }

    /// An area of `series` over as many months as they have values.
    fn area(series: &[(&str, &[Option<f64>])]) -> ChartData {
        let points = series.first().map_or(0, |(_, values)| values.len());
        let mut chart = data(ChartKind::Area, months(points), series);
        chart.stacked = true;
        chart
    }

    /// Options whose value axis runs by powers of ten.
    fn logarithmic() -> LayoutOptions {
        LayoutOptions {
            y_scale: YScale::Log,
            ..LayoutOptions::default()
        }
    }

    fn named(names: &[&str]) -> XValues {
        XValues::Categories(names.iter().map(|name| (*name).to_owned()).collect())
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
        let second = Point::new(scatter.categories[1], scatter.values[0][1].unwrap());
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
            ..LayoutOptions::default()
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

    #[test]
    fn horizontal_bars_grow_rightwards_in_slots_down_the_plot() {
        let chart = data(
            ChartKind::Hbars,
            months(3),
            &[("total", &[Some(10.0), Some(30.0), Some(20.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert_eq!(bars.len(), 3);
        let zero = drawing.plot.min.x;
        for (rect, _) in &bars {
            assert!((rect.min.x - zero).abs() < 0.01, "{rect:?} starts at zero");
        }
        assert!(bars[1].0.width() > bars[2].0.width());
        assert!(bars[2].0.width() > bars[0].0.width());
        assert!(
            bars[0].0.max.y <= bars[1].0.min.y,
            "the first category reads first, at the top"
        );
    }

    #[test]
    fn a_long_names_gutter_stops_at_a_third_of_the_width() {
        let long = "Wholesale & Distribution (Northern Europe)";
        let chart = data(
            ChartKind::Hbars,
            named(&[long, "Acme"]),
            &[("net", &[Some(1.0), Some(2.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let gutter = drawing.plot.min.x - PADDING - LABEL_GAP;
        assert!(
            (gutter - SIZE.width * MAX_GUTTER_SHARE).abs() < 0.01,
            "the gutter stops at a third of the width, not at {gutter}"
        );
        let labels = texts(&drawing, Paint::Label);
        let cut = labels
            .iter()
            .find(|label| label.ends_with('…'))
            .unwrap_or_else(|| panic!("the long name is cut: {labels:?}"));
        assert!(
            Fixed.width(cut) <= gutter,
            "{cut} is measured into the gutter"
        );
        assert!(
            labels.contains(&"Acme".to_owned()),
            "a name that fits is left whole: {labels:?}"
        );
    }

    #[test]
    fn names_are_cut_to_the_room_they_have() {
        // Fixed measures every character 7 points wide, so 35 points holds five of them.
        let cases = [
            ("Acme", 35.0, "Acme"),
            ("Wholesale", 35.0, "Whol…"),
            ("Wholesale", 7.0, "…"),
            ("Wholesale", 0.0, "…"),
        ];
        for (name, room, expected) in cases {
            assert_eq!(
                shortened_to(name, room, &Fixed),
                expected,
                "{name} in {room}"
            );
        }
    }

    /// The reason the value ticks lie down at [`X_TICK_SPACING`]'s wider target rather than
    /// [`Y_TICK_SPACING`]'s: side by side, each label needs room for itself.
    #[test]
    fn the_value_labels_lying_down_never_collide() {
        let values: Vec<Option<f64>> = (0..8).map(|n| Some(f64::from(n) * 125_000.0)).collect();
        let chart = data(ChartKind::Hbars, months(8), &[("net", &values)]);
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let mut placed: Vec<(f32, f32)> = drawing
            .marks
            .iter()
            .filter_map(|mark| match mark {
                Mark::Text {
                    at,
                    text,
                    paint: Paint::Label,
                    ..
                } if text.chars().all(|c| c.is_ascii_digit() || c == ',') => {
                    Some((at.x, Fixed.width(text)))
                }
                _ => None,
            })
            .collect();
        placed.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(placed.len() > 2, "{placed:?}");
        for pair in placed.windows(2) {
            let (left, right) = (pair[0], pair[1]);
            assert!(
                right.0 - left.0 >= (left.1 + right.1) / 2.0,
                "{left:?} and {right:?} overlap"
            );
        }
    }

    #[test]
    fn negative_horizontal_bars_grow_leftwards_from_a_vertical_zero_line() {
        let chart = data(
            ChartKind::Hbars,
            months(2),
            &[("net", &[Some(-5.0), Some(5.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        let zero = bars[1].0.min.x;
        assert!(
            (bars[0].0.max.x - zero).abs() < 0.01,
            "the negative bar ends where the positive one starts"
        );
        assert!(
            drawing.marks.iter().any(|mark| matches!(
                mark,
                Mark::Line { from, to, paint: Paint::Axis }
                    if (from.x - zero).abs() < 0.01 && (from.x - to.x).abs() < 0.01
            )),
            "the zero line is drawn down the plot: {:?}",
            drawing.marks
        );
    }

    #[test]
    fn stacked_horizontal_pieces_lie_left_to_right_from_zero() {
        let mut chart = data(
            ChartKind::Hbars,
            months(2),
            &[
                ("bikes", &[Some(10.0), Some(-4.0)]),
                ("parts", &[Some(5.0), Some(6.0)]),
            ],
        );
        chart.stacked = true;
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let bars = rects(&drawing);
        assert_eq!(bars.len(), 4, "a piece per value");
        let (bikes, parts) = (bars[0].0, bars[1].0);
        assert!(
            (bikes.max.x - parts.min.x).abs() < 0.01,
            "parts start where bikes end"
        );
        assert!(
            (bikes.min.y - parts.min.y).abs() < 0.01,
            "one bar per month, its pieces in a row"
        );
        assert!(bikes.width() > parts.width());
        let (negative_bikes, positive_parts) = (bars[2].0, bars[3].0);
        assert!(
            negative_bikes.max.x <= positive_parts.min.x,
            "a negative piece hangs left of zero, a positive one right of it"
        );
    }

    #[test]
    fn hovering_a_horizontal_chart_finds_the_slot_down_the_plot() {
        let drawing = layout(
            &data(ChartKind::Hbars, months(4), &[("total", &[Some(1.0); 4])]),
            &LayoutOptions::default(),
            SIZE,
            &Fixed,
        );
        let plot = drawing.plot;
        let third = Point::new(plot.min.x + 1.0, plot.min.y + plot.height() * 0.6);
        assert_eq!(drawing.hit(third), Some(2));
        assert_eq!(drawing.hit(Point::new(1.0, 1.0)), None);
        let (from, to) = drawing.guide(2).expect("a guide at the third slot");
        assert!(
            (from.y - to.y).abs() < 0.01 && (from.x - to.x).abs() > 1.0,
            "the guide lies across the plot: {from:?} to {to:?}"
        );
    }

    #[test]
    fn a_horizontal_charts_axis_names_follow_their_axes() {
        let chart = data(
            ChartKind::Hbars,
            months(2),
            &[("net", &[Some(1.0), Some(2.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let at = |wanted: &str| {
            drawing.marks.iter().find_map(|mark| match mark {
                Mark::Text {
                    at,
                    text,
                    paint: Paint::Title,
                    ..
                } if text == wanted => Some(*at),
                _ => None,
            })
        };
        let (Some(net), Some(month)) = (at("net"), at("month")) else {
            panic!("expected both axes named: {:?}", drawing.marks)
        };
        assert!(
            net.y > drawing.plot.max.y,
            "the values run along the bottom, so their name is under the chart"
        );
        assert!(
            month.y < drawing.plot.min.y,
            "the categories run up the side, so their name is above the plot"
        );
    }

    #[test]
    fn an_areas_bands_pile_on_each_other_as_stacked_bars_do() {
        let chart = area(&[
            ("bikes", &[Some(10.0), Some(20.0), Some(30.0)]),
            ("parts", &[Some(5.0), Some(5.0), Some(5.0)]),
        ]);
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let rings = rings(&drawing);
        assert_eq!(rings.len(), 2, "one band a series");
        assert_eq!(rings[0].1, Paint::Series(0));
        assert_eq!(rings[1].1, Paint::Series(1));
        let (bikes, bikes_below) = band_edges(&rings[0].0);
        let (parts, parts_below) = band_edges(&rings[1].0);
        assert_eq!(bikes.len(), 3, "a point per row: {bikes:?}");
        let zero = drawing.plot.max.y;
        for point in &bikes_below {
            assert!(
                (point.y - zero).abs() < 0.01,
                "the first band sits on zero: {point:?}"
            );
        }
        for (above, below) in bikes.iter().zip(&parts_below) {
            assert!(
                (above.y - below.y).abs() < 0.01,
                "parts sits on bikes: {above:?} and {below:?}"
            );
        }
        assert!(
            parts
                .iter()
                .zip(&bikes)
                .all(|(top, bottom)| top.y < bottom.y),
            "the stack grows upwards"
        );
        assert!(
            bikes[0].y > bikes[2].y,
            "bikes grows over the months: {bikes:?}"
        );
    }

    #[test]
    fn a_negative_band_hangs_below_zero_while_the_positive_ones_pile_above_it() {
        let chart = area(&[
            ("bikes", &[Some(10.0), Some(10.0)]),
            ("returns", &[Some(-4.0), Some(-6.0)]),
            ("parts", &[Some(5.0), Some(5.0)]),
        ]);
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let rings = rings(&drawing);
        assert_eq!(rings.len(), 3);
        let (bikes, _) = band_edges(&rings[0].0);
        let (returns, returns_below) = band_edges(&rings[1].0);
        let (_, parts_below) = band_edges(&rings[2].0);
        for (edge, below) in returns.iter().zip(&returns_below) {
            assert!(
                edge.y > below.y,
                "returns hangs below the zero line it starts at: {edge:?} under {below:?}"
            );
        }
        for (above, below) in bikes.iter().zip(&parts_below) {
            assert!(
                (above.y - below.y).abs() < 0.01,
                "parts sits on bikes, not on the negative band"
            );
        }
    }

    #[test]
    fn a_gap_breaks_a_band_as_it_breaks_a_line() {
        let chart = area(&[("net", &[Some(1.0), Some(2.0), None, Some(4.0), Some(5.0)])]);
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        let rings = rings(&drawing);
        assert_eq!(rings.len(), 2, "a ring either side of the gap");
        for (ring, _) in &rings {
            assert_eq!(ring.len(), 4, "two points, two edges: {ring:?}");
        }
    }

    #[test]
    fn a_run_of_one_point_draws_no_band() {
        let chart = area(&[("net", &[Some(1.0), None, Some(3.0)])]);
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);
        assert!(
            rings(&drawing).is_empty(),
            "a band needs two points to have width"
        );
    }

    #[test]
    fn a_bands_quads_cover_it_piece_by_piece() {
        let ring = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 2.0),
            Point::new(20.0, 4.0),
            Point::new(20.0, 14.0),
            Point::new(10.0, 12.0),
            Point::new(0.0, 10.0),
        ];
        let quads: Vec<[Point; 4]> = band_quads(&ring).collect();
        assert_eq!(
            quads,
            [
                [
                    Point::new(0.0, 0.0),
                    Point::new(10.0, 2.0),
                    Point::new(10.0, 12.0),
                    Point::new(0.0, 10.0),
                ],
                [
                    Point::new(10.0, 2.0),
                    Point::new(20.0, 4.0),
                    Point::new(20.0, 14.0),
                    Point::new(10.0, 12.0),
                ],
            ]
        );
        assert_eq!(band_quads(&ring[..2]).count(), 0, "no width, no piece");
        assert_eq!(band_quads(&[]).count(), 0);
    }

    #[test]
    fn a_log_axis_puts_equal_ratios_at_equal_distances() {
        let chart = data(
            ChartKind::Scatter,
            XValues::Numbers(vec![1.0, 2.0, 3.0]),
            &[("price", &[Some(10.0), Some(100.0), Some(1000.0)])],
        );
        let options = LayoutOptions {
            y_scale: YScale::Log,
            ..LayoutOptions::default()
        };
        let drawing = layout(&chart, &options, SIZE, &Fixed);

        let placed: Vec<f32> = drawing.values[0].iter().flatten().copied().collect();
        assert_eq!(placed.len(), 3);
        let decade = placed[0] - placed[1];
        assert!(
            (decade - (placed[1] - placed[2])).abs() < 0.01,
            "a tenfold step is one distance: {placed:?}"
        );
        let labels = texts(&drawing, Paint::Label);
        for decade in ["10", "100", "1,000"] {
            assert!(
                labels.contains(&decade.to_owned()),
                "{decade} in {labels:?}"
            );
        }
    }

    #[test]
    fn a_log_scale_makes_zero_and_negative_values_counted_gaps() {
        let chart = data(
            ChartKind::Lines,
            XValues::Numbers(vec![1.0, 2.0, 3.0, 4.0]),
            &[("net", &[Some(10.0), Some(0.0), Some(-5.0), Some(100.0)])],
        );
        let options = LayoutOptions {
            y_scale: YScale::Log,
            ..LayoutOptions::default()
        };
        let drawing = layout(&chart, &options, SIZE, &Fixed);

        let placed = drawing.values[0].iter().flatten().count();
        assert_eq!(placed, 2, "zero and the negative value have no place");
        assert_eq!(
            chart.note(&logarithmic()),
            "Charting 4 rows · 2 gaps in net: zero or negative on a log scale"
        );
        assert_eq!(
            chart.note(&LayoutOptions::default()),
            "Charting 4 rows",
            "an even axis places all four"
        );
    }

    /// With no value a log axis can place, the chart draws no points and the note says why.
    #[test]
    fn a_log_axis_with_nothing_to_place_draws_no_points() {
        let chart = data(
            ChartKind::Lines,
            XValues::Numbers(vec![1.0, 2.0]),
            &[("net", &[Some(0.0), Some(-5.0)])],
        );
        let options = LayoutOptions {
            y_scale: YScale::Log,
            ..LayoutOptions::default()
        };
        let drawing = layout(&chart, &options, SIZE, &Fixed);

        assert!(drawing.values[0].iter().all(Option::is_none));
        assert!(
            !drawing
                .marks
                .iter()
                .any(|mark| matches!(mark, Mark::Polyline { .. } | Mark::Circle { .. })),
            "nothing is drawn where nothing can be placed"
        );
        assert!(
            chart
                .note(&logarithmic())
                .contains("2 gaps in net: zero or negative")
        );
    }

    /// A log axis's granularity is the decade, so the only way its labels stay readable is to
    /// show fewer of them when there is less room. At every size, none sits within a line of
    /// the next.
    #[test]
    fn a_log_axis_labels_never_crowd_each_other() {
        let chart = data(
            ChartKind::Scatter,
            XValues::Numbers(vec![1.0, 2.0, 3.0]),
            &[("price", &[Some(12.0), Some(89.0), Some(1290.0)])],
        );
        let options = LayoutOptions {
            y_scale: YScale::Log,
            ..LayoutOptions::default()
        };
        for height in [120.0_f32, 200.0, 400.0, 900.0] {
            let size = Size {
                width: 600.0,
                height,
            };
            let drawing = layout(&chart, &options, size, &Fixed);
            // A value label is the one anchored at the axis it labels; the categories' are
            // centred under theirs.
            let mut placed: Vec<f32> = drawing
                .marks
                .iter()
                .filter_map(|mark| match mark {
                    Mark::Text {
                        at,
                        anchor: Anchor::End,
                        paint: Paint::Label,
                        ..
                    } => Some(at.y),
                    _ => None,
                })
                .collect();
            placed.sort_by(f32::total_cmp);
            assert!(placed.len() >= 2, "at {height}: {placed:?}");
            for pair in placed.windows(2) {
                assert!(
                    pair[1] - pair[0] >= Fixed.line_height(),
                    "at {height}, labels {:?} are within a line of each other",
                    pair
                );
            }
        }
    }

    /// Lying down, the last value tick sits at the plot's right edge and its label is centred
    /// on it, so the plot has to leave room for half of it or the label runs off the picture.
    #[test]
    fn a_horizontal_charts_last_value_label_stays_inside_the_picture() {
        let chart = data(
            ChartKind::Hbars,
            months(3),
            &[("net", &[Some(500.0), Some(1_200.0), Some(2_000.0)])],
        );
        let drawing = layout(&chart, &LayoutOptions::default(), SIZE, &Fixed);

        for mark in &drawing.marks {
            let Mark::Text {
                at,
                text,
                anchor: Anchor::Middle,
                paint: Paint::Label,
            } = mark
            else {
                continue;
            };
            let right = at.x + Fixed.width(text) / 2.0;
            assert!(
                right <= SIZE.width,
                "{text} ends at {right}, past the picture's {}",
                SIZE.width
            );
        }
        assert!(
            drawing.plot.max.x < SIZE.width - PADDING,
            "the plot leaves the room: {:?}",
            drawing.plot
        );
    }
}
