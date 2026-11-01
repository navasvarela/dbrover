//! Power in the grid (W5), driven through the whole app against SQLite.

use egui_kittest::kittest::NodeT;

use super::*;

/// The orders as the database has them, by id.
fn orders(dir: &Path) -> Vec<(i64, String)> {
    let connection = rusqlite::Connection::open(dir.join("shop.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT id, customer FROM orders ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The grid's filter field, by its hint, while the database sorts and filters.
fn grid_filter<'a>(harness: &'a Harness<'_, DbRoverApp>) -> egui_kittest::Node<'a> {
    harness.get_by(|node| {
        node.role() == Role::TextInput && node.placeholder() == Some("Filter every row, then Enter")
    })
}

/// Waits until a submit has been answered: the pending changes and "Saving…" are gone.
fn wait_for_submit(harness: &mut Harness<'_, DbRoverApp>) {
    step_until(harness, "the changes being saved", |harness| {
        harness.query_by_label_contains("changed cell").is_none()
            && harness.query_by_label_contains("new row").is_none()
            && harness.query_by_label("Saving…").is_none()
    });
}

#[test]
fn adds_edits_and_deletes_rows_in_one_submit() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT NOT NULL DEFAULT 'walk-in');
         INSERT INTO orders VALUES (1, 'ada'), (2, 'bob');",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "2 rows");
    rename_ada(&mut harness);

    harness.get_by_label("bob").click_secondary();
    harness.run();
    harness.get_by_label("Delete row").click();
    harness.run();
    harness.get_by_label("Add row").click();
    harness.run();
    harness.get_by_label("1 changed cell · 1 new row · 1 deleted row");
    // The new row's id has no default but SQLite numbers it; its customer has one.
    harness.get_by_label("DEFAULT").click_secondary();
    harness.run();
    harness.get_by_label("Edit value").click();
    harness.run();
    harness
        .get_by(|node| node.role() == Role::TextInput && node.is_focused())
        .type_text("cy");
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(harness.query_by_label("DEFAULT").is_none());

    harness.get_by_label("Submit").click();
    wait_for_submit(&mut harness);
    // Adding a row runs the statement again, so the id SQLite gave it shows: the deleted
    // row's, since the delete ran first.
    step_until(&mut harness, "the statement running again", |harness| {
        harness.query_by_label("cy").is_some() && harness.query_by_label("bob").is_none()
    });
    assert_eq!(
        orders(dir.path()),
        [(1, "Ada Lovelace".to_owned()), (2, "cy".to_owned())]
    );
}

#[test]
fn a_failed_submit_names_the_new_row_and_keeps_every_change() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT NOT NULL);
         INSERT INTO orders VALUES (1, 'ada');",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "1 rows");
    rename_ada(&mut harness);
    harness.get_by_label("Add row").click();
    harness.run();

    // The customer column has no default and refuses NULL.
    harness.get_by_label("Submit").click();
    step_until_visible(&mut harness, "New row 1: NOT NULL constraint failed");
    harness.get_by_label("1 changed cell · 1 new row");
    assert_eq!(orders(dir.path()), [(1, "ada".to_owned())]);

    harness.get_by_label("Revert").click();
    harness.run();
    assert!(harness.query_by_label("Add row").is_some());
    assert!(harness.query_by_label_contains("new row").is_none());
}

/// From an order to its customer through the key's link, then from the customer back to their
/// orders through Referencing rows: each opens in a new console.
#[test]
fn follows_a_foreign_key_to_its_row_and_back_to_the_rows_that_refer_to_it() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE orders (id INTEGER PRIMARY KEY,
             customer_id INTEGER REFERENCES customers (id));
         INSERT INTO customers VALUES (7, 'ada'), (8, 'bob');
         INSERT INTO orders VALUES (10, 8), (11, 7), (12, 8);",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "3 rows");
    // Add row shows once the table is described, and with it its keys.
    step_until_visible(&mut harness, "Add row");

    harness.get_by_label("7").click_secondary();
    harness.run();
    harness.get_by_label("Open customers row").click();
    step_until_visible(&mut harness, "ada");
    harness.get_by_label_contains("1 rows");
    assert!(harness.query_by_label("bob").is_none());
    step_until_visible(&mut harness, "Add row");

    harness.get_by_label("ada").click_secondary();
    harness.run();
    harness.get_by_label_contains("Referencing rows").click();
    harness.run();
    harness.get_by_label("orders.customer_id").click();
    step_until_visible(&mut harness, "11");
    harness.get_by_label_contains("1 rows");
    assert!(harness.query_by_label("10").is_none());
}

