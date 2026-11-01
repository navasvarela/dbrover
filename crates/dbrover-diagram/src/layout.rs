//! A layered layout, by Sugiyama's method, with layers as columns, left to right:
//!
//! 1. Keys become edges from the referencing table to the referenced one, one per pair of
//!    tables, self-references left out.
//! 2. Cycles are broken by reversing the edges a depth-first walk finds closing one.
//! 3. Each table joined to another gets a layer by the longest path to a table that references
//!    nothing, which is in layer 0. An edge spanning several layers gets a dummy vertex in each
//!    layer between.
//! 4. Each layer is ordered to reduce crossings, by sweeping the layers in barycentre order.
//! 5. Layers become columns of boxes, and the tables no key joins go in columns after them.

use std::collections::HashSet;

use crate::{Point, Size};

/// Room between columns for the lines that cross from one to the next.
const COLUMN_GAP: f32 = 96.0;
/// Room between boxes in a column.
const ROW_GAP: f32 = 32.0;
/// How many times the ordering sweeps the layers, each time both ways. Crossings rarely drop
/// after a handful of sweeps; the best order seen is kept, so more only costs time.
const SWEEPS: usize = 12;

/// A table in a [`Graph`], as [`Graph::add`] returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(usize);

/// Tables as boxes of a known size, and the keys that join them.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    sizes: Vec<Size>,
    /// From the referencing table to the referenced one, in the order joined.
    keys: Vec<(NodeId, NodeId)>,
}

impl Graph {
    /// Adds a table whose box is `size`. Tables added first come first in a layer, before the
    /// ordering moves them, and walking for cycles starts from them: adding tables in name
    /// order lays a schema out the same way every time.
    pub fn add(&mut self, size: Size) -> NodeId {
        self.sizes.push(size);
        NodeId(self.sizes.len() - 1)
    }

    /// Records a key of `referencing` that points at `referenced`. Several keys between the
    /// same two tables count once, and a key of a table to itself doesn't move it.
    pub fn join(&mut self, referencing: NodeId, referenced: NodeId) {
        self.keys.push((referencing, referenced));
    }

    /// Where each table's box goes, in the order the tables were added.
    pub fn layout(&self) -> Vec<Point> {
        let edges = break_cycles(self.sizes.len(), &self.edges());
        let layers = longest_path_layers(self.sizes.len(), &edges);
        let vertices = Vertices::new(&layers, &edges);
        let columns = vertices.ordered_layers();
        let mut positions = vec![Point::default(); self.sizes.len()];
        let placed = place_columns(&columns, &self.sizes, &mut positions);
        let unjoined: Vec<usize> = (0..self.sizes.len())
            .filter(|&node| layers[node].is_none())
            .collect();
        place_unjoined(&unjoined, &self.sizes, placed, &mut positions);
        positions
    }

