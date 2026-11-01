//! A picture as a PNG, drawn from its SVG with resvg, at a scale.

use std::fmt;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// The most pixels a PNG may have: 64 megapixels, 256 MB while it is drawn. A diagram of a few
/// hundred tables at 3× passes it; past it the export is refused rather than risk running out
/// of memory, and SVG, or a smaller scale, is the way.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// The font files the SVG's text is drawn with. Only these: the system's fonts are never
/// looked at, so a PNG looks the same on every machine.
#[derive(Clone)]
pub struct Fonts {
    /// The first is the face for any family the SVG names that isn't among them.
    pub files: Vec<&'static [u8]>,
    /// The family of the first file, which text falls back to.
    pub fallback_family: &'static str,
}

impl fmt::Debug for Fonts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} font files", self.files.len())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PngError {
    #[error("the picture's SVG can't be read: {0}")]
    Svg(#[from] resvg::usvg::Error),
    #[error(
        "a PNG of {width} × {height} pixels is too large to draw; \
         choose a smaller scale, or export SVG"
    )]
    TooLarge { width: u32, height: u32 },
    #[error("the PNG could not be encoded: {0}")]
    Encode(String),
}

/// Draws `svg` at `scale` (1 for its own size in points) and encodes it as PNG.
///
/// # Errors
///
/// [`PngError::Svg`] if `svg` isn't SVG, [`PngError::TooLarge`] past [`MAX_PIXELS`], and
/// [`PngError::Encode`] if the pixels can't be written as PNG.
pub fn png(svg: &str, scale: f32, fonts: &Fonts) -> Result<Vec<u8>, PngError> {
    let mut options = Options {
        font_family: fonts.fallback_family.to_owned(),
        ..Options::default()
    };
    for file in &fonts.files {
        options.fontdb_mut().load_font_data(file.to_vec());
    }
    let tree = Tree::from_str(svg, &options)?;
    let size = tree.size().to_int_size();
    let too_large = || PngError::TooLarge {
        width: (size.width() as f32 * scale).ceil() as u32,
        height: (size.height() as f32 * scale).ceil() as u32,
    };
    let scaled = size.scale_by(scale).ok_or_else(too_large)?;
    if u64::from(scaled.width()) * u64::from(scaled.height()) > MAX_PIXELS {
        return Err(too_large());
    }
    let mut pixmap = Pixmap::new(scaled.width(), scaled.height()).ok_or_else(too_large)?;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|error| PngError::Encode(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTER: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf");

    fn fonts() -> Fonts {
        Fonts {
            files: vec![INTER],
            fallback_family: "Inter",
        }
    }

    fn svg(width: u32, height: u32, body: &str) -> String {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
             viewBox=\"0 0 {width} {height}\">{body}</svg>"
        )
    }

    /// The PNG's width, height, and how many of its pixels aren't white.
    fn decode(png: &[u8]) -> (u32, u32, usize) {
        let pixmap = Pixmap::decode_png(png).unwrap();
        let inked = pixmap
            .pixels()
            .iter()
            .filter(|pixel| pixel.red() < 200)
            .count();
        (pixmap.width(), pixmap.height(), inked)
    }

    #[test]
    fn draws_at_each_scale() {
        let picture = svg(40, 20, "<rect width=\"40\" height=\"20\" fill=\"#000\"/>");
        for scale in [1.0, 2.0, 3.0] {
            let (width, height, inked) = decode(&png(&picture, scale, &fonts()).unwrap());

            assert_eq!((width, height), (40 * scale as u32, 20 * scale as u32));
            assert_eq!(inked as u32, width * height);
        }
    }

    #[test]
    fn draws_text_with_the_fonts_it_is_given() {
        let white = "<rect width=\"120\" height=\"30\" fill=\"#fff\"/>";
        let text = "<text x=\"4\" y=\"20\" font-family=\"Inter\" font-size=\"16\">orders</text>";
        let without_text = decode(&png(&svg(120, 30, white), 1.0, &fonts()).unwrap()).2;
        let with_text =
            decode(&png(&svg(120, 30, &format!("{white}{text}")), 1.0, &fonts()).unwrap()).2;

        assert_eq!(without_text, 0);
        assert!(with_text > 50, "{with_text} pixels of text");
    }

    #[test]
    fn refuses_a_picture_too_large_to_draw() {
        let huge = svg(20_000, 10_000, "");

        let refused = png(&huge, 3.0, &fonts());

        assert!(
            matches!(
                refused,
                Err(PngError::TooLarge {
                    width: 60_000,
                    height: 30_000
                })
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn refuses_what_isnt_svg() {
        assert!(matches!(
            png("<html/>", 1.0, &fonts()),
            Err(PngError::Svg(_))
        ));
    }
}
