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

#[test]
fn exports_the_diagram_as_svg_and_as_png() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let pictures = dir.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let answer = pictures.clone();
    harness.state_mut().pick_picture =
        Box::new(move |name, format| Some(answer.join(format!("{name}.{}", format.extension()))));
    diagram_of_main(&mut harness);
    loaded_diagram(&mut harness);

    for (item, file) in [("SVG…", "main.svg"), ("PNG at 2×…", "main.png")] {
        harness.get_by_label("Export").click();
        harness.run_steps(2);
        harness.get_by_label(item).click();
        step_until_visible(
            &mut harness,
            &format!("Exported to {}", pictures.join(file).display()),
        );
    }

    let svg = std::fs::read_to_string(pictures.join("main.svg")).unwrap();
    for table in ["customers", "orders", "order_items", "products", "notes"] {
        assert!(
            svg.contains(&format!(">{table}</text>")),
            "{table} in the SVG"
        );
    }
    let curves = svg
        .lines()
        .filter(|line| line.starts_with("<path d=\"M") && line.contains(" C"));
    assert_eq!(curves.count(), 4, "a curve per key");
    let png = std::fs::read(pictures.join("main.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let svg_width: f32 = svg
        .split("width=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|width| width.parse().ok())
        .unwrap();
    let png_width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(png_width, (svg_width * 2.0) as u32, "drawn at 2×");
}

/// A database of the test's own on the PostgreSQL test server, holding the exit's tables in
/// `public` and one more, and dropped when the test ends.
struct ExitDatabase<'a> {
    runtime: &'a tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    name: String,
}

impl<'a> ExitDatabase<'a> {
    fn create(server: &PostgresTestServer, runtime: &'a tokio::runtime::Runtime) -> Self {
        let name = format!("dbrover_er1_{}", std::process::id());
        let mut admin = server.open(runtime, &server.database);
        first_value(
            runtime,
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        );
        first_value(runtime, &mut admin, &format!("CREATE DATABASE {name}"));
        let mut shop = server.open(runtime, &name);
        for statement in [
            "CREATE TABLE customers (id int PRIMARY KEY, name text NOT NULL)",
            "CREATE TABLE products (id int PRIMARY KEY, name text NOT NULL, price numeric(10, 2))",
            "CREATE TABLE orders (id int PRIMARY KEY, \
             customer_id int NOT NULL REFERENCES customers (id), \
             product_id int REFERENCES products (id), placed_at timestamptz)",
            "CREATE TABLE audit_log (id bigint PRIMARY KEY, happened text)",
        ] {
            first_value(runtime, &mut shop, statement);
        }
        let _ = runtime.block_on(shop.close());
        Self {
            runtime,
            admin,
            name,
        }
    }
}

impl Drop for ExitDatabase<'_> {
    fn drop(&mut self) {
        let drop = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
        first_value(self.runtime, &mut self.admin, &drop);
    }
}

/// ER1's exit: on the PostgreSQL test server, right-click `public` → **Diagram…**, pick
/// `orders` and tick **Add the tables these reference**: `orders`, `customers` and `products`
/// appear, laid out with their keys drawn. Move `products`, quit, start again and reopen the
/// diagram: `products` is where it was left. **Export › SVG…** writes the same boxes and lines.
#[test]
fn draws_moves_reopens_and_exports_a_diagram_on_postgres() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let database = ExitDatabase::create(&server, &runtime);
    let dir = tempfile::tempdir().unwrap();
    let source = server.data_source(&database.name, "pg");
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let start = || {
        let (_layer, feed) = log_console::pipeline();
        crate::test_support::start_app(
            harness_at_default_size(),
            dir.path(),
            feed,
            server.secrets(&source),
        )
    };

    let mut harness = start();
    for (expand, appears) in [
        ("Expand pg", format!("Expand {}", database.name)),
        (
            &*format!("Expand {}", database.name),
            "Expand public".to_owned(),
        ),
    ] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
        step_until_visible(&mut harness, &appears);
    }
    node_menu(&mut harness, "public", "Diagram…");
    step_until_visible(&mut harness, "audit_log");
    harness.run_steps(2);
    for (role, label) in [
        (Role::Button, "None"),
        (Role::CheckBox, "orders"),
        (Role::CheckBox, "Add the tables these reference"),
        (Role::Button, "Open"),
    ] {
        harness.get_by_role_and_label(role, label).click();
        harness.run_steps(2);
    }
    let (key, laid_out) = loaded_diagram(&mut harness);
    assert_eq!(table_names(&laid_out), ["customers", "orders", "products"]);
    assert_eq!(laid_out.key_lines().len(), 2, "orders' two keys");
    assert!(left_of(&laid_out, "customers") < left_of(&laid_out, "orders"));

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
        harness.hover_at(products + egui::vec2(20.0, 25.0) * step as f32);
        harness.run_steps(1);
    }
    harness.drop_at(products + egui::vec2(80.0, 100.0));
    harness.run_steps(2);
    let (_, moved) = loaded_diagram(&mut harness);
    assert_ne!(
        top_left(&moved, "products"),
        top_left(&laid_out, "products")
    );
    let file = dir.path().join("diagrams/_none/public.toml");
    step_until(
        &mut harness,
        "the diagram's file being written",
        |harness| {
            let saved = std::fs::read_to_string(&file).unwrap_or_default();
            let (_, diagram) = harness.state().open_diagrams().next().unwrap();
            toml::from_str::<dbrover_store::DiagramFile>(&saved).ok() == Some(diagram.to_file())
        },
    );
    drop(harness);

    let mut harness = start();
    step_until_visible(&mut harness, "Expand pg");
    harness.get_by_label("pg").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Diagrams").hover();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::Button, "public")
        .click();
    let (_, reopened) = loaded_diagram(&mut harness);
    assert_eq!(
        top_left(&reopened, "products"),
        top_left(&moved, "products").round(),
        "where it was left"
    );

    let svg_path = dir.path().join("public.svg");
    let answer = svg_path.clone();
    harness.state_mut().pick_picture = Box::new(move |_, _| Some(answer.clone()));
    harness.get_by_label("Export").click();
    harness.run_steps(2);
    harness.get_by_label("SVG…").click();
    step_until_visible(&mut harness, "Exported to");
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    for table in ["customers", "orders", "products"] {
        assert!(
            svg.contains(&format!(">{table}</text>")),
            "{table} in the SVG"
        );
    }
    assert!(!svg.contains(">audit_log</text>"));
}
