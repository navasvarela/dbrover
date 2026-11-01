//! Pictures DbRover exports, such as ER diagrams: each one is written as SVG by whatever draws
//! it, and [`png`] draws that SVG as a PNG, with the fonts it is given rather than the
//! system's.

mod png;

pub use png::{Fonts, MAX_PIXELS, PngError, png};
