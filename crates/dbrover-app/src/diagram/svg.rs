//! A diagram as SVG: the boxes and lines the tab draws, where it draws them, in the light
//! theme's colours on white, so the picture reads in a document or a pull request whatever the
//! app's theme. PNG exports are drawn from it.

use std::fmt::{self, Write as _};

use dbrover_core::TableDescription;
use egui::{Color32, Pos2, Rect, Vec2, vec2};

use super::geometry::{self, Curve, HEADER_HEIGHT, MARK_WIDTH, PADDING, arrowhead};
use super::model::{Diagram, DiagramTable, TableState};
use crate::theme::Palette;

/// Around the diagram, inside the picture.
const MARGIN: f32 = 24.0;
const SANS: &str = "Inter, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif";
const MONO: &str = "'JetBrains Mono', Menlo, Consolas, monospace";
/// The tab's sizes, so the picture's text fits the boxes the tab measured.
const HEADER_TEXT_SIZE: f32 = 13.0;
const COLUMN_TEXT_SIZE: f32 = 12.5;
const TYPE_TEXT_SIZE: f32 = 11.5;
const MARK_TEXT_SIZE: f32 = 8.5;
/// From a line of text's middle down to its baseline, as a share of its size: about where
/// Inter's and JetBrains Mono's lower-case letters sit. Not `dominant-baseline`, which resvg and
/// browsers read differently.
const BASELINE_DROP: f32 = 0.35;
const CORNER_RADIUS: f32 = 6.0;
const ARROW_SIZE: f32 = 7.0;

/// The diagram as an SVG document, or `None` while no box is placed.
pub fn svg(diagram: &Diagram) -> Option<String> {
    let bounds = diagram.bounds()?.expand(MARGIN);
    let palette = Palette::LIGHT;
    let mut out = Svg {
        text: String::new(),
        offset: -bounds.min.to_vec2(),
        palette,
    };
    out.open(bounds.size());
    for line in diagram.key_lines() {
        if let Some(path) = diagram.key_path(line) {
            out.key_line(&path);
        }
    }
    for (index, table) in diagram.tables().iter().enumerate() {
        if let Some(rect) = diagram.rect(index) {
            out.table(rect, &diagram.label(&table.name), table);
        }
    }
    out.text.push_str("</svg>\n");
    Some(out.text)
}

struct Svg {
    text: String,
    /// Moves the diagram's coordinates so its bounds start at the picture's origin.
    offset: Vec2,
    palette: Palette,
}

