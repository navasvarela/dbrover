//! Pictures the app exports, such as ER diagrams and charts: the formats **Export ›** offers.
//! What every SVG writer shares, so pictures look alike, is `dbrover_picture::svg`. The engine
//! writes them (`Command::ExportPicture`), drawing a PNG from the SVG with
//! `dbrover_picture::png`.

use dbrover_picture::Rgb;
use egui::Color32;

/// What **Export ›** writes: SVG, or PNG at a scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureFormat {
    Svg,
    /// Drawn at this many pixels a point.
    Png {
        scale: u8,
    },
}

impl PictureFormat {
    /// The choices, in the order the menu lists them.
    pub const ALL: [Self; 4] = [
        Self::Svg,
        Self::Png { scale: 1 },
        Self::Png { scale: 2 },
        Self::Png { scale: 3 },
    ];

    pub fn label(self) -> String {
        match self {
            Self::Svg => "SVG…".to_owned(),
            Self::Png { scale } => format!("PNG at {scale}×…"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Svg => "SVG",
            Self::Png { .. } => "PNG",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png { .. } => "png",
        }
    }
}

/// A theme's colour as a picture writes it, `#2a78d6`.
pub(crate) fn hex(color: Color32) -> String {
    Rgb(color.r(), color.g(), color.b()).to_string()
}