/// With In the database ticked, sorting and filtering run the statement again wrapped, so
/// they cover rows that weren't loaded, and the rows stay editable.
#[test]
fn sorts_and_filters_every_row_in_the_database() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 600)
         INSERT INTO orders SELECT i, 'customer ' || i FROM n;",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "500 rows, more available");
    step_until_visible(&mut harness, "Add row");

    harness.get_by_label("In the database").click();
    harness.run();
    harness.get_by_label("id").click();
    step_until_visible(&mut harness, "id ▲");
    step_until_visible(&mut harness, "Sorted and filtered by the database");
    harness.get_by_label("id ▲").click();
    step_until_visible(&mut harness, "customer 600");

    grid_filter(&harness).focus();
    harness.run();
    grid_filter(&harness).type_text("ER 59");
    harness.key_press(egui::Key::Enter);
    // customer 59 and customer 590 to 599, sorted by id descending.
    step_until_visible(&mut harness, "11 rows");
    harness.get_by_label("customer 599");
    assert!(harness.query_by_label("customer 600").is_none());
    step_until_visible(&mut harness, "Add row");

    harness.get_by_label("In the database").click();
    step_until_visible(&mut harness, "500 rows, more available");
    harness.get_by_label("customer 1");
}

/// Export › All rows runs the statement again and writes every row, not only the 500 loaded;
/// sorted in the database, the file is in the grid's order.
#[test]
fn exports_every_row_not_only_those_loaded() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 600)
         INSERT INTO orders SELECT i, 'customer ' || i FROM n;",
    );
    let export_path = dir.path().join("orders.csv");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = export_path.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
    open_orders(&mut harness, "500 rows, more available");
    step_until_visible(&mut harness, "Add row");
    harness.get_by_label("In the database").click();
    harness.run();
    harness.get_by_label("id").click();
    step_until_visible(&mut harness, "id ▲");
    harness.get_by_label("id ▲").click();
    step_until_visible(&mut harness, "customer 600");

    open_export_dialog(&mut harness, "All rows", None);
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Exported 600 rows");

    let text = std::fs::read_to_string(&export_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 601);
    assert_eq!(
        (lines[0], lines[1], lines[600]),
        ("id,customer", "600,customer 600", "1,customer 1")
    );
}

/// Export › Loaded rows offers every format, each saved under its own extension: CSV for
/// Excel starts with a byte order mark, Markdown writes a table, the SQL formats write to the
/// table the grid edits, and Excel and Parquet write their own kinds of file.
#[test]
fn exports_the_loaded_rows_in_other_formats() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'Zoë | Ltd'), (2, NULL);",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (folder, paths) = (dir.path().to_owned(), picked.clone());
    harness.state_mut().pick_export_path = Box::new(move |file, _| {
        let mut paths = paths.lock().unwrap();
        let path = folder.join(format!("orders{}.{}", paths.len(), file.extension));
        paths.push(path.clone());
        Some(path)
    });
    open_orders(&mut harness, "2 rows");
    step_until_visible(&mut harness, "Add row");

    let expected = [
        (
            "CSV for Excel",
            "\u{feff}id,customer\r\n1,Zoë | Ltd\r\n2,\r\n",
        ),
        (
            "Markdown",
            "| id | customer |\n| --- | --- |\n| 1 | Zoë \\| Ltd |\n| 2 | NULL |\n",
        ),
        (
            "SQL INSERT",
            "INSERT INTO \"main\".\"orders\" (\"id\", \"customer\") VALUES (1, 'Zoë | Ltd');\n\
             INSERT INTO \"main\".\"orders\" (\"id\", \"customer\") VALUES (2, NULL);\n",
        ),
        (
            "SQL UPDATE",
            "UPDATE \"main\".\"orders\" SET \"customer\" = 'Zoë | Ltd' WHERE \"id\" = '1';\n\
             UPDATE \"main\".\"orders\" SET \"customer\" = NULL WHERE \"id\" = '2';\n",
        ),
    ];
    for (index, (format, contents)) in expected.into_iter().enumerate() {
        open_export_dialog(&mut harness, "Loaded rows", Some(format));
        confirm_export(&mut harness);
        step_until_visible(&mut harness, &format!("orders{index}."));
        let path = picked.lock().unwrap()[index].clone();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            contents,
            "{format}"
        );
    }
    for (index, (format, magic)) in [("Excel", &b"PK"[..]), ("Parquet", &b"PAR1"[..])]
        .into_iter()
        .enumerate()
    {
        open_export_dialog(&mut harness, "Loaded rows", Some(format));
        confirm_export(&mut harness);
        step_until_visible(&mut harness, &format!("orders{}.", index + 4));
        let file = std::fs::read(&picked.lock().unwrap()[index + 4]).unwrap();
        assert!(
            file.starts_with(magic),
            "{format} starts with its magic number"
        );
    }
    let extensions: Vec<String> = picked
        .lock()
        .unwrap()
        .iter()
        .filter_map(|path| Some(path.extension()?.to_str()?.to_owned()))
        .collect();
    assert_eq!(extensions, ["csv", "md", "sql", "sql", "xlsx", "parquet"]);
}

