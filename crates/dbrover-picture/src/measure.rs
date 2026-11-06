//! How wide text is in a bundled face, read from the font file, for laying out a picture with
//! no window: a report's chart, which egui never measures.
//!
//! Widths are the glyphs' advances, without kerning or shaping, so a label may be a fraction of
//! a point wider or narrower than egui lays it out on screen. Each path is consistent with
//! itself: a file's layout never comes from the screen's measure.

use ttf_parser::{Face, FaceParsingError};

use crate::fonts::INTER_REGULAR;

/// A face's advances and line, at a size.
#[derive(Clone)]
pub struct FontMeasure {
    face: Face<'static>,
    /// Points per font unit.
    scale: f32,
}

/// The bundled face can't be read: only a build with a broken font file gets this.
#[derive(Debug, thiserror::Error)]
#[error("the bundled font can't be read: {0}")]
pub struct MeasureError(#[from] FaceParsingError);

impl FontMeasure {
    /// Inter, the face pictures write their text in, at `size` points.
    ///
    /// # Errors
    ///
    /// [`MeasureError`] if the bundled font file doesn't read as a font.
    pub fn inter(size: f32) -> Result<Self, MeasureError> {
        Self::of(INTER_REGULAR, size)
    }

    fn of(file: &'static [u8], size: f32) -> Result<Self, MeasureError> {
        let face = Face::parse(file, 0)?;
        let scale = size / f32::from(face.units_per_em());
        Ok(Self { face, scale })
    }

    /// `text`'s width in points: each character's advance, a character the face lacks taking
    /// the advance of its missing-glyph box, as a renderer draws it.
    pub fn width(&self, text: &str) -> f32 {
        let units: u32 = text
            .chars()
            .map(|c| {
                let glyph = self.face.glyph_index(c).unwrap_or_default();
                u32::from(self.face.glyph_hor_advance(glyph).unwrap_or_default())
            })
            .sum();
        units as f32 * self.scale
    }

    /// A line's height in points: the face's ascender to its descender, and its line gap.
    pub fn line_height(&self) -> f32 {
        let units = i32::from(self.face.ascender()) - i32::from(self.face.descender())
            + i32::from(self.face.line_gap());
        units as f32 * self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_wider_text_wider_and_scales_with_the_size() {
        let small = FontMeasure::inter(11.0).unwrap();
        let large = FontMeasure::inter(22.0).unwrap();

        let net = small.width("net");
        assert!(net > 10.0 && net < 25.0, "{net}");
        assert!(small.width("net sales") > net);
        assert!((large.width("net") - 2.0 * net).abs() < 0.001);
        assert_eq!(small.width(""), 0.0);
    }

    #[test]
    fn a_line_is_a_little_taller_than_the_size() {
        let line = FontMeasure::inter(11.0).unwrap().line_height();

        assert!(line > 11.0 && line < 16.0, "{line}");
    }

    #[test]
    fn a_character_the_face_lacks_still_takes_room() {
        let measure = FontMeasure::inter(11.0).unwrap();

        assert!(measure.width("\u{10FFFD}") > 0.0);
    }
}
