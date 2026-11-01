//! Pictures the app exports, such as ER diagrams: the formats **Export ›** offers, and what
//! every SVG writer shares, so pictures look alike. The engine writes them
//! (`Command::ExportPicture`), drawing a PNG from the SVG with `dbrover_picture::png`.

use std::fmt::{self, Write as _};

use egui::Color32;

/// The fonts pictures name, each followed by the platform's, for a browser without them. A
/// PNG is drawn with the app's own (`fonts::bundled_files`).
pub(crate) const SANS: &str = "Inter, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif";
pub(crate) const MONO: &str = "'JetBrains Mono', Menlo, Consolas, monospace";

/// From a line of text's middle down to its baseline, as a share of its size: about where
/// Inter's and JetBrains Mono's lower-case letters sit. Not `dominant-baseline`, which resvg and
/// browsers read differently.
pub(crate) const BASELINE_DROP: f32 = 0.35;

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

pub(crate) fn hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Text as XML character data: `&`, `<` and `>` escaped, and the control characters XML
/// can't hold dropped.
pub(crate) struct Escaped<'a>(pub(crate) &'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                c if c.is_control() && !matches!(c, '\t' | '\n') => {}
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}