/// For rows of no one table, UPDATE is greyed out and INSERT asks for the table's name.
#[test]
fn asks_which_table_to_insert_rows_of_no_one_table_into() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'a');",
    );
    let export_path = dir.path().join("copy.sql");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = export_path.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
    open_console(&mut harness);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("SELECT a.id, b.customer FROM orders a JOIN orders b USING (id)");
    harness.run();
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "Read-only");

    open_export_dialog(&mut harness, "Loaded rows", None);
    assert!(
        harness
            .get_by_label("SQL UPDATE")
            .accesskit_node()
            .is_disabled()
    );
    harness.get_by_label("SQL INSERT").click();
    harness.run();
    harness.get_by_label_contains("Name the table to insert into");
    let export = harness.get_all_by_label("Export…").last().unwrap();
    assert!(export.accesskit_node().is_disabled());

    let table = harness.get_by_role_and_label(Role::TextInput, "Table");
    table.click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Table")
        .type_text("archive");
    harness.run();
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "copy.sql");
    assert_eq!(
        std::fs::read_to_string(&export_path).unwrap(),
        "INSERT INTO \"archive\" (\"id\", \"customer\") VALUES (1, 'a');\n"
    );
}

/// The dialog starts from the format and options used last, CSV for Excel shown by name.
#[test]
fn remembers_the_format_and_options_used_last() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'a;b');",
    );
    let export_path = dir.path().join("orders.csv");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = export_path.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
    open_orders(&mut harness, "1 row");

    open_export_dialog(&mut harness, "Loaded rows", Some("CSV for Excel"));
    harness.get_by_label("Semicolon").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::CheckBox, "Header with the column names")
        .click();
    harness.run();
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Exported 1 rows");
    assert_eq!(
        std::fs::read_to_string(&export_path).unwrap(),
        "\u{feff}1;\"a;b\"\r\n"
    );

    open_export_dialog(&mut harness, "Loaded rows", None);
    for shown in ["CSV", "Semicolon"] {
        assert!(is_chosen(&harness, shown), "{shown} is chosen");
    }
    assert_eq!(
        harness.state().export_choices.format,
        dbrover_export::ExportFormat::Csv
    );
    assert!(!harness.state().export_choices.options.csv.header);
}

