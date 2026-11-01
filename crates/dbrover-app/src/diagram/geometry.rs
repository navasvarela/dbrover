//! Where a box's rows are and how a key's line runs between two boxes: the same for the tab
//! and for an exported picture.

use egui::{Pos2, Rect, pos2, vec2};

pub const HEADER_HEIGHT: f32 = 28.0;
pub const ROW_HEIGHT: f32 = 20.0;
/// Inside a box, left and right of its text.
pub const PADDING: f32 = 10.0;
/// The space a key or link mark takes before a column's name.
pub const MARK_WIDTH: f32 = 18.0;
/// Between the longest column name and the types.
pub const TYPE_GAP: f32 = 16.0;
/// Between a diagram and tables added to it after the user moved its boxes.
pub const NEW_TABLES_GAP: f32 = 96.0;
/// How far a line leaves a box straight before it curves, at the least.
const MIN_REACH: f32 = 32.0;
/// How far a key of a table to itself loops out to the right.
const SELF_LOOP_REACH: f32 = 48.0;

/// The vertical middle of a row of the box, or of its header for `None`.
pub fn row_middle(rect: Rect, row: Option<usize>) -> f32 {
    match row {
        Some(row) => rect.top() + HEADER_HEIGHT + ROW_HEIGHT * (row as f32 + 0.5),
        None => rect.top() + HEADER_HEIGHT / 2.0,
    }
}

/// A key's line as a cubic Bézier curve: from the side of the referencing box that faces the
/// referenced one, at `from_y`, to the facing side of the referenced box, at `to_y`, leaving
/// and arriving level. A key of a table to itself loops out of its right side and back.
pub fn key_curve(from: Rect, from_y: f32, to: Rect, to_y: f32) -> [Pos2; 4] {
    if from == to {
        let x = from.right();
        return [
            pos2(x, from_y),
            pos2(x + SELF_LOOP_REACH, from_y),
            pos2(x + SELF_LOOP_REACH, to_y),
            pos2(x, to_y),
        ];
    }
    let leftwards = to.center().x < from.center().x;
    let (start_x, end_x, outwards) = if leftwards {
        (from.left(), to.right(), -1.0)
    } else {
        (from.right(), to.left(), 1.0)
    };
    let reach = ((end_x - start_x).abs() / 2.0).max(MIN_REACH);
    [
        pos2(start_x, from_y),
        pos2(start_x + outwards * reach, from_y),
        pos2(end_x - outwards * reach, to_y),
        pos2(end_x, to_y),
    ]
}

/// The arrowhead at the end of a curve, pointing into the referenced box: its tip and the
/// two corners behind it.
pub fn arrowhead(curve: &[Pos2; 4], size: f32) -> [Pos2; 3] {
    let tip = curve[3];
    let direction = (curve[3] - curve[2]).normalized();
    let back = tip - direction * size;
    let across = vec2(-direction.y, direction.x) * (size / 2.0);
    [tip, back + across, back - across]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(100.0, 80.0))
    }

    #[test]
    fn a_line_leaves_and_enters_the_sides_that_face_each_other() {
        let left = rect(0.0, 0.0);
        let right = rect(300.0, 0.0);
        let cases = [
            (right, left, 300.0, 100.0, "pointing left"),
            (left, right, 100.0, 300.0, "pointing right"),
        ];
        for (from, to, start_x, end_x, case) in cases {
            let curve = key_curve(from, 10.0, to, 20.0);

            assert_eq!((curve[0].x, curve[3].x), (start_x, end_x), "{case}");
            assert_eq!((curve[0].y, curve[3].y), (10.0, 20.0), "{case}");
            assert_eq!(curve[1].y, curve[0].y, "{case}: leaves level");
            assert_eq!(curve[2].y, curve[3].y, "{case}: arrives level");
        }
    }

    #[test]
    fn a_key_of_a_table_to_itself_loops_out_of_its_right_side() {
        let table = rect(0.0, 0.0);

        let curve = key_curve(table, 50.0, table, 38.0);

        assert_eq!(curve[0], pos2(100.0, 50.0));
        assert_eq!(curve[3], pos2(100.0, 38.0));
        assert!(curve[1].x > 100.0 && curve[2].x > 100.0);
    }

    #[test]
    fn rows_are_below_the_header() {
        let table = rect(0.0, 100.0);

        assert_eq!(row_middle(table, None), 114.0);
        assert_eq!(row_middle(table, Some(0)), 138.0);
        assert_eq!(row_middle(table, Some(2)), 178.0);
    }

    #[test]
    fn an_arrowhead_points_where_the_curve_arrives() {
        let curve = key_curve(rect(300.0, 0.0), 10.0, rect(0.0, 0.0), 10.0);

        let [tip, a, b] = arrowhead(&curve, 8.0);

        assert_eq!(tip, pos2(100.0, 10.0));
        assert!(
            a.x > tip.x && b.x > tip.x,
            "behind the tip, outside the box"
        );
        assert_eq!(a.y + b.y, 20.0, "on both sides of the line");
    }
}
