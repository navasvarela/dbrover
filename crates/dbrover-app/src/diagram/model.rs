//! A diagram's tables: which ones it shows, how far each one is described, and where each box
//! sits. It knows nothing of drawing: the view measures each box from its text, then asks the
//! diagram to place the boxes it hasn't placed yet.

use std::collections::{HashSet, VecDeque};

use dbrover_core::{DataSourceId, Identifier, RelationName, TableDescription, TableName};
use dbrover_diagram::{Graph, NodeId, Point, Size};
use dbrover_engine::{Command, DescribedTable};
use egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::geometry;

/// How many tables one `DescribeTables` asks for. The explorer's connection answers in order,
/// so a smaller batch lets the explorer's own requests through sooner, and a bigger one costs
/// fewer events; eight tables take a few milliseconds on a local server.
pub const BATCH: usize = 8;

/// Which tables a diagram grows by once the tables it started from are described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Growth {
    /// The tables their foreign keys point at: **Add the tables these reference**.
    Referenced,
    /// Those, and the tables whose foreign keys point at them: **Diagram with related
    /// tables**.
    Related,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableState {
    Loading,
    Described(TableDescription),
    /// Described as no table: dropped since the diagram was made, or a view.
    Missing,
    Failed(String),
}

/// A table's box as the view measured it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShape {
    pub size: Vec2,
    /// Where each column's type starts, from the box's left edge.
    pub type_x: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagramTable {
    pub name: TableName,
    pub state: TableState,
    /// The box's top-left corner, once placed.
    pub position: Option<Pos2>,
    /// `None` until the view measures it, and again whenever what it shows changes.
    pub shape: Option<BoxShape>,
}

/// A key drawn between two tables of the diagram: from the row of the referencing table's
/// first key column to the row of the column it refers to, or to the header when the
/// diagram doesn't show that column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLine {
    pub from: usize,
    pub from_row: Option<usize>,
    pub to: usize,
    pub to_row: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Diagram {
    pub data_source: DataSourceId,
    /// The schema the diagram was opened on, whose tables it names without it.
    home_schema: Option<String>,
    tables: Vec<DiagramTable>,
    to_describe: VecDeque<TableName>,
    /// The batch asked for and not answered yet.
    asked: Vec<RelationName>,
    /// The tables to grow from, and how, once every table asked for is described.
    growth: Option<(Growth, Vec<TableName>)>,
    /// Whether the layout places every box. Off once the user moves one, after which only new
    /// tables are placed, beside the rest.
    arranged: bool,
    /// Tables were added or changed since the boxes were last placed.
    unplaced: bool,
}

impl Diagram {
    pub fn new(
        data_source: DataSourceId,
        home_schema: Option<String>,
        tables: Vec<TableName>,
        growth: Option<Growth>,
    ) -> Self {
        let mut diagram = Self {
            data_source,
            home_schema,
            tables: Vec::new(),
            to_describe: VecDeque::new(),
            asked: Vec::new(),
            growth: growth.map(|growth| (growth, tables.clone())),
            arranged: true,
            unplaced: true,
        };
        diagram.add(tables);
        diagram
    }

    pub fn tables(&self) -> &[DiagramTable] {
        &self.tables
    }

    /// The table's name as the diagram shows it: without the schema the diagram was opened
    /// on.
    pub fn label(&self, table: &TableName) -> String {
        if self.home_schema.as_deref() == Some(table.schema.as_str()) {
            table.name.clone()
        } else {
            format!("{}.{}", table.schema, table.name)
        }
    }

    /// How many tables are described (or found missing, or failed), of how many.
    pub fn progress(&self) -> (usize, usize) {
        let loading = self
            .tables
            .iter()
            .filter(|table| table.state == TableState::Loading)
            .count();
        (self.tables.len() - loading, self.tables.len())
    }

    pub fn is_loading(&self) -> bool {
        !self.asked.is_empty() || !self.to_describe.is_empty()
    }

