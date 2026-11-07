//! Measuring a chart's text with the bundled font's file, for a chart laid out with no window.

use dbrover_picture::FontMeasure;

use crate::TextMeasure;

impl TextMeasure for FontMeasure {
    fn width(&self, text: &str) -> f32 {
        FontMeasure::width(self, text)
    }

    fn line_height(&self) -> f32 {
        FontMeasure::line_height(self)
    }
}