    /// The keys as edges between node indices: one per ordered pair of tables, in the order
    /// first joined, without self-references.
    fn edges(&self) -> Vec<(usize, usize)> {
        let mut seen = HashSet::new();
        self.keys
            .iter()
            .map(|&(NodeId(from), NodeId(to))| (from, to))
            .filter(|&(from, to)| from != to && seen.insert((from, to)))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    OnPath,
    Done,
}

/// The edges with every edge that closes a cycle reversed, so none is left, and the doubles
/// that reversing makes dropped. Walks depth first from each node in turn, following edges in
/// the order given: the edge reversed is the one found last, the one leading back to a node
/// still on the walk's path.
fn break_cycles(nodes: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut outgoing = vec![Vec::new(); nodes];
    for (index, &(from, _)) in edges.iter().enumerate() {
        outgoing[from].push(index);
    }
    let mut visit = vec![Visit::New; nodes];
    let mut reversed = vec![false; edges.len()];
    for root in 0..nodes {
        if visit[root] != Visit::New {
            continue;
        }
        visit[root] = Visit::OnPath;
        let mut path = vec![(root, 0)];
        while let Some(&(node, next)) = path.last() {
            let Some(&edge) = outgoing[node].get(next) else {
                visit[node] = Visit::Done;
                path.pop();
                continue;
            };
            if let Some(top) = path.last_mut() {
                top.1 += 1;
            }
            let target = edges[edge].1;
            match visit[target] {
                Visit::New => {
                    visit[target] = Visit::OnPath;
                    path.push((target, 0));
                }
                Visit::OnPath => reversed[edge] = true,
                Visit::Done => {}
            }
        }
    }
    let mut seen = HashSet::new();
    edges
        .iter()
        .zip(reversed)
        .map(|(&(from, to), reversed)| if reversed { (to, from) } else { (from, to) })
        .filter(|&edge| seen.insert(edge))
        .collect()
}

/// Each node's layer: 0 for a node with no outgoing edge, otherwise one more than the
/// furthest node its edges lead to. `None` for a node no edge touches. `edges` has no cycle.
fn longest_path_layers(nodes: usize, edges: &[(usize, usize)]) -> Vec<Option<usize>> {
    let mut outgoing_left = vec![0usize; nodes];
    let mut incoming = vec![Vec::new(); nodes];
    let mut joined = vec![false; nodes];
    for &(from, to) in edges {
        outgoing_left[from] += 1;
        incoming[to].push(from);
        joined[from] = true;
        joined[to] = true;
    }
    let mut layers = vec![0usize; nodes];
    // Kahn's algorithm from the referenced end: a node is settled once every node it points
    // at is, and its layer is final then because every edge out of it has been seen.
    let mut settled: Vec<usize> = (0..nodes).filter(|&n| outgoing_left[n] == 0).collect();
    while let Some(node) = settled.pop() {
        for &from in &incoming[node] {
            layers[from] = layers[from].max(layers[node] + 1);
            outgoing_left[from] -= 1;
            if outgoing_left[from] == 0 {
                settled.push(from);
            }
        }
    }
    layers
        .into_iter()
        .zip(joined)
        .map(|(layer, joined)| joined.then_some(layer))
        .collect()
}

/// The tables in layers, with a dummy vertex wherever an edge passes through a layer. Vertex
/// `v` is the table `v` while `v` is below the number of tables, and a dummy after that.
struct Vertices {
    layer_of: Vec<usize>,
    /// Each layer's vertices, in the order they start in.
    layers: Vec<Vec<usize>>,
    /// The vertices each vertex's edges lead to, one layer to the left.
    left: Vec<Vec<usize>>,
    /// The vertices whose edges lead to each vertex, one layer to the right.
    right: Vec<Vec<usize>>,
}

impl Vertices {
    fn new(layers: &[Option<usize>], edges: &[(usize, usize)]) -> Self {
        let mut vertices = Self {
            layer_of: layers.iter().map(|layer| layer.unwrap_or(0)).collect(),
            layers: Vec::new(),
            left: vec![Vec::new(); layers.len()],
            right: vec![Vec::new(); layers.len()],
        };
        for &(from, to) in edges {
            let mut previous = from;
            for layer in (vertices.layer_of[to] + 1..vertices.layer_of[from]).rev() {
                let dummy = vertices.push_dummy(layer);
                vertices.link(previous, dummy);
                previous = dummy;
            }
            vertices.link(previous, to);
        }
        let joined = layers
            .iter()
            .enumerate()
            .filter_map(|(v, l)| Some((v, (*l)?)));
        let dummies = (layers.len()..vertices.layer_of.len()).map(|v| (v, vertices.layer_of[v]));
        let count = layers.iter().flatten().max().map_or(0, |last| last + 1);
        vertices.layers = vec![Vec::new(); count];
        for (vertex, layer) in joined.chain(dummies).collect::<Vec<_>>() {
            vertices.layers[layer].push(vertex);
        }
        vertices
    }

    fn push_dummy(&mut self, layer: usize) -> usize {
        self.layer_of.push(layer);
        self.left.push(Vec::new());
        self.right.push(Vec::new());
        self.layer_of.len() - 1
    }

    fn link(&mut self, from: usize, to: usize) {
        self.left[from].push(to);
        self.right[to].push(from);
    }