    /// The next tables to describe, unless a batch is still being answered or none are left.
    pub fn next_batch(&mut self) -> Option<Command> {
        if !self.asked.is_empty() || self.to_describe.is_empty() {
            return None;
        }
        let count = self.to_describe.len().min(BATCH);
        self.asked = self
            .to_describe
            .drain(..count)
            .map(|t| relation(&t))
            .collect();
        Some(Command::DescribeTables {
            data_source: self.data_source,
            tables: self.asked.clone(),
        })
    }

    /// Takes the descriptions of the tables it is waiting for. Once its batch is answered, a
    /// diagram that was to grow adds the tables it grows by.
    pub fn described(&mut self, described: &[DescribedTable]) {
        for answer in described {
            let Some(asked) = self.asked.iter().position(|t| *t == answer.table) else {
                continue;
            };
            self.asked.swap_remove(asked);
            let state = match &answer.result {
                Ok(Some(description)) => TableState::Described(description.clone()),
                Ok(None) => TableState::Missing,
                Err(error) => TableState::Failed(error.clone()),
            };
            if let Some(table) = self.table_named_mut(&answer.table) {
                table.state = state;
                table.shape = None;
                self.unplaced = true;
            }
        }
        if !self.is_loading()
            && let Some((growth, from)) = self.growth.take()
        {
            let related = self.related(&from, growth);
            self.add(related);
        }
    }

    /// Marks every table still waiting as failed, with `reason`: the connection went away
    /// before describing them.
    pub fn stop_loading(&mut self, reason: &str) {
        self.asked.clear();
        self.to_describe.clear();
        self.growth = None;
        for table in &mut self.tables {
            if table.state == TableState::Loading {
                table.state = TableState::Failed(reason.to_owned());
                table.shape = None;
                self.unplaced = true;
            }
        }
    }

    /// Lays every box out again, forgetting where the user moved them: **Arrange**.
    pub fn arrange(&mut self) {
        self.arranged = true;
        self.unplaced = true;
    }

    /// Moves the table's box by `delta`, which leaves the rest where they are from then on.
    pub fn move_table(&mut self, index: usize, delta: Vec2) {
        if let Some(position) = self.tables.get_mut(index).and_then(|t| t.position.as_mut()) {
            *position += delta;
            self.arranged = false;
        }
    }

    pub fn set_shape(&mut self, index: usize, shape: BoxShape) {
        if let Some(table) = self.tables.get_mut(index) {
            table.shape = Some(shape);
        }
    }

    /// Places the boxes once every one is measured: all of them while the layout decides,
    /// otherwise only the new ones, beside the rest. Returns whether the whole diagram was
    /// laid out, which is when the view shows it all again.
    pub fn place(&mut self) -> bool {
        let measured = self.tables.iter().all(|table| table.shape.is_some());
        if !self.unplaced || !measured {
            return false;
        }
        self.unplaced = false;
        if self.arranged {
            let all: Vec<usize> = (0..self.tables.len()).collect();
            for (index, position) in self.lay_out(&all, Pos2::ZERO) {
                self.tables[index].position = Some(position);
            }
            return true;
        }
        let unplaced: Vec<usize> = (0..self.tables.len())
            .filter(|&index| self.tables[index].position.is_none())
            .collect();
        let origin = self.bounds().map_or(Pos2::ZERO, |bounds| {
            pos2(bounds.right() + geometry::NEW_TABLES_GAP, bounds.top())
        });
        for (index, position) in self.lay_out(&unplaced, origin) {
            self.tables[index].position = Some(position);
        }
        false
    }

    /// The box of the table at `index`, once measured and placed.
    pub fn rect(&self, index: usize) -> Option<Rect> {
        let table = self.tables.get(index)?;
        Some(Rect::from_min_size(table.position?, table.shape?.size))
    }

    /// Around every placed box.
    pub fn bounds(&self) -> Option<Rect> {
        (0..self.tables.len())
            .filter_map(|index| self.rect(index))
            .reduce(|bounds, rect| bounds.union(rect))
    }