impl Svg {
    fn open(&mut self, size: Vec2) {
        let (width, height) = (size.x.ceil(), size.y.ceil());
        self.line(format_args!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
             viewBox=\"0 0 {width} {height}\" font-family=\"{SANS}\">"
        ));
        self.line(format_args!(
            "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
            hex(self.palette.field)
        ));
    }

    fn key_line(&mut self, path: &[Curve]) {
        let (Some(first), Some(last)) = (path.first(), path.last()) else {
            return;
        };
        let mut d = String::new();
        let start = first[0] + self.offset;
        let _ = write!(d, "M{} {}", start.x, start.y);
        for curve in path {
            let [_, a, b, end] = curve.map(|point| point + self.offset);
            let _ = write!(d, " C{} {} {} {} {} {}", a.x, a.y, b.x, b.y, end.x, end.y);
        }
        let color = hex(self.palette.text_weak);
        self.line(format_args!(
            "<path d=\"{d}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.2\"/>"
        ));
        let [tip, left, right] = arrowhead(last, ARROW_SIZE).map(|point| point + self.offset);
        self.line(format_args!(
            "<path d=\"M{} {} L{} {} L{} {} Z\" fill=\"{color}\"/>",
            tip.x, tip.y, left.x, left.y, right.x, right.y
        ));
    }

    fn table(&mut self, rect: Rect, label: &str, table: &DiagramTable) {
        let rect = rect.translate(self.offset);
        let palette = self.palette;
        self.line(format_args!("<g>"));
        self.rect(rect, &hex(palette.field), None);
        // The header's top corners are round and its bottom ones square: a round rectangle
        // with a square one over its lower half.
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_HEIGHT));
        let header_fill = hex(palette.selection);
        self.rect(header, &header_fill, None);
        let lower_half = Rect::from_min_max(header.left_center(), header.max);
        self.square_rect(lower_half, &header_fill);
        let dashed = table.state == TableState::Missing;
        let border = if dashed {
            palette.warning
        } else {
            palette.border
        };
        self.rect(rect, "none", Some((&hex(border), dashed)));
        self.table_mark(rect, header.center().y);
        let x = rect.left() + PADDING + MARK_WIDTH;
        self.text(
            vec2(x, header.center().y),
            label,
            HEADER_TEXT_SIZE,
            palette.text,
            "font-weight=\"600\"",
        );
        match &table.state {
            TableState::Described(description) => {
                let type_x = table.shape.map_or(0.0, |shape| shape.type_x);
                self.columns(rect, type_x, description);
            }
            TableState::Loading | TableState::Missing => {
                let note = if dashed { "missing" } else { "Loading…" };
                let y = geometry::row_middle(rect, Some(0));
                self.text(vec2(x, y), note, COLUMN_TEXT_SIZE, palette.text_weak, "");
            }
            TableState::Failed(error) => {
                let y = geometry::row_middle(rect, Some(0));
                self.text(vec2(x, y), error, COLUMN_TEXT_SIZE, palette.error, "");
            }
        }
        self.line(format_args!("</g>"));
    }

    fn columns(&mut self, rect: Rect, type_x: f32, description: &TableDescription) {
        let palette = self.palette;
        for (row, column) in description.columns.iter().enumerate() {
            let y = geometry::row_middle(rect, Some(row));
            let primary_key = description.primary_key.contains(&column.name);
            let foreign_key = description
                .foreign_keys
                .iter()
                .any(|key| key.columns.contains(&column.name));
            let mark = match (primary_key, foreign_key) {
                (true, _) => Some(("PK", palette.warning)),
                (false, true) => Some(("FK", palette.secondary)),
                (false, false) => None,
            };
            if let Some((mark, color)) = mark {
                let at = vec2(rect.left() + PADDING, y);
                self.text(at, mark, MARK_TEXT_SIZE, color, "font-weight=\"600\"");
            }
            let name_at = vec2(rect.left() + PADDING + MARK_WIDTH, y);
            self.text(name_at, &column.name, COLUMN_TEXT_SIZE, palette.text, "");
            let mono = format!("font-family=\"{MONO}\"");
            let type_at = vec2(rect.left() + type_x, y);
            self.text(
                type_at,
                &column.data_type,
                TYPE_TEXT_SIZE,
                palette.text_weak,
                &mono,
            );
        }
    }

    /// A small table drawn where the tab shows its table icon.
    fn table_mark(&mut self, rect: Rect, middle: f32) {
        let size = 10.0;
        let mark = Rect::from_min_size(
            Pos2::new(rect.left() + PADDING, middle - size / 2.0),
            Vec2::splat(size),
        );
        let color = hex(self.palette.primary);
        self.line(format_args!(
            "<rect x=\"{}\" y=\"{}\" width=\"{size}\" height=\"{size}\" rx=\"1.5\" \
             fill=\"none\" stroke=\"{color}\" stroke-width=\"1.2\"/>",
            mark.left(),
            mark.top()
        ));
        let rule = mark.top() + size / 3.0;
        self.line(format_args!(
            "<path d=\"M{} {rule} H{}\" stroke=\"{color}\" stroke-width=\"1.2\"/>",
            mark.left(),
            mark.right()
        ));
    }

    /// A rectangle with round corners, filled, and stroked when `stroke` is given: its colour,
    /// and whether it is dashed.
    fn rect(&mut self, rect: Rect, fill: &str, stroke: Option<(&str, bool)>) {
        let stroke = match stroke {
            Some((color, true)) => format!(" stroke=\"{color}\" stroke-dasharray=\"6 4\""),
            Some((color, false)) => format!(" stroke=\"{color}\""),
            None => String::new(),
        };
        self.line(format_args!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{CORNER_RADIUS}\" \
             fill=\"{fill}\"{stroke}/>",
            rect.left(),
            rect.top(),
            rect.width(),
            rect.height()
        ));
    }

    fn square_rect(&mut self, rect: Rect, fill: &str) {
        self.line(format_args!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\"/>",
            rect.left(),
            rect.top(),
            rect.width(),
            rect.height()
        ));
    }

    /// `content` starting at `at`'s x, its middle at `at`'s y.
    fn text(&mut self, at: Vec2, content: &str, size: f32, color: Color32, extra: &str) {
        let baseline = at.y + size * BASELINE_DROP;
        self.line(format_args!(
            "<text x=\"{}\" y=\"{baseline}\" font-size=\"{size}\" fill=\"{}\" {extra}>{}</text>",
            at.x,
            hex(color),
            Escaped(content)
        ));
    }

    fn line(&mut self, line: fmt::Arguments<'_>) {
        // Writing into a String can't fail.
        let _ = self.text.write_fmt(line);
        self.text.push('\n');
    }
}