/// Copy as ›, Copy with headers (⇧⌘C) and Copy row, from the cell menu and the keys; the
/// selected columns of the selected rows, as ⌘C copies them.
#[test]
fn copies_the_selected_cells_in_other_formats() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'Zoë | Ltd'), (2, 'tab' || char(9) || 'quote\"');",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "2 rows");
    step_until_visible(&mut harness, "Add row");

    let copy_as = |harness: &mut Harness<'_, DbRoverApp>, cell: &str, format: &str| {
        harness.get_by_label(cell).click_secondary();
        harness.run();
        harness.get_by_label("Copy as ⏵").click();
        harness.run();
        harness.get_by_label(format).click();
        harness.step();
        copied(harness)
    };
    assert_eq!(
        copy_as(&mut harness, "Zoë | Ltd", "Markdown").as_deref(),
        Some("| customer |\n| --- |\n| Zoë \\| Ltd |\n")
    );
    harness.run();

    // The row number comes first, then the id cell.
    harness.get_all_by_label("1").last().unwrap().click();
    harness.run();
    harness
        .get_by_label("Zoë | Ltd")
        .click_modifiers(egui::Modifiers::SHIFT);
    harness.run();
    assert_eq!(
        copy_as(&mut harness, "Zoë | Ltd", "SQL INSERT").as_deref(),
        Some("INSERT INTO \"main\".\"orders\" (\"id\", \"customer\") VALUES (1, 'Zoë | Ltd');\n")
    );
    harness.run();

    // The press alone, in a frame of its own: a frame for its release would replace the
    // output the copy is in.
    harness.event(egui::Event::Key {
        key: egui::Key::C,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    });
    harness.step();
    assert_eq!(
        copied(&harness).as_deref(),
        Some("id\tcustomer\n1\tZoë | Ltd\n")
    );
    harness.run();

    harness.get_by_label("tab\tquote\"").click_secondary();
    harness.run();
    harness.get_by_label("Copy row").click();
    harness.step();
    assert_eq!(copied(&harness).as_deref(), Some("2\t\"tab\tquote\"\"\""));
}

/// Export data… on a table writes every row on the explorer's connection; the dialog shows the
/// table's columns once described, and how the export went.
#[test]
fn exports_a_table_from_the_explorer() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'a'), (2, NULL);",
    );
    let export_path = dir.path().join("orders.csv");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = export_path.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
    for expand in ["Expand shop", "Expand main", "Expand tables"] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
    }
    step_until_visible(&mut harness, "Expand orders");
    harness.get_by_label("orders").click_secondary();
    harness.run();
    harness.get_by_label("Export data…").click();
    harness.run();
    step_until_visible(&mut harness, "id,customer");

    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Exported 2 rows");
    assert_eq!(
        std::fs::read_to_string(&export_path).unwrap(),
        "id,customer\r\n1,a\r\n2,\r\n"
    );
    harness.get_by_label("Close").click();
    harness.run();
    assert!(harness.state().export_dialog.is_none());
}

/// Run to file (⌥⌘↵) asks for the format and file, then the statement's parameters, and writes
/// its rows without loading the grid, which says where they went.
#[test]
fn runs_a_statement_straight_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY);
         INSERT INTO orders VALUES (1), (2), (3);",
    );
    let export_path = dir.path().join("big.jsonl");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = export_path.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
    open_console(&mut harness);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("SELECT id FROM orders WHERE id >= :min ORDER BY id");
    // Steps rather than `run`: the console's checks of what was typed keep it repainting.
    harness.run_steps(2);

    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
        egui::Key::Enter,
    );
    step_until_visible(&mut harness, "Run to file");
    harness.run_steps(2);
    harness.get_by_label("JSON Lines").click();
    harness.run();
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Parameter values");
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, ":min")
        .type_text("2");
    harness.run();
    harness.key_press(egui::Key::Enter);
    step_until_visible(&mut harness, "Exported 2 rows to");

    assert_eq!(
        std::fs::read_to_string(&export_path).unwrap(),
        "{\"id\":\"2\"}\n{\"id\":\"3\"}\n"
    );
    assert!(
        harness.query_by_label("Add row").is_none(),
        "the grid didn't load the rows"
    );
}

/// Saves `shop` with an orders table at `safety`, starts the app, and opens Import CSV… on
/// the table with `csv` as the file.
fn import_csv_into_orders(
    dir: &Path,
    safety: dbrover_core::SafetyLevel,
    csv: &str,
) -> Harness<'static, DbRoverApp> {
    let shop = save_shop(
        dir,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT NOT NULL, qty INTEGER,
             note TEXT DEFAULT 'none');
         INSERT INTO orders VALUES (1, 'ada', 1, 'first');",
    );
    DataSourceFile::new(dir.join("data_sources.toml"))
        .save(&[dbrover_core::DataSource { safety, ..shop }])
        .unwrap();
    let file = dir.join("orders.csv");
    std::fs::write(&file, csv).unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir, feed);
    harness.state_mut().pick_csv_file = Box::new(move || Some(file.clone()));
    expand_shop_tables(&mut harness, "orders");
    harness.get_by_label("orders").click_secondary();
    harness.run();
    harness.get_by_label("Import CSV…").click();
    step_until_visible(&mut harness, "Source of customer");
    // The dialog settles where it is centred before a pointer can find its buttons.
    harness.run();
    harness
}