    /// Every key between two tables of the diagram, each once.
    pub fn key_lines(&self) -> Vec<KeyLine> {
        let mut lines = Vec::new();
        for (from, table) in self.tables.iter().enumerate() {
            let TableState::Described(description) = &table.state else {
                continue;
            };
            for key in &description.foreign_keys {
                let Some(to) = self.index_of(&key.referenced) else {
                    continue;
                };
                lines.push(KeyLine {
                    from,
                    from_row: key.columns.first().and_then(|c| column_row(description, c)),
                    to,
                    to_row: key
                        .referenced_columns
                        .first()
                        .and_then(|column| self.row_of(to, column)),
                });
            }
        }
        lines
    }

    fn add(&mut self, tables: Vec<TableName>) {
        let mut known: HashSet<TableName> = self.tables.iter().map(|t| t.name.clone()).collect();
        for name in tables {
            if !known.insert(name.clone()) {
                continue;
            }
            self.to_describe.push_back(name.clone());
            self.tables.push(DiagramTable {
                name,
                state: TableState::Loading,
                position: None,
                shape: None,
            });
            self.unplaced = true;
        }
    }

    /// The tables `from`'s keys join, in the order their keys list them.
    fn related(&self, from: &[TableName], growth: Growth) -> Vec<TableName> {
        let mut related = Vec::new();
        for name in from {
            let Some(TableState::Described(description)) =
                self.index_of(name).map(|index| &self.tables[index].state)
            else {
                continue;
            };
            related.extend(
                description
                    .foreign_keys
                    .iter()
                    .map(|k| k.referenced.clone()),
            );
            if growth == Growth::Related {
                related.extend(description.referenced_by.iter().map(|k| k.table.clone()));
            }
        }
        related
    }

    /// Lays the tables at `indices` out among themselves, by their keys, from `origin`.
    fn lay_out(&self, indices: &[usize], origin: Pos2) -> Vec<(usize, Pos2)> {
        // In name order, so a diagram of the same tables lays out the same way every time.
        let mut indices = indices.to_vec();
        indices.sort_by_key(|&index| &self.tables[index].name.name);
        let mut graph = Graph::default();
        let nodes: Vec<(usize, NodeId)> = indices
            .iter()
            .map(|&index| {
                let size = self.tables[index]
                    .shape
                    .map_or(Vec2::ZERO, |shape| shape.size);
                (
                    index,
                    graph.add(Size {
                        width: size.x,
                        height: size.y,
                    }),
                )
            })
            .collect();
        let node_of = |index: usize| nodes.iter().find(|(i, _)| *i == index).map(|(_, n)| *n);
        for line in self.key_lines() {
            if let (Some(from), Some(to)) = (node_of(line.from), node_of(line.to)) {
                graph.join(from, to);
            }
        }
        let positions = graph.layout();
        nodes
            .iter()
            .zip(positions)
            .map(|(&(index, _), Point { x, y })| (index, origin + vec2(x, y)))
            .collect()
    }

    fn index_of(&self, name: &TableName) -> Option<usize> {
        self.tables.iter().position(|table| table.name == *name)
    }

    fn table_named_mut(&mut self, relation_name: &RelationName) -> Option<&mut DiagramTable> {
        self.tables
            .iter_mut()
            .find(|table| relation(&table.name) == *relation_name)
    }

    /// The row of `column` in the table at `index`, once described.
    fn row_of(&self, index: usize, column: &str) -> Option<usize> {
        match &self.tables[index].state {
            TableState::Described(description) => column_row(description, column),
            _ => None,
        }
    }
}

fn column_row(description: &TableDescription, column: &str) -> Option<usize> {
    description.columns.iter().position(|c| c.name == column)
}

