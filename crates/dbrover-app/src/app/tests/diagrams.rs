//! ER diagrams (ER1), driven through the whole app against SQLite: opened from a schema's
//! **Diagram…** and a table's **Diagram with related tables**, described a batch at a time on
//! the explorer's connection, and laid out with referenced tables first.

use dbrover_core::TableName;

use super::*;
use crate::diagram::{Diagram, TableState};

/// Orders point at customers and products, order items at orders and products; notes are
/// joined to nothing.
const SHOP: &str = "
    CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price NUMERIC);
    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        customer_id INTEGER NOT NULL REFERENCES customers (id),
        product_id INTEGER REFERENCES products (id)
    );
    CREATE TABLE order_items (
        order_id INTEGER NOT NULL REFERENCES orders (id),
        product_id INTEGER NOT NULL REFERENCES products (id),
        PRIMARY KEY (order_id, product_id)
    );
    CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
";

/// The only open diagram, once it has finished loading.
fn loaded_diagram(harness: &mut Harness<'_, DbRoverApp>) -> (DiagramKey, Diagram) {
    step_until(harness, "the diagram loading", |harness| {
        harness
            .state()
            .open_diagrams()
            .any(|(_, diagram)| !diagram.is_loading())
    });
    // One more frame measures and places the boxes.
    harness.run_steps(2);
    let mut diagrams = harness.state().open_diagrams();
    let (key, diagram) = diagrams.next().expect("a diagram is open");
    assert!(diagrams.next().is_none(), "one diagram");
    (key.clone(), diagram.clone())
}

fn table_names(diagram: &Diagram) -> Vec<&str> {
    let mut names: Vec<&str> = diagram
        .tables()
        .iter()
        .map(|table| table.name.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

fn left_of(diagram: &Diagram, table: &str) -> f32 {
    let index = diagram
        .tables()
        .iter()
        .position(|t| t.name.name == table)
        .unwrap_or_else(|| panic!("{table} is in the diagram"));
    diagram
        .rect(index)
        .unwrap_or_else(|| panic!("{table} is placed"))
        .left()
}

#[test]
fn draws_the_tables_picked_in_a_schema_and_the_tables_they_reference() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Expand shop");
    harness.get_by_label("Expand shop").click();
    step_until_visible(&mut harness, "Expand main");

    node_menu(&mut harness, "main", "Diagram…");
    step_until_visible(&mut harness, "order_items");
    harness.run_steps(2);
    harness.get_by_label("Diagram of main");
    for (role, label) in [
        (Role::Button, "None"),
        (Role::CheckBox, "orders"),
        (Role::CheckBox, "Add the tables these reference"),
        (Role::Button, "Open"),
    ] {
        harness.get_by_role_and_label(role, label).click();
        harness.run_steps(2);
    }

    let (key, diagram) = loaded_diagram(&mut harness);
    assert_eq!(key.name.as_str(), "main");
    assert_eq!(key.folder, DiagramFolder::NoProject);
    assert_eq!(table_names(&diagram), ["customers", "orders", "products"]);
    assert!(
        diagram
            .tables()
            .iter()
            .all(|table| matches!(table.state, TableState::Described(_)))
    );
    assert!(left_of(&diagram, "customers") < left_of(&diagram, "orders"));
    assert!(left_of(&diagram, "products") < left_of(&diagram, "orders"));
    assert_eq!(diagram.key_lines().len(), 2);
    harness.get_by_label("3 tables");

    harness.get_by_label("Arrange").click();
    harness.run_steps(2);
    let (_, arranged) = loaded_diagram(&mut harness);
    assert_eq!(arranged.bounds(), diagram.bounds(), "the same layout again");
}

#[test]
fn draws_a_table_with_every_table_its_keys_join() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    expand_shop_tables(&mut harness, "orders");

    node_menu(&mut harness, "orders", "Diagram with related tables");

    let (key, diagram) = loaded_diagram(&mut harness);
    assert_eq!(key.name.as_str(), "orders");
    assert_eq!(
        table_names(&diagram),
        ["customers", "order_items", "orders", "products"],
        "notes is joined to nothing"
    );
    assert!(left_of(&diagram, "orders") < left_of(&diagram, "order_items"));
    let label = diagram.label(&TableName {
        schema: "main".to_owned(),
        name: "orders".to_owned(),
    });
    assert_eq!(label, "orders", "named without the schema it was opened on");
}
