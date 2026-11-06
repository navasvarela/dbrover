//! Pictures DbRover exports, such as ER diagrams and charts: each one is written as SVG by
//! whatever draws it, with the helpers in [`svg`], and [`png`] draws that SVG as a PNG, with the
//! fonts it is given rather than the system's. [`FontMeasure`] measures text in the bundled
//! fonts, for a picture laid out with no window.

mod fonts;
mod measure;
mod png;
pub mod svg;

pub use fonts::{Fonts, INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO};
pub use measure::{FontMeasure, MeasureError};
pub use png::{MAX_PIXELS, PngError, png};
pub use svg::{BASELINE_DROP, Escaped, MONO, Points, Rgb, SANS};