    /// The layers, each in the order with the fewest crossings the sweeps found.
    fn ordered_layers(&self) -> Vec<Vec<usize>> {
        let mut order = self.layers.clone();
        let mut position = vec![0usize; self.layer_of.len()];
        for layer in &order {
            set_positions(layer, &mut position);
        }
        let mut best = (self.crossings(&order, &position), order.clone());
        for _ in 0..SWEEPS {
            if best.0 == 0 {
                break;
            }
            for layer in order.iter_mut().skip(1) {
                sort_by_barycentre(layer, &self.left, &mut position);
            }
            for layer in order.iter_mut().rev().skip(1) {
                sort_by_barycentre(layer, &self.right, &mut position);
            }
            let crossings = self.crossings(&order, &position);
            if crossings < best.0 {
                best = (crossings, order.clone());
            }
        }
        best.1
    }

    /// How many pairs of edges cross between neighbouring layers, at these positions.
    fn crossings(&self, order: &[Vec<usize>], position: &[usize]) -> usize {
        order
            .iter()
            .skip(1)
            .map(|layer| {
                let mut segments: Vec<(usize, usize)> = layer
                    .iter()
                    .flat_map(|&v| {
                        self.left[v]
                            .iter()
                            .map(move |&to| (position[v], position[to]))
                    })
                    .collect();
                segments.sort_unstable();
                inversions(segments.into_iter().map(|(_, to)| to))
            })
            .sum()
    }
}

/// Sorts `layer` by the mean position of each vertex's neighbours in the layer beside it,
/// keeping a vertex with none where it is, and records the new positions.
fn sort_by_barycentre(layer: &mut [usize], neighbours: &[Vec<usize>], position: &mut [usize]) {
    let barycentre = |vertex: usize| -> f32 {
        let beside = &neighbours[vertex];
        if beside.is_empty() {
            return position[vertex] as f32;
        }
        let sum: usize = beside.iter().map(|&n| position[n]).sum();
        sum as f32 / beside.len() as f32
    };
    let mut keyed: Vec<(f32, usize)> = layer.iter().map(|&v| (barycentre(v), v)).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (slot, (_, vertex)) in layer.iter_mut().zip(keyed) {
        *slot = vertex;
    }
    set_positions(layer, position);
}

fn set_positions(layer: &[usize], position: &mut [usize]) {
    for (index, &vertex) in layer.iter().enumerate() {
        position[vertex] = index;
    }
}

/// How many pairs in `values` are out of order, counted with a Fenwick tree of the values
/// seen so far.
fn inversions(values: impl Iterator<Item = usize>) -> usize {
    let values: Vec<usize> = values.collect();
    let size = values.iter().max().map_or(0, |max| max + 1);
    let mut tree = vec![0usize; size + 1];
    let mut count = 0;
    for (seen, &value) in values.iter().enumerate() {
        let mut not_above = 0;
        let mut index = value + 1;
        while index > 0 {
            not_above += tree[index];
            index &= index - 1;
        }
        count += seen - not_above;
        let mut index = value + 1;
        while index <= size {
            tree[index] += 1;
            index += index & index.wrapping_neg();
        }
    }
    count
}

/// Where the columns ended: their right edge, and the tallest one's height.
#[derive(Debug, Clone, Copy, Default)]
struct Placed {
    right: Option<f32>,
    tallest: f32,
}

/// Places each layer's tables as a column, left to right, each column centred on the tallest.
/// Dummy vertices take no room.
fn place_columns(columns: &[Vec<usize>], sizes: &[Size], positions: &mut [Point]) -> Placed {
    let tables = |column: &[usize]| -> Vec<usize> {
        column
            .iter()
            .copied()
            .filter(|&v| v < sizes.len())
            .collect()
    };
    let columns: Vec<Vec<usize>> = columns.iter().map(|column| tables(column)).collect();
    let height = |column: &[usize]| -> f32 {
        let boxes: f32 = column.iter().map(|&table| sizes[table].height).sum();
        boxes + ROW_GAP * column.len().saturating_sub(1) as f32
    };
    let tallest = columns.iter().map(|c| height(c)).fold(0.0, f32::max);
    let mut x = 0.0;
    let mut right = None;
    for column in &columns {
        let mut y = (tallest - height(column)) / 2.0;
        let mut width: f32 = 0.0;
        for &table in column {
            positions[table] = Point { x, y };
            y += sizes[table].height + ROW_GAP;
            width = width.max(sizes[table].width);
        }
        right = Some(x + width);
        x += width + COLUMN_GAP;
    }
    Placed { right, tallest }
}

/// Places the tables no key joins in columns after the others, each column filled down to the
/// tallest one's height, or to a square's side when the others are short or there are none.
fn place_unjoined(tables: &[usize], sizes: &[Size], placed: Placed, positions: &mut [Point]) {
    let area: f32 = tables
        .iter()
        .map(|&t| (sizes[t].width + COLUMN_GAP) * (sizes[t].height + ROW_GAP))
        .sum();
    let highest = tables.iter().map(|&t| sizes[t].height).fold(0.0, f32::max);
    let limit = placed.tallest.max(area.sqrt()).max(highest);
    let mut x = placed.right.map_or(0.0, |right| right + COLUMN_GAP);
    let mut y = 0.0;
    let mut width: f32 = 0.0;
    for &table in tables {
        let size = sizes[table];
        if y > 0.0 && y + size.height > limit {
            x += width + COLUMN_GAP;
            y = 0.0;
            width = 0.0;
        }
        positions[table] = Point { x, y };
        y += size.height + ROW_GAP;
        width = width.max(size.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: Size = Size {
        width: 160.0,
        height: 100.0,
    };

    /// A graph of `names.len()` same-sized tables, joined by `keys` given as (referencing,
    /// referenced) names, and each table's position by name.
    fn lay_out<'a>(names: &[&'a str], keys: &[(&str, &str)]) -> Vec<(&'a str, Point)> {
        let mut graph = Graph::default();
        let ids: Vec<NodeId> = names.iter().map(|_| graph.add(TABLE)).collect();
        let id = |name: &str| ids[names.iter().position(|n| *n == name).unwrap()];
        for (from, to) in keys {
            graph.join(id(from), id(to));
        }
        names.iter().copied().zip(graph.layout()).collect()
    }

    fn at(placed: &[(&str, Point)], name: &str) -> Point {
        placed.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn referenced_tables_come_first_and_the_tables_pointing_at_them_after() {
        let placed = lay_out(
            &["customers", "order_items", "orders", "products"],
            &[
                ("orders", "customers"),
                ("order_items", "orders"),
                ("order_items", "products"),
            ],
        );
        let x = |name| at(&placed, name).x;

        assert_eq!(x("customers"), 0.0);
        assert_eq!(x("products"), 0.0);
        assert!(x("orders") > x("customers"));
        assert!(x("order_items") > x("orders"), "the longest path decides");
    }

    #[test]
    fn cycles_and_self_references_still_lay_out() {
        type Tables<'a> = &'a [&'a str];
        type Keys<'a> = &'a [(&'a str, &'a str)];
        let cases: [(Tables, Keys); 4] = [
            (&["employees"], &[("employees", "employees")]),
            (&["a", "b"], &[("a", "b"), ("b", "a")]),
            (&["a", "b", "c"], &[("a", "b"), ("b", "c"), ("c", "a")]),
            (
                &["a", "b", "c"],
                &[("a", "a"), ("a", "b"), ("b", "c"), ("c", "b"), ("b", "b")],
            ),
        ];
        for (names, keys) in cases {
            let placed = lay_out(names, keys);

            assert_no_overlap(&placed);
            if names.len() > 1 {
                let columns: HashSet<u32> = placed.iter().map(|(_, p)| p.x.to_bits()).collect();
                assert!(columns.len() > 1, "{names:?} share a column");
            }
        }
    }

    #[test]
    fn a_cycle_is_broken_at_the_key_found_last() {
        let placed = lay_out(&["a", "b"], &[("a", "b"), ("b", "a")]);

        assert!(at(&placed, "b").x < at(&placed, "a").x, "a → b is kept");
    }

    #[test]
    fn several_keys_between_two_tables_lay_out_as_one() {
        let once = lay_out(&["a", "b"], &[("a", "b")]);
        let twice = lay_out(&["a", "b"], &[("a", "b"), ("a", "b")]);

        assert_eq!(once, twice);
    }

    #[test]
    fn tables_no_key_joins_go_after_the_others() {
        let placed = lay_out(
            &["lonely", "orders", "customers"],
            &[("orders", "customers")],
        );

        assert!(at(&placed, "lonely").x > at(&placed, "orders").x);
    }

    #[test]
    fn a_schema_without_keys_is_a_grid_not_a_column() {
        let names: Vec<String> = (0..16).map(|n| format!("t{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let placed = lay_out(&names, &[]);
        let columns: HashSet<u32> = placed.iter().map(|(_, p)| p.x.to_bits()).collect();

        assert_no_overlap(&placed);
        assert!(
            (3..=6).contains(&columns.len()),
            "{} columns",
            columns.len()
        );
    }

    #[test]
    fn sweeping_removes_a_crossing_index_order_makes() {
        // In the order added, c and d would sit level with a and b, and their keys would
        // cross: c → b, d → a.
        let placed = lay_out(&["a", "b", "c", "d"], &[("c", "b"), ("d", "a")]);
        let y = |name| at(&placed, name).y;

        assert!(y("a") < y("b"));
        assert!(y("d") < y("c"));
    }

    #[test]
    fn a_long_key_passes_through_the_layers_between() {
        // a → c skips b's layer; its dummy there keeps c's and d's keys from crossing it.
        let placed = lay_out(
            &["a", "b", "c", "d", "e"],
            &[("a", "b"), ("b", "c"), ("a", "c"), ("d", "e"), ("b", "e")],
        );

        assert_no_overlap(&placed);
        assert!(at(&placed, "a").x > at(&placed, "b").x);
        assert!(at(&placed, "b").x > at(&placed, "c").x);
    }

    #[test]
    fn counts_crossings_as_inversions() {
        let cases: [(&[usize], usize); 4] = [
            (&[], 0),
            (&[0, 1, 2], 0),
            (&[2, 1, 0], 3),
            (&[1, 1, 0, 2], 2),
        ];
        for (values, expected) in cases {
            assert_eq!(inversions(values.iter().copied()), expected, "{values:?}");
        }
    }

    #[test]
    fn a_large_schema_lays_out_without_overlaps() {
        let mut graph = Graph::default();
        let mut seed: u64 = 7;
        let mut next = |bound: usize| {
            // A linear congruential generator, so the schema is the same on every run.
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) as usize % bound
        };
        let sizes: Vec<Size> = (0..300)
            .map(|_| Size {
                width: 120.0 + next(120) as f32,
                height: 40.0 + next(400) as f32,
            })
            .collect();
        let ids: Vec<NodeId> = sizes.iter().map(|&size| graph.add(size)).collect();
        for _ in 0..450 {
            graph.join(ids[next(ids.len())], ids[next(ids.len())]);
        }

        let positions = graph.layout();

        let boxes: Vec<(Point, Size)> = positions.into_iter().zip(sizes).collect();
        assert_boxes_apart(&boxes);
    }

    fn assert_no_overlap(placed: &[(&str, Point)]) {
        let boxes: Vec<(Point, Size)> = placed.iter().map(|(_, p)| (*p, TABLE)).collect();
        assert_boxes_apart(&boxes);
    }

    fn assert_boxes_apart(boxes: &[(Point, Size)]) {
        for (i, (a, a_size)) in boxes.iter().enumerate() {
            assert!(a.x.is_finite() && a.y.is_finite(), "box {i} at {a:?}");
            for (b, b_size) in &boxes[i + 1..] {
                let apart = a.x + a_size.width <= b.x
                    || b.x + b_size.width <= a.x
                    || a.y + a_size.height <= b.y
                    || b.y + b_size.height <= a.y;
                assert!(apart, "{a:?} {a_size:?} overlaps {b:?} {b_size:?}");
            }
        }
    }
}
