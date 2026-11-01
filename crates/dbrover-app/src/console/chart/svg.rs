//! A chart as SVG: the marks the tab paints, where it paints them, in the light theme's colours
//! on white, so the picture reads in a document or a pull request whatever the app's theme. PNG
//! exports are drawn from it.

use std::fmt::{self, Write as _};

use dbrover_chart::{Anchor, Drawing, Mark, Point, Rect};

use super::{BAR_ROUNDING, LINE_WIDTH, Paints, ROUNDED_BAR_WIDTH};
use crate::picture::{BASELINE_DROP, Escaped, SANS, hex};
use crate::theme::Palette;

/// `drawing` as an SVG document, its text at `text_size` points, the size the tab measured it
/// with.
pub fn svg(drawing: &Drawing, text_size: f32) -> String {
    let palette = Palette::LIGHT;
    let mut out = Svg {
        text: String::new(),
        paints: Paints::of(&palette),
        text_size,
    };
    let (width, height) = (drawing.size.width.ceil(), drawing.size.height.ceil());
    out.line(format_args!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" font-family=\"{SANS}\" font-size=\"{text_size}\">"
    ));
    out.line(format_args!(
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        hex(palette.field)
    ));
    for mark in &drawing.marks {
        out.mark(mark);
    }
    out.text.push_str("</svg>\n");
    out.text
}

struct Svg {
    text: String,
    paints: Paints,
    text_size: f32,
}

impl Svg {
    fn mark(&mut self, mark: &Mark) {
        match mark {
            Mark::Rect { rect, paint } => self.rect(*rect, &hex(self.paints.colour(*paint))),
            Mark::Line { from, to, paint } => self.line(format_args!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
                from.x,
                from.y,
                to.x,
                to.y,
                hex(self.paints.colour(*paint))
            )),
            Mark::Polyline { points, paint } => {
                self.polyline(points, &hex(self.paints.colour(*paint)))
            }
            Mark::Circle {
                centre,
                radius,
                paint,
            } => self.line(format_args!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{radius}\" fill=\"{}\"/>",
                centre.x,
                centre.y,
                hex(self.paints.colour(*paint))
            )),
            Mark::Text {
                at,
                text,
                anchor,
                paint,
            } => self.text(*at, text, *anchor, &hex(self.paints.colour(*paint))),
        }
    }

    fn rect(&mut self, rect: Rect, fill: &str) {
        let rounding = if rect.width() >= ROUNDED_BAR_WIDTH {
            format!(" rx=\"{BAR_ROUNDING}\"")
        } else {
            String::new()
        };
        self.line(format_args!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"{rounding} fill=\"{fill}\"/>",
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height()
        ));
    }

    fn polyline(&mut self, points: &[Point], stroke: &str) {
        let mut coordinates = String::new();
        for point in points {
            let _ = write!(coordinates, "{},{} ", point.x, point.y);
        }
        self.line(format_args!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{stroke}\" \
             stroke-width=\"{LINE_WIDTH}\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/>",
            coordinates.trim_end()
        ));
    }

    fn text(&mut self, at: Point, text: &str, anchor: Anchor, fill: &str) {
        let anchor = match anchor {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        };
        // Marks place text by its middle; SVG places it by its baseline.
        let y = at.y + self.text_size * BASELINE_DROP;
        self.line(format_args!(
            "<text x=\"{}\" y=\"{y}\" text-anchor=\"{anchor}\" fill=\"{fill}\">{}</text>",
            at.x,
            Escaped(text)
        ));
    }

    fn line(&mut self, element: fmt::Arguments<'_>) {
        let _ = self.text.write_fmt(element);
        self.text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use dbrover_chart::{ChartData, ChartKind, Choice, Column, Role, Size, TextMeasure, layout};
    use dbrover_core::ValueKind;

    use super::*;

    struct Fixed;

    impl TextMeasure for Fixed {
        fn width(&self, text: &str) -> f32 {
            text.chars().count() as f32 * 6.0
        }

        fn line_height(&self) -> f32 {
            13.0
        }
    }

    /// Totals by month, as `kind`, laid out at 600 × 300.
    fn months(kind: ChartKind) -> Drawing {
        let columns = ["month", "total"].map(|name| Column {
            name: name.to_owned(),
            kind: ValueKind::Undeclared,
        });
        let rows: Vec<dbrover_core::Row> = [
            ("2026-01", "120.5"),
            ("2026-02", "80"),
            ("<Q3> & co", "200"),
        ]
        .into_iter()
        .map(|(month, total)| vec![Some(month.to_owned()), Some(total.to_owned())])
        .collect();
        let borrowed: Vec<&dbrover_core::Row> = rows.iter().collect();
        let choice = Choice {
            kind,
            x: 0,
            ys: vec![1],
        };
        let data = ChartData::read(&columns, &[Role::Text, Role::Number], &borrowed, &choice);
        layout(
            &data,
            Size {
                width: 600.0,
                height: 300.0,
            },
            &Fixed,
        )
    }

    #[test]
    fn writes_bars_labels_and_escaped_text_on_white() {
        let svg = svg(&months(ChartKind::Bars), 11.0);

        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"600\""));
        assert!(svg.contains(&format!("fill=\"{}\"", hex(Palette::LIGHT.field))));
        let bars = svg
            .lines()
            .filter(|line| line.starts_with("<rect x=") && line.contains(" rx="))
            .count();
        assert_eq!(bars, 3);
        for label in ["2026-01", "month", "total", "&lt;Q3&gt; &amp; co"] {
            assert!(
                svg.contains(&format!(">{label}</text>")),
                "{label} in the SVG"
            );
        }
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn writes_a_line_as_a_polyline() {
        let svg = svg(&months(ChartKind::Lines), 11.0);

        assert_eq!(svg.matches("<polyline ").count(), 1);
        assert_eq!(svg.matches("<circle ").count(), 3);
    }

    #[test]
    fn draws_as_a_png_with_the_bundled_fonts() {
        let svg = svg(&months(ChartKind::Bars), 11.0);

        let png = dbrover_picture::png(&svg, 2.0, &crate::fonts::bundled_files());

        assert!(png.is_ok(), "{png:?}");
    }
}
