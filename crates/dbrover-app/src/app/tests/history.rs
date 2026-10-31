//! The history as an audit trail (W4), driven through the whole app against SQLite.

use dbrover_store::{HistoryEntry, HistoryFile, HistoryOutcome, HistorySource};

use super::*;

/// Writes entries straight into the history file, before the app starts, as another process
/// would.
fn record(dir: &Path, data_source: dbrover_core::DataSourceId, entries: &[(&str, HistorySource)]) {
    let history = HistoryFile::new(dir.join("history.sqlite")).open().unwrap();
    for (at, (sql, source)) in (1_i64..).zip(entries) {
        history
            .record(&HistoryEntry {
                data_source,
                executed_at: jiff::Timestamp::from_second(at).unwrap(),
                sql: (*sql).to_owned(),
                duration: None,
                rows: Some(1),
                outcome: HistoryOutcome::Ok,
                source: source.clone(),
            })
            .unwrap();
    }
}

fn open_history(harness: &mut Harness<'_, DbRoverApp>, entries: usize) {
    harness.get_by_label("History").click();
    step_until(harness, "the history loading", |harness| {
        harness.query_all_by_label("Insert").count() == entries
    });
}

fn choose(harness: &mut Harness<'_, DbRoverApp>, menu: &str, choice: &str) {
    harness
        .get_by(|node| node.role() == Role::ComboBox && node.value().as_deref() == Some(menu))
        .click();
    harness.run();
    harness.get_by_label(choice).click();
}

#[test]
fn marks_each_statement_with_where_it_came_from_and_filters_by_it() {
    let dir = tempfile::tempdir().unwrap();
    let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    record(
        dir.path(),
        shop.id,
        &[
            (
                "CREATE TABLE refunds (id INTEGER)",
                HistorySource::ObjectHelper,
            ),
            (
                "UPDATE \"orders\" SET \"id\" = '2' WHERE \"id\" = '1'",
                HistorySource::GridEdit(None),
            ),
            (
                "INSERT INTO \"orders\" (\"id\") VALUES\n\u{2026}",
                HistorySource::Import {
                    file: "/data/orders.csv".into(),
                },
            ),
            ("SELECT 'from before'", HistorySource::DbRover),
        ],
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "0 rows");

    // The four recorded, and the console's own SELECT.
    open_history(&mut harness, 5);
    for badge in ["Console", "Object", "Grid", "Import", "DbRover"] {
        harness.get_by_label(badge);
    }
    choose(&mut harness, "All sources", "Grid edits");
    step_until(&mut harness, "the grid edits alone", |harness| {
        harness.query_all_by_label("Insert").count() == 1
    });
    harness.get_by_label_contains("UPDATE \"orders\"");
}

/// A cell edited in the grid shows in the history as a grid edit with the value it replaced,
/// and the statement that puts it back goes into the console.
#[test]
fn shows_a_grid_edits_old_values_and_the_statement_to_put_them_back() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'ada');",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "1 rows");
    rename_ada(&mut harness);
    harness.get_by_label("Submit").click();
    step_until(&mut harness, "the edit being saved", |harness| {
        harness.query_by_label_contains("changed cell").is_none()
            && harness.query_by_label("Saving…").is_none()
    });

    open_history(&mut harness, 2);
    harness
        .get_by_label_contains("UPDATE \"main\".\"orders\"")
        .click();
    harness.run();
    harness.get_by_label("Submitted from the grid, on main.orders.");
    harness.get_by_label("Changed the row where id = 1:");
    for heading in ["Before", "After"] {
        harness.get_by_label(heading);
    }
    let revert = r#"UPDATE "main"."orders" SET "customer" = 'ada' WHERE "id" = '1'"#;
    harness.get_by_label(revert);
    harness.get_by_label("Insert into console").click();
    harness.run();
    assert!(
        editor_text(&harness).contains(revert),
        "{}",
        editor_text(&harness)
    );
}

/// Every data source's statements, named (a data source no longer saved too), and only the
/// recent ones once a period is chosen.
#[test]
fn searches_every_data_source_and_a_recent_period() {
    let dir = tempfile::tempdir().unwrap();
    let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let removed = dbrover_core::DataSourceId::generate();
    // At 1970's first seconds: older than any period.
    record(
        dir.path(),
        removed,
        &[(
            "SELECT 'from a removed data source'",
            HistorySource::Console,
        )],
    );
    record(
        dir.path(),
        shop.id,
        &[("SELECT 'long ago'", HistorySource::Console)],
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "0 rows");

    open_history(&mut harness, 2);
    // The name is in the explorer and the console's tab too.
    let named_elsewhere = harness.query_all_by_label(&shop.name).count();
    choose(&mut harness, "This data source", "Every data source");
    step_until(&mut harness, "every data source's statements", |harness| {
        harness.query_all_by_label("Insert").count() == 3
    });
    harness.get_by_label("Removed data source");
    harness.get_by_label("SELECT 'from a removed data source'");
    assert_eq!(
        harness.query_all_by_label(&shop.name).count(),
        named_elsewhere + 2
    );

    choose(&mut harness, "Any time", "Last hour");
    step_until(&mut harness, "the last hour's statements", |harness| {
        harness.query_all_by_label("Insert").count() == 1
    });
    harness.get_by_label_contains("SELECT * FROM main.orders");
}

/// Choosing how long to keep statements saves it and deletes the older ones at once.
#[test]
fn keeps_the_history_as_long_as_the_settings_say() {
    let dir = tempfile::tempdir().unwrap();
    let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    record(
        dir.path(),
        shop.id,
        &[("SELECT 'from 1970'", HistorySource::Console)],
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_orders(&mut harness, "0 rows");

    harness.get_by_label("View").click();
    harness.run_steps(2);
    harness.get_by_label_contains("Settings…").click();
    harness.run_steps(2);
    harness.get_by_label("Query history").click();
    harness.run_steps(2);
    let no_limit = |node: &egui_kittest::kittest::AccessKitNode<'_>| {
        node.role() == Role::ComboBox && node.value().as_deref() == Some("no limit")
    };
    harness.get_all_by(no_limit).next().unwrap().click();
    harness.run();
    harness.get_by_label("a year").click();
    step_until(&mut harness, "the settings file", |_| {
        std::fs::read_to_string(dir.path().join("settings.toml"))
            .is_ok_and(|text| text.contains("max_age_days = 365"))
    });

    // The tab is in front of the console and its toolbar.
    close_settings_tab(&mut harness);
    open_history(&mut harness, 1);
    assert!(harness.query_by_label("SELECT 'from 1970'").is_none());
}

/// Export writes what the window lists to the file the save dialog names, and says so.
#[test]
fn exports_the_search_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = dir.path().join("history.json");
    let answer = picked.clone();
    harness.state_mut().pick_export_path = Box::new(move |_, what| {
        assert_eq!(what, "history");
        Some(answer.clone())
    });
    open_orders(&mut harness, "0 rows");

    open_history(&mut harness, 1);
    harness.get_by_label("Export history").click();
    harness.run();
    harness.get_by_label("As JSON…").click();
    step_until(&mut harness, "the export", |harness| {
        harness
            .query_by_label_contains("Exported 1 statement to")
            .is_some()
    });
    let exported: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&picked).unwrap()).unwrap();
    assert_eq!(exported[0]["sql"], "SELECT * FROM main.orders");
    assert_eq!(exported[0]["data_source"], "shop");
}
