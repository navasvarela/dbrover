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

/// Opens **Diagram…** on `main` with every table, once the explorer shows `main`.
fn diagram_of_main(harness: &mut Harness<'_, DbRoverApp>) {
    step_until_visible(harness, "Expand shop");
    harness.get_by_label("Expand shop").click();
    step_until_visible(harness, "Expand main");
    node_menu(harness, "main", "Diagram…");
    step_until_visible(harness, "order_items");
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::Button, "Open").click();
}

fn top_left(diagram: &Diagram, table: &str) -> egui::Pos2 {
    let index = diagram
        .tables()
        .iter()
        .position(|t| t.name.name == table)
        .unwrap_or_else(|| panic!("{table} is in the diagram"));
    diagram.rect(index).unwrap().min
}

#[test]
fn keeps_a_moved_table_where_it_was_left_and_a_dropped_one_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let file = dir.path().join("diagrams/_none/main.toml");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    diagram_of_main(&mut harness);
    let (key, laid_out) = loaded_diagram(&mut harness);

    let products = harness
        .state()
        .diagram_table_on_screen(&key, "products")
        .unwrap()
        .center();
    harness.hover_at(products);
    harness.run_steps(2);
    harness.drag_at(products);
    harness.run_steps(2);
    for step in 1..=4 {
        harness.hover_at(products + egui::vec2(15.0, 10.0) * step as f32);
        harness.run_steps(1);
    }
    harness.drop_at(products + egui::vec2(60.0, 40.0));
    harness.run_steps(2);
    let (_, moved) = loaded_diagram(&mut harness);
    assert_ne!(
        top_left(&moved, "products"),
        top_left(&laid_out, "products")
    );
    assert_eq!(top_left(&moved, "orders"), top_left(&laid_out, "orders"));

    step_until(
        &mut harness,
        "the diagram's file being written",
        |harness| {
            let saved = std::fs::read_to_string(&file).unwrap_or_default();
            let (_, diagram) = harness.state().open_diagrams().next().unwrap();
            let expected = diagram.to_file();
            toml::from_str::<dbrover_store::DiagramFile>(&saved).ok() == Some(expected)
        },
    );
    drop(harness);
    rusqlite::Connection::open(dir.path().join("shop.db"))
        .unwrap()
        .execute_batch("DROP TABLE notes")
        .unwrap();

    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Expand shop");
    harness.get_by_label("shop").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Diagrams").hover();
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::Button, "main").click();
    let (key, reopened) = loaded_diagram(&mut harness);

    assert_eq!(key.name.as_str(), "main");
    let rounded = |point: egui::Pos2| point.round();
    assert_eq!(
        top_left(&reopened, "products"),
        rounded(top_left(&moved, "products"))
    );
    assert_eq!(table_names(&reopened), table_names(&moved));
    let notes = reopened
        .tables()
        .iter()
        .find(|table| table.name.name == "notes")
        .unwrap();
    assert_eq!(notes.state, TableState::Missing, "kept, shown as missing");
    assert_eq!(
        reopened.to_file(),
        moved.to_file(),
        "nothing dropped from the file"
    );
}