/// The file's columns are matched to the table's by header name, a value that doesn't fit
/// its column is flagged, and Import inserts the rows, the unmatched column at its default.
#[test]
fn imports_a_csv_file_matching_its_columns_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = import_csv_into_orders(
        dir.path(),
        dbrover_core::SafetyLevel::Normal,
        "Customer,ID,Qty\r\nbob,2,3\r\n\"cy, jr\",3,lots\r\n",
    );
    harness.get_by_label("Import into main.orders");
    harness.get_by(|node| {
        node.role() == Role::ComboBox
            && node.label().as_deref() == Some("Source of id")
            && node.value().as_deref() == Some("ID")
    });
    harness.get_by_label_contains("Line 3, qty: \u{201c}lots\u{201d} isn't a whole number");

    harness.get_by_label("Import").click();
    step_until_visible(&mut harness, "Imported 2 rows into main.orders");
    let connection = rusqlite::Connection::open(dir.path().join("shop.db")).unwrap();
    let imported: Vec<(i64, String, String)> = connection
        .prepare("SELECT id, customer, note FROM orders WHERE id > 1 ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        imported,
        [
            (2, "bob".to_owned(), "none".to_owned()),
            (3, "cy, jr".to_owned(), "none".to_owned())
        ]
    );
    // The dialog grew by the status line, and is centred again before Close can be clicked.
    harness.run();
    harness.get_by_label("Close").click();
    harness.run();
    assert!(harness.query_by_label("Import into main.orders").is_none());
}

/// A careful data source asks first; a row the database refuses rolls everything back, and
/// the dialog names its line.
#[test]
fn a_failed_import_names_its_line_and_imports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = import_csv_into_orders(
        dir.path(),
        dbrover_core::SafetyLevel::Careful,
        "id,customer\n2,bob\n1,again\n",
    );
    harness.get_by_label("Import").click();
    harness.run();
    harness.get_by_label_contains("\u{201c}shop\u{201d} is careful: this inserts the file's rows");
    harness.get_by_label("Import").click();

    step_until_visible(&mut harness, "Line 3: UNIQUE constraint failed: orders.id");
    harness.get_by_label_contains("Nothing was imported.");
    let count: i64 = rusqlite::Connection::open(dir.path().join("shop.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM orders", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

/// A JSON value opens as a tree (N0b): an object or array folds and unfolds, numbers stay as
/// written, and each line copies its value or its path.
#[test]
fn opens_a_json_value_as_a_tree_and_copies_a_path() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        r#"CREATE TABLE orders (id INTEGER PRIMARY KEY, detail TEXT);
           INSERT INTO orders VALUES
             (1, '{"items": [{"sku": "a-1", "qty": 2}, {"sku": "b-2", "qty": 1.10}], "gift": true}');"#,
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "1 rows");

    harness
        .get_by_label_contains(r#"{"items": [{"sku""#)
        .click();
    harness.run();
    harness.key_press(egui::Key::Space);
    harness.run();
    harness.get_by_label("Value of detail");
    harness.get_by_label("[2 items]");
    assert!(
        harness.query_by_label("b-2").is_none(),
        "items starts folded"
    );

    harness.get_by_label("items").click();
    harness.run();
    harness
        .get_all_by_label("{2 fields}")
        .last()
        .unwrap()
        .click();
    harness.run();
    harness.get_by_label("b-2");

    let copy = |harness: &mut Harness<'_, DbRoverApp>, line: &str, item: &str| {
        harness.get_by_label(line).click_secondary();
        harness.run();
        harness.get_by_label(item).click();
        harness.step();
        let text = copied(harness);
        harness.run();
        text
    };
    assert_eq!(
        copy(&mut harness, "1.10", "Copy path").as_deref(),
        Some("items[1].qty")
    );
    assert_eq!(
        copy(&mut harness, "1.10", "Copy value").as_deref(),
        Some("1.10")
    );
    assert_eq!(
        copy(&mut harness, "b-2", "Copy all").as_deref(),
        Some(r#"{"items":[{"sku":"a-1","qty":2},{"sku":"b-2","qty":1.10}],"gift":true}"#)
    );
}
