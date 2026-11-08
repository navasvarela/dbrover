//! A chart as SVG: the marks the tab paints, where it paints them, in [`FILE_COLOURS`] on
//! white. A PNG is drawn from it (`dbrover_picture::png`).

use std::fmt::{self, Write as _};

use dbrover_picture::{BASELINE_DROP, Escaped, FontMeasure, MeasureError, Points, Rgb, SANS};

use crate::palette::{BAR_ROUNDING, FILE_COLOURS, FILE_TEXT_SIZE, LINE_WIDTH, ROUNDED_BAR_WIDTH};
use crate::{Anchor, ChartData, Drawing, LayoutOptions, Mark, Point, Rect, Size, layout_file};

/// `data` drawn as a file draws it, with no window: laid out in `size` with the bundled font's
/// measure at [`FILE_TEXT_SIZE`], `note` in its last line, and written as SVG. What a report's
/// chart and the MCP server's `chart` tool write.
///
/// # Errors
///
/// [`MeasureError`] if the bundled font doesn't read, which only a broken build does.
pub fn file_svg(
    data: &ChartData,
    options: &LayoutOptions,
    size: Size,
    note: &str,
) -> Result<String, MeasureError> {
    let measure = FontMeasure::inter(FILE_TEXT_SIZE)?;
    let drawing = layout_file(data, options, size, &measure, note);
    Ok(svg(&drawing, FILE_TEXT_SIZE))
}

/// `drawing` as an SVG document, its text at `text_size` points, the size it was measured at.
/// Every number is written to a hundredth of a point ([`Points`]), so the same drawing is the
/// same bytes on every platform.
pub fn svg(drawing: &Drawing, text_size: f32) -> String {
    let mut out = Svg {
        text: String::new(),
        text_size,
    };
    let (width, height) = (
        Points(drawing.size.width.ceil()),
        Points(drawing.size.height.ceil()),
    );
    out.line(format_args!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" font-family=\"{SANS}\" font-size=\"{}\">",
        Points(text_size)
    ));
    out.line(format_args!(
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        FILE_COLOURS.background
    ));
    for mark in &drawing.marks {
        out.mark(mark);
    }
    out.text.push_str("</svg>\n");
    out.text
}

struct Svg {
    text: String,
    text_size: f32,
}

