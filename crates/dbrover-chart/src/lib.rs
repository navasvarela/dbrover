//! Charts of a result's columns, drawn from the rows already fetched.
//!
//! A chart is worked out in steps, each a pure function tested on its own:
//!
//! 1. Each column gets a [`Role`] (numbers, times or text) from its declared type, or from
//!    its values when it has none.
//! 2. A [`Choice`] picks the X column, the Y columns and the [`ChartKind`];
//!    [`Choice::starting`] is the one a result opens with.
//! 3. [`ChartData::read`] reads the chosen columns from the rows: one X per point, and each
//!    series' value there, with what was left out and why.
//! 4. [`layout`] places it all in a size, as [`Mark`]s in points: bars, lines, dots, axes,
//!    labels and the legend.
//!
//! A chart can also be written down as a [`ChartSpec`], a few TOML keys under `[chart]`, which
//! resolves against a result's columns into a [`Choice`], how its rows are sorted and cut
//! ([`arrange`]), and the [`LayoutOptions`] its titles set.
//!
//! Whoever shows a chart only paints the marks, and [`svg`] writes the same marks as a file, so a
//! file shows the chart the screen shows, in [`FILE_COLOURS`]. With no window, text is measured
//! with the bundled font's file (`dbrover_picture::FontMeasure`). Nothing here paints on screen
//! or talks to a database.

mod choice;
mod data;
mod layout;
mod measure;
mod note;
mod palette;
mod role;
mod spec;
mod svg;
mod ticks;
mod time;
mod values;

pub use choice::{ChartKind, Choice, STARTING_SERIES};
pub use data::{ChartData, MAX_BARS, MAX_POINTS, Series, Summary, Times, XValues};
pub use layout::{
    Anchor, Drawing, LayoutOptions, Mark, POINT_RADIUS, Paint, Point, Rect, Size, TextMeasure,
    layout,
};
pub use palette::{
    BAR_ROUNDING, FILE_COLOURS, FILE_TEXT_SIZE, FileColours, LINE_WIDTH, ROUNDED_BAR_WIDTH,
    SERIES_DARK, SERIES_LIGHT,
};
pub use role::{Column, Role};
pub use spec::{
    ChartSpec, ColumnProblem, Resolved, SPEC_TABLE, SavedChoice, Sort, SortBy, SpecProblem,
    WriteError, arrange, for_report, write_back,
};
pub use svg::svg;
