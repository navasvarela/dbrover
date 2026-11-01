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
//! Whoever shows a chart only paints the marks, and an export writes the same marks, so a file
//! shows the chart the screen shows. Nothing here draws or talks to a database.

mod choice;
mod data;
mod layout;
mod role;
mod ticks;
mod time;
mod values;

pub use choice::{ChartKind, Choice, STARTING_SERIES};
pub use data::{ChartData, MAX_BARS, MAX_POINTS, Series, Summary, Times, XValues};
pub use layout::{
    Anchor, Drawing, Mark, POINT_RADIUS, Paint, Point, Rect, Size, TextMeasure, layout,
};
pub use role::{Column, Role};