/// The table as `DescribeTables` names it: both names quoted, so they are taken as spelled.
pub fn relation(table: &TableName) -> RelationName {
    let quoted = |name: &str| Identifier {
        name: name.to_owned(),
        quoted: true,
    };
    RelationName {
        schema: Some(quoted(&table.schema)),
        name: quoted(&table.name),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{ColumnDescription, ForeignKey};

    use super::*;

    fn name(table: &str) -> TableName {
        TableName {
            schema: "public".to_owned(),
            name: table.to_owned(),
        }
    }

    fn column(name: &str) -> ColumnDescription {
        ColumnDescription {
            name: name.to_owned(),
            data_type: "integer".to_owned(),
            nullable: false,
            default: None,
            identity: None,
        }
    }

    fn key(table: &str, column: &str, referenced: &str) -> ForeignKey {
        ForeignKey {
            table: name(table),
            columns: vec![column.to_owned()],
            referenced: name(referenced),
            referenced_columns: vec!["id".to_owned()],
        }
    }

    /// The shop: orders point at customers and products; reviews point at products.
    fn shop(table: &str) -> TableDescription {
        let keys = |own: &[(&str, &str)]| -> Vec<ForeignKey> {
            own.iter().map(|(c, to)| key(table, c, to)).collect()
        };
        let (columns, foreign_keys, referenced_by) = match table {
            "orders" => (
                vec!["id", "customer_id", "product_id"],
                keys(&[("customer_id", "customers"), ("product_id", "products")]),
                vec![],
            ),
            "customers" => (
                vec!["id", "name"],
                vec![],
                vec![key("orders", "customer_id", "customers")],
            ),
            "products" => (
                vec!["id", "name"],
                vec![],
                vec![
                    key("orders", "product_id", "products"),
                    key("reviews", "product_id", "products"),
                ],
            ),
            "reviews" => (
                vec!["id", "product_id"],
                keys(&[("product_id", "products")]),
                vec![],
            ),
            other => panic!("no table {other}"),
        };
        TableDescription {
            schema: "public".to_owned(),
            name: table.to_owned(),
            columns: columns.into_iter().map(column).collect(),
            primary_key: vec!["id".to_owned()],
            foreign_keys,
            referenced_by,
        }
    }

    /// Answers every batch the diagram asks for from the shop, as the engine would, and
    /// returns how many batches it took.
    fn answer_all(diagram: &mut Diagram) -> usize {
        let mut batches = 0;
        while let Some(Command::DescribeTables { tables, .. }) = diagram.next_batch() {
            batches += 1;
            let answers: Vec<DescribedTable> = tables
                .into_iter()
                .map(|table| {
                    let result = match table.name.name.as_str() {
                        "gone" => Ok(None),
                        "broken" => Err("permission denied".to_owned()),
                        found => Ok(Some(shop(found))),
                    };
                    DescribedTable { table, result }
                })
                .collect();
            diagram.described(&answers);
        }
        batches
    }

    fn names(diagram: &Diagram) -> Vec<&str> {
        let mut names: Vec<&str> = diagram
            .tables()
            .iter()
            .map(|t| t.name.name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn grows_by_the_tables_it_references_or_by_every_related_one() {
        let cases = [
            (None, vec!["orders"]),
            (
                Some(Growth::Referenced),
                vec!["customers", "orders", "products"],
            ),
            (
                Some(Growth::Related),
                vec!["customers", "orders", "products"],
            ),
        ];
        for (growth, expected) in cases {
            let mut diagram =
                Diagram::new(DataSourceId::generate(), None, vec![name("orders")], growth);
            answer_all(&mut diagram);

            assert_eq!(names(&diagram), expected, "{growth:?}");
            assert!(!diagram.is_loading());
        }

        let mut from_products = Diagram::new(
            DataSourceId::generate(),
            None,
            vec![name("products")],
            Some(Growth::Related),
        );
        answer_all(&mut from_products);
        assert_eq!(names(&from_products), ["orders", "products", "reviews"]);
    }

    #[test]
    fn asks_a_batch_at_a_time_and_only_once_the_last_is_answered() {
        let many: Vec<TableName> = (0..20).map(|n| name(&format!("t{n}"))).collect();
        let mut diagram = Diagram::new(DataSourceId::generate(), None, many, None);

        let Some(Command::DescribeTables { tables, .. }) = diagram.next_batch() else {
            panic!("asks for the first batch");
        };
        assert_eq!(tables.len(), BATCH);
        assert!(diagram.next_batch().is_none(), "waits for the answer");
        assert_eq!(diagram.progress(), (0, 20));
    }

    #[test]
    fn a_missing_or_failed_table_stays_in_the_diagram() {
        let tables = vec![name("orders"), name("gone"), name("broken")];
        let mut diagram = Diagram::new(DataSourceId::generate(), None, tables, None);
        answer_all(&mut diagram);

        let states: Vec<&TableState> = diagram.tables().iter().map(|t| &t.state).collect();
        assert!(matches!(states[0], TableState::Described(_)));
        assert_eq!(states[1], &TableState::Missing);
        assert_eq!(
            states[2],
            &TableState::Failed("permission denied".to_owned())
        );
        assert_eq!(diagram.progress(), (3, 3));
    }

    #[test]
    fn a_lost_connection_fails_what_was_still_loading() {
        let tables = vec![name("orders"), name("customers")];
        let mut diagram = Diagram::new(
            DataSourceId::generate(),
            None,
            tables,
            Some(Growth::Related),
        );
        diagram.next_batch();

        diagram.stop_loading("disconnected");

        assert!(!diagram.is_loading());
        assert!(diagram.next_batch().is_none());
        assert!(
            diagram
                .tables()
                .iter()
                .all(|t| t.state == TableState::Failed("disconnected".to_owned()))
        );
    }

    #[test]
    fn draws_a_line_per_key_between_the_tables_it_shows() {
        let tables = vec![name("orders"), name("customers")];
        let mut diagram = Diagram::new(DataSourceId::generate(), None, tables, None);
        answer_all(&mut diagram);

        assert_eq!(
            diagram.key_lines(),
            [KeyLine {
                from: 0,
                from_row: Some(1),
                to: 1,
                to_row: Some(0),
            }],
            "orders.customer_id → customers.id; products isn't shown"
        );
    }

    fn measure_all(diagram: &mut Diagram) {
        for index in 0..diagram.tables().len() {
            diagram.set_shape(
                index,
                BoxShape {
                    size: vec2(120.0, 80.0),
                    type_x: 60.0,
                },
            );
        }
    }

    #[test]
    fn places_boxes_once_measured_and_keeps_moved_ones_where_they_were_left() {
        let mut diagram = Diagram::new(
            DataSourceId::generate(),
            None,
            vec![name("orders")],
            Some(Growth::Referenced),
        );
        answer_all(&mut diagram);
        assert!(!diagram.place(), "nothing is measured yet");
        measure_all(&mut diagram);
        assert!(diagram.place(), "the whole diagram is laid out");
        let customers = diagram.index_of(&name("customers")).unwrap();
        let orders = diagram.index_of(&name("orders")).unwrap();
        assert!(diagram.rect(customers).unwrap().left() < diagram.rect(orders).unwrap().left());

        diagram.move_table(customers, vec2(500.0, 40.0));
        let moved = diagram.rect(customers).unwrap().min;
        diagram.add(vec![name("reviews")]);
        answer_all(&mut diagram);
        measure_all(&mut diagram);
        assert!(!diagram.place(), "only the new table is placed");

        assert_eq!(diagram.rect(customers).unwrap().min, moved);
        let reviews = diagram.index_of(&name("reviews")).unwrap();
        let others = diagram
            .rect(customers)
            .unwrap()
            .union(diagram.rect(orders).unwrap());
        assert!(
            diagram.rect(reviews).unwrap().left() > others.right(),
            "beside the rest"
        );

        diagram.arrange();
        assert!(diagram.place());
        assert_ne!(diagram.rect(customers).unwrap().min, moved);
    }

    #[test]
    fn names_tables_outside_the_home_schema_with_their_schema() {
        let diagram = Diagram::new(
            DataSourceId::generate(),
            Some("public".to_owned()),
            vec![],
            None,
        );
        let audit = TableName {
            schema: "audit".to_owned(),
            name: "log".to_owned(),
        };

        assert_eq!(diagram.label(&name("orders")), "orders");
        assert_eq!(diagram.label(&audit), "audit.log");
    }
}
