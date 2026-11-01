//! ER diagrams: a database's tables as boxes, and the foreign keys between them as lines.
//!
//! A [`Graph`] holds each table's box size and which tables a key joins; [`Graph::layout`]
//! places the boxes in layers, referenced tables first, and [`png`] draws a diagram's SVG as a
//! picture. Nothing here measures text or talks to a database: the app measures the boxes,
//! draws them where the layout says, and writes the SVG.

mod layout;
mod png;

pub use layout::{Graph, NodeId};
pub use png::{Fonts, MAX_PIXELS, PngError, png};

/// A box's size, in points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// Where a box's top-left corner sits, in points, the diagram's origin at the top left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
