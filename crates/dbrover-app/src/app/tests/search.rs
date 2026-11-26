//! Search data (W16) through the whole app, on SQLite: right-click a data source → **Search
//! data…**, a value found in two tables and opened on its row, a table with no column that can
//! hold it said so, and a careful data source asking first.

use super::*;

const SHOP: &str = "
    CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
    INSERT INTO customers VALUES (1, 'Kate O''Brien', 'kate@example.com'),
                                 (2, 'Liam Hale', 'liam@example.org');
    CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
    INSERT INTO notes VALUES (88, 'called O''Brien about the refund');
    CREATE TABLE amounts (id INTEGER PRIMARY KEY, total INTEGER);
    INSERT INTO amounts VALUES (1, 300);
";

fn open_search(harness: &mut Harness<'_, DbRoverApp>) {
    step_until_visible(harness, "shop");
    harness.get_by_label("shop").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Search data\u{2026}").click();
    step_until_visible(harness, "A value to find");
}

fn search_for(harness: &mut Harness<'_, DbRoverApp>, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, "A value to find")
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "A value to find")
        .type_text(value);
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::Button, "Search")
        .click();
}

#[test]
fn finds_a_value_in_two_tables_and_opens_its_row() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_search(&mut harness);

    search_for(&mut harness, "o'brien");
    step_until_visible(&mut harness, "Searched 2 tables");
    assert!(harness.query_by_label_contains("main.customers").is_some());
    assert!(harness.query_by_label_contains("main.notes").is_some());
    assert!(
        harness
            .query_by_label_contains("1 table with no column that can hold the value")
            .is_some()
    );
    assert!(harness.query_by_label_contains("Kate O'Brien").is_some());

    // The customer's row, by its key, in a new console whose grid can edit it.
    harness.get_all_by_label("Open row").next().unwrap().click();
    step_until_visible(&mut harness, "1 rows");
    step_until_visible(&mut harness, "Add row");
}

#[test]
fn a_careful_data_source_asks_before_searching_its_tables() {
    let dir = tempfile::tempdir().unwrap();
    let mut shop = save_shop(dir.path(), SHOP);
    shop.safety = dbrover_core::SafetyLevel::Careful;
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&shop))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_search(&mut harness);

    search_for(&mut harness, "o'brien");
    step_until_visible(&mut harness, "Search 2 tables of \u{201c}shop\u{201d}");
    // A modal is measured on its first frame and placed on the next.
    harness.run_steps(2);
    harness.get_by_label("Not now").click();
    step_until(&mut harness, "the question closing", |harness| {
        harness.query_by_label("Not now").is_none()
    });
    assert!(harness.query_by_label_contains("main.customers").is_none());

    harness
        .get_by_role_and_label(Role::Button, "Search")
        .click();
    step_until_visible(&mut harness, "Search 2 tables of \u{201c}shop\u{201d}");
    harness.run_steps(2);
    harness
        .get_all_by_role_and_label(Role::Button, "Search")
        .last()
        .unwrap()
        .click();
    step_until_visible(&mut harness, "Searched 2 tables");
    assert!(harness.query_by_label_contains("main.notes").is_some());
}