fn hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Text as XML character data: `&`, `<` and `>` escaped, and the control characters XML
/// can't hold dropped.
struct Escaped<'a>(&'a str);

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

#[cfg(test)]
mod tests {
    use dbrover_core::{ColumnDescription, DataSourceId, ForeignKey, TableName};
    use dbrover_engine::{Command, DescribedTable};

    use super::*;
    use crate::diagram::model::BoxShape;

    fn name(table: &str) -> TableName {
        TableName {
            schema: "public".to_owned(),
            name: table.to_owned(),
        }
    }

    fn description(table: &str, columns: &[&str], keys: &[(&str, &str)]) -> TableDescription {
        TableDescription {
            schema: "public".to_owned(),
            name: table.to_owned(),
            columns: columns
                .iter()
                .map(|column| ColumnDescription {
                    name: (*column).to_owned(),
                    data_type: "integer".to_owned(),
                    nullable: true,
                    default: None,
                    identity: None,
                })
                .collect(),
            primary_key: vec!["id".to_owned()],
            foreign_keys: keys
                .iter()
                .map(|(column, to)| ForeignKey {
                    table: name(table),
                    columns: vec![(*column).to_owned()],
                    referenced: name(to),
                    referenced_columns: vec!["id".to_owned()],
                })
                .collect(),
            referenced_by: vec![],
        }
    }

    /// `orders` pointing at `<customers>`, a table whose name needs escaping, and `gone`,
    /// missing; measured and placed as the tab would.
    fn diagram() -> Diagram {
        let tables = vec![name("orders"), name("<customers>"), name("gone")];
        let mut diagram = Diagram::new(
            DataSourceId::generate(),
            Some("public".to_owned()),
            tables,
            None,
        );
        let Some(Command::DescribeTables { tables, .. }) = diagram.next_batch() else {
            panic!("asks for the tables");
        };
        let results = [
            Ok(Some(description(
                "orders",
                &["id", "customer_id"],
                &[("customer_id", "<customers>")],
            ))),
            Ok(Some(description("<customers>", &["id", "name"], &[]))),
            Ok(None),
        ];
        let answers: Vec<DescribedTable> = tables
            .into_iter()
            .zip(results)
            .map(|(table, result)| DescribedTable { table, result })
            .collect();
        diagram.described(&answers);
        for index in 0..3 {
            diagram.set_shape(
                index,
                BoxShape {
                    size: vec2(150.0, 80.0),
                    type_x: 90.0,
                },
            );
        }
        diagram.place();
        diagram
    }

    #[test]
    fn writes_the_boxes_and_lines_the_tab_draws() {
        let svg = svg(&diagram()).unwrap();

        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.contains(">orders</text>"));
        assert!(svg.contains(">&lt;customers&gt;</text>"), "escaped");
        assert!(svg.contains(">customer_id</text>"));
        assert!(svg.contains(">PK</text>") && svg.contains(">FK</text>"));
        assert!(
            svg.contains("stroke-dasharray"),
            "the missing table is dashed"
        );
        assert!(svg.contains(">missing</text>"));
        let curves = svg
            .lines()
            .filter(|line| line.starts_with("<path d=\"M") && line.contains(" C"));
        assert_eq!(curves.count(), 1, "one key, one curve");
        assert_eq!(svg.matches("<g>").count(), 3);
    }

    #[test]
    fn starts_at_the_pictures_origin_with_a_margin() {
        let diagram = diagram();
        let bounds = diagram.bounds().unwrap();

        let svg = svg(&diagram).unwrap();

        let width = (bounds.width() + 2.0 * MARGIN).ceil();
        assert!(svg.contains(&format!("width=\"{width}\"")), "{svg}");
        assert!(
            svg.contains(&format!("<rect x=\"{MARGIN}\"")),
            "the leftmost box: {svg}"
        );
    }

    #[test]
    fn draws_as_a_png_with_the_bundled_fonts() {
        let svg = svg(&diagram()).unwrap();

        let png = dbrover_diagram::png(&svg, 2.0, &crate::fonts::bundled_files());

        assert!(png.is_ok(), "{png:?}");
    }

    #[test]
    fn nothing_placed_is_no_picture() {
        let diagram = Diagram::new(DataSourceId::generate(), None, vec![name("orders")], None);

        assert!(svg(&diagram).is_none());
    }
}
