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
/// How far above or below a box a line bends to pass it: inside the gap between two boxes
/// of a column, which is 32 points.
const CLEARANCE: f32 = 12.0;
/// How far a key of a table to itself loops out to the right.
const SELF_LOOP_REACH: f32 = 48.0;

/// The vertical middle of a row of the box, or of its header for `None`.
pub fn row_middle(rect: Rect, row: Option<usize>) -> f32 {
    match row {
        Some(row) => rect.top() + HEADER_HEIGHT + ROW_HEIGHT * (row as f32 + 0.5),
        None => rect.top() + HEADER_HEIGHT / 2.0,
    }
}

/// A cubic Bézier curve: start, two control points, end.
pub type Curve = [Pos2; 4];

/// A key's line, as curves end to end: from the side of the referencing box that faces the
/// referenced one, at `from_y`, to the facing side of the referenced box, at `to_y`, leaving
/// and arriving level. Where it would pass behind one of `boxes` on the way, it bends above or
/// below that box, whichever is nearer, so a line never looks as if it ended at a box it only
/// crosses. A key of a table to itself loops out of its right side and back.
pub fn key_path(from: Rect, from_y: f32, to: Rect, to_y: f32, boxes: &[Rect]) -> Vec<Curve> {
    if from == to {
        let x = from.right();
        return vec![[
            pos2(x, from_y),
            pos2(x + SELF_LOOP_REACH, from_y),
            pos2(x + SELF_LOOP_REACH, to_y),
            pos2(x, to_y),
        ]];
    }
    let leftwards = to.center().x < from.center().x;
    let (start, end) = if leftwards {
        (pos2(from.left(), from_y), pos2(to.right(), to_y))
    } else {
        (pos2(from.right(), from_y), pos2(to.left(), to_y))
    };
    let mut points = vec![start];
    points.extend(detours(start, end, boxes));
    points.push(end);
    points
        .windows(2)
        .map(|pair| level_curve(pair[0], pair[1]))
        .collect()
}

/// Where a line from `start` to `end` bends to pass the boxes wholly between them: above or
/// below each box it would cross, level with it from its near side to its far side, so the
/// curve into the bend can't clip the box's corner. In the order the line meets them.
fn detours(start: Pos2, end: Pos2, boxes: &[Rect]) -> Vec<Pos2> {
    let (low, high) = (start.x.min(end.x), start.x.max(end.x));
    let mut between: Vec<&Rect> = boxes
        .iter()
        .filter(|rect| rect.left() > low && rect.right() < high)
        .collect();
    between.sort_by(|a, b| {
        let distance = |rect: &Rect| (rect.center().x - start.x).abs();
        distance(a).total_cmp(&distance(b))
    });
    let mut detours = Vec::new();
    let mut last = start;
    for rect in between {
        let x = rect.center().x;
        let share = (x - last.x) / (end.x - last.x);
        let y = last.y + (end.y - last.y) * share;
        let (above, below) = (rect.top() - CLEARANCE, rect.bottom() + CLEARANCE);
        if y <= above || y >= below {
            continue;
        }
        let detour = if y - above <= below - y { above } else { below };
        let (near, far) = if end.x < start.x {
            (rect.right(), rect.left())
        } else {
            (rect.left(), rect.right())
        };
        detours.push(pos2(near, detour));
        last = pos2(far, detour);
        detours.push(last);
    }
    detours
}

/// A curve from `start` to `end` that leaves and arrives level.
fn level_curve(start: Pos2, end: Pos2) -> Curve {
    let outwards = if end.x < start.x { -1.0 } else { 1.0 };
    let reach = ((end.x - start.x).abs() / 2.0).max(MIN_REACH);
    [
        start,
        pos2(start.x + outwards * reach, start.y),
        pos2(end.x - outwards * reach, end.y),
        end,
    ]
}

/// The arrowhead at the end of a curve, pointing into the referenced box: its tip and the
/// two corners behind it.
pub fn arrowhead(curve: &Curve, size: f32) -> [Pos2; 3] {
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
            let [curve] = key_path(from, 10.0, to, 20.0, &[]).try_into().unwrap();

            assert_eq!((curve[0].x, curve[3].x), (start_x, end_x), "{case}");
            assert_eq!((curve[0].y, curve[3].y), (10.0, 20.0), "{case}");
            assert_eq!(curve[1].y, curve[0].y, "{case}: leaves level");
            assert_eq!(curve[2].y, curve[3].y, "{case}: arrives level");
        }
    }

    #[test]
    fn a_key_of_a_table_to_itself_loops_out_of_its_right_side() {
        let table = rect(0.0, 0.0);

        let [curve] = key_path(table, 50.0, table, 38.0, &[]).try_into().unwrap();

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
        let path = key_path(rect(300.0, 0.0), 10.0, rect(0.0, 0.0), 10.0, &[]);
        let curve = path.last().unwrap();

        let [tip, a, b] = arrowhead(curve, 8.0);

        assert_eq!(tip, pos2(100.0, 10.0));
        assert!(
            a.x > tip.x && b.x > tip.x,
            "behind the tip, outside the box"
        );
        assert_eq!(a.y + b.y, 20.0, "on both sides of the line");
    }

    #[test]
    fn a_line_bends_around_a_box_it_would_pass_behind() {
        let from = rect(600.0, 100.0);
        let to = rect(0.0, 100.0);
        let in_the_way = Rect::from_min_size(pos2(300.0, 100.0), vec2(100.0, 100.0));
        let out_of_the_way = rect(300.0, 400.0);

        let path = key_path(from, 120.0, to, 130.0, &[in_the_way, out_of_the_way]);

        assert_eq!(path.len(), 3, "into the bend, past the box, out of it");
        assert_eq!(path[0][3], pos2(400.0, 88.0), "above, the nearer side");
        assert_eq!(path[1][3], pos2(300.0, 88.0), "level past the box");
        assert_eq!(path[1][0], path[0][3], "end to end");
        assert_eq!((path[0][0].x, path[2][3].x), (600.0, 100.0));
    }

    #[test]
    fn a_line_clear_of_every_box_is_one_curve() {
        let beside = Rect::from_min_size(pos2(300.0, 300.0), vec2(100.0, 100.0));

        let path = key_path(rect(600.0, 0.0), 20.0, rect(0.0, 0.0), 20.0, &[beside]);

        assert_eq!(path.len(), 1);
    }
}