impl Svg {
    fn mark(&mut self, mark: &Mark) {
        match mark {
            Mark::Rect {
                rect,
                paint,
                rounded,
            } => self.rect(*rect, *rounded, FILE_COLOURS.of(*paint)),
            Mark::Line { from, to, paint } => self.line(format_args!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
                Points(from.x),
                Points(from.y),
                Points(to.x),
                Points(to.y),
                FILE_COLOURS.of(*paint)
            )),
            Mark::Polyline { points, paint } => self.polyline(points, FILE_COLOURS.of(*paint)),
            Mark::Circle {
                centre,
                radius,
                paint,
            } => self.line(format_args!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{}\"/>",
                Points(centre.x),
                Points(centre.y),
                Points(*radius),
                FILE_COLOURS.of(*paint)
            )),
            Mark::Text {
                at,
                text,
                anchor,
                paint,
            } => self.text(*at, text, *anchor, FILE_COLOURS.of(*paint)),
        }
    }

    fn rect(&mut self, rect: Rect, rounded: bool, fill: Rgb) {
        let rounding = if rounded && rect.width() >= ROUNDED_BAR_WIDTH {
            format!(" rx=\"{BAR_ROUNDING}\"")
        } else {
            String::new()
        };
        self.line(format_args!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"{rounding} fill=\"{fill}\"/>",
            Points(rect.min.x),
            Points(rect.min.y),
            Points(rect.width()),
            Points(rect.height())
        ));
    }

    fn polyline(&mut self, points: &[Point], stroke: Rgb) {
        let mut coordinates = String::new();
        for point in points {
            let _ = write!(coordinates, "{},{} ", Points(point.x), Points(point.y));
        }
        self.line(format_args!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{}\" \
             stroke-linejoin=\"round\" stroke-linecap=\"round\"/>",
            coordinates.trim_end(),
            Points(LINE_WIDTH)
        ));
    }

    fn text(&mut self, at: Point, text: &str, anchor: Anchor, fill: Rgb) {
        let anchor = match anchor {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        };
        // Marks place text by its middle; SVG places it by its baseline.
        let y = at.y + self.text_size * BASELINE_DROP;
        self.line(format_args!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"{anchor}\" fill=\"{fill}\">{}</text>",
            Points(at.x),
            Points(y),
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
    use dbrover_core::{Row, ValueKind};

    use super::*;
    use crate::{ChartData, ChartKind, Choice, Column, LayoutOptions, Role, Size, TextMeasure};

    struct Fixed;

    impl TextMeasure for Fixed {
        fn width(&self, text: &str) -> f32 {
            text.chars().count() as f32 * 6.0
        }

        fn line_height(&self) -> f32 {
            13.0
        }
    }

    fn columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|name| Column {
                name: (*name).to_owned(),
                kind: ValueKind::Undeclared,
            })
            .collect()
    }

    fn rows(cells: &[&[&str]]) -> Vec<Row> {
        cells
            .iter()
            .map(|row| row.iter().map(|cell| Some((*cell).to_owned())).collect())
            .collect()
    }

    fn laid_out(data: &ChartData) -> Drawing {
        let size = Size {
            width: 600.0,
            height: 300.0,
        };
        crate::layout(data, &LayoutOptions::default(), size, &Fixed)
    }

    /// Totals by month, as `kind`, laid out at 600 × 300.
    fn months(kind: ChartKind) -> Drawing {
        let rows = rows(&[
            &["2026-01", "120.5"],
            &["2026-02", "80"],
            &["<Q3> & co", "200"],
        ]);
        let borrowed: Vec<&Row> = rows.iter().collect();
        let choice = Choice::of(kind, 0, vec![1]);
        let roles = [Role::Text, Role::Number];
        laid_out(&ChartData::read(
            &columns(&["month", "total"]),
            &roles,
            &borrowed,
            &choice,
        ))
    }

    #[test]
    fn writes_bars_labels_and_escaped_text_on_white() {
        let svg = svg(&months(ChartKind::Bars), 11.0);

        assert!(
            svg.starts_with(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"600\" height=\"300\""
            )
        );
        assert!(svg.contains("font-size=\"11\">"));
        assert!(svg.contains("fill=\"#ffffff\""));
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
    fn writes_every_number_to_a_hundredth() {
        let svg = svg(&months(ChartKind::Lines), 11.0);

        let numbers = svg
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter(|word| word.contains('.') && word.chars().any(|c| c.is_ascii_digit()));
        for number in numbers {
            let decimals = number.rsplit('.').next().map_or(0, str::len);
            assert!(decimals <= 2, "{number} in {svg}");
        }
    }

    #[test]
    fn writes_stacked_bars_as_square_pieces() {
        let rows = rows(&[
            &["2026-09", "bikes", "1290"],
            &["2026-09", "parts", "42"],
            &["2026-10", "bikes", "649"],
        ]);
        let borrowed: Vec<&Row> = rows.iter().collect();
        let choice = Choice {
            series: Some(1),
            stack: true,
            ..Choice::of(ChartKind::Bars, 0, vec![2])
        };
        let roles = [Role::Text, Role::Text, Role::Number];
        let data = ChartData::read(
            &columns(&["month", "category", "net"]),
            &roles,
            &borrowed,
            &choice,
        );
        let svg = svg(&laid_out(&data), 11.0);

        let pieces: Vec<&str> = svg
            .lines()
            .filter(|line| line.starts_with("<rect x=") && line.contains("height"))
            .filter(|line| !line.contains("100%"))
            .collect();
        let square = pieces.iter().filter(|line| !line.contains(" rx=")).count();
        assert_eq!(square, 3, "{svg}");
        assert!(svg.contains(">bikes</text>") && svg.contains(">parts</text>"));
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

        let png = dbrover_picture::png(&svg, 2.0, &dbrover_picture::Fonts::bundled());

        assert!(png.is_ok(), "{png:?}");
    }
}
