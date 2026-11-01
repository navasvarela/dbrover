//! Custom actions from `actions.toml` (H1b), driven through the whole app against SQLite with a
//! temporary config folder.

use super::*;

/// A table whose name would be a second statement if it were pasted into SQL as written.
const HOSTILE: &str = "it's; DROP TABLE orders; --";

const ACTIONS: &str = r#"
[[action]]
id = "row-count"
name = "Row count"
applies_to = ["table", "view"]
sql = "SELECT count(*) AS n FROM {qualified_name}"

[[action]]
id = "latest"
name = "Last 100 rows"
applies_to = ["table"]
sql = "SELECT * FROM {qualified_name} ORDER BY {column:created_at} DESC LIMIT 100"
run = false

[[action]]
id = "wipe"
name = "Wipe"
applies_to = ["table"]
sql = "DELETE FROM {qualified_name}"

[[action]]
id = "forget"
name = "Forget these orders"
applies_to = ["grid.row"]
sql = "DELETE FROM orders WHERE id = {row.id}"
"#;

fn start_shop(dir: &Path, keymap: Option<&str>) -> Harness<'static, DbRoverApp> {
    save_shop(
        dir,
        &format!(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT, created_at TEXT);
             INSERT INTO orders VALUES (1, 'ada', '2026-01-01'), (2, 'bob', '2026-02-01'),
                 (3, 'cy', '2026-03-01');
             CREATE TABLE \"{}\" (id INTEGER PRIMARY KEY);
             WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 12)
                 INSERT INTO \"{}\" SELECT x FROM n;",
            HOSTILE.replace('"', "\"\""),
            HOSTILE.replace('"', "\"\"")
        ),
    );
    std::fs::write(dir.join("actions.toml"), ACTIONS).unwrap();
    if let Some(keymap) = keymap {
        std::fs::write(dir.join("keymap.toml"), keymap).unwrap();
    }
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir, feed);
    for expand in ["Expand shop", "Expand main", "Expand tables"] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
    }
    step_until_visible(&mut harness, "Expand orders");
    harness
}

fn order_ids(dir: &Path) -> Vec<i64> {
    let connection = rusqlite::Connection::open(dir.join("shop.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT id FROM orders ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn runs_an_action_on_a_hostile_table_name_and_records_it_as_the_actions() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_shop(dir.path(), None);
    harness.get_by_label(HOSTILE).click_secondary();
    harness.run();

    harness.get_by_label("Row count").click();

    step_until_visible(&mut harness, "1 rows");
    harness.get_by_label("12");
    assert_eq!(
        editor_text(&harness),
        "SELECT count(*) AS n FROM \"main\".\"it's; DROP TABLE orders; --\";\n"
    );
    assert_eq!(order_ids(dir.path()), [1, 2, 3]);
    harness.get_by_label("History").click();
    step_until_visible(&mut harness, "Action");
}

#[test]
fn a_key_and_the_actions_menu_act_on_the_selected_table() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_shop(
        dir.path(),
        Some("[bindings]\n\"custom.latest\" = \"f6\"\n\"custom.nothing\" = \"f7\"\n"),
    );
    step_until_visible(&mut harness, "keymap.toml: 1 problem");
    harness.get_by_label("orders").click();
    harness.run();

    harness.key_press(egui::Key::F6);
    step_until(&mut harness, "the action's console opening", |harness| {
        harness.query_by_role(Role::MultilineTextInput).is_some()
    });
    assert_eq!(
        editor_text(&harness),
        "SELECT * FROM \"main\".\"orders\" ORDER BY \"created_at\" DESC LIMIT 100;\n"
    );
    assert!(
        harness.query_by_label_contains("rows").is_none(),
        "run = false only opens it"
    );

    harness.get_by_label("Actions").click();
    harness.run();
    harness.get_by_label("Row count").click();
    step_until_visible(&mut harness, "1 rows");
    harness.get_by_label("3");
    harness.get_by_label("keymap.toml: 1 problem").click();
    harness.run();
    harness.get_by_label("\"custom.nothing\" is not an action in actions.toml");
    harness.get_by_label("custom.row-count");
}

#[test]
fn an_action_on_rows_runs_once_per_row_and_a_change_to_every_row_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_shop(dir.path(), None);
    harness.get_by_label("orders").click_secondary();
    harness.run();
    harness.get_by_label("Wipe").click();
    step_until_visible(&mut harness, "Run anyway?");
    harness.get_by_label("Cancel").click();
    harness.run();
    assert_eq!(order_ids(dir.path()), [1, 2, 3]);

    harness.get_by_label("orders").click_secondary();
    harness.run();
    harness.get_by_label("Open data").click();
    step_until_visible(&mut harness, "3 rows");
    harness.get_by_label("ada").click();
    harness.run();
    harness
        .get_by_label("bob")
        .click_modifiers(egui::Modifiers::SHIFT);
    harness.run();
    harness.get_by_label("bob").click_secondary();
    harness.run();
    harness.get_by_label("Forget these orders").click();

    step_until(&mut harness, "both rows going", |_| {
        order_ids(dir.path()) == [3]
    });
}

#[test]
fn view_actions_creates_the_file_once_and_lists_every_action_after_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Expand shop");
    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Custom actions…").click();
    harness.run();
    harness.get_by_label_contains("No actions yet.");
    harness.get_by_label("0 actions");

    harness.get_by_label("Create actions.toml").click();
    let file = dir.path().join("actions.toml");
    // The file exists before its text is in, and writing over it then would mix the two.
    step_until(&mut harness, "actions.toml being written", |_| {
        std::fs::read_to_string(&file).is_ok_and(|text| text == crate::custom_actions::TEMPLATE)
    });
    assert!(harness.query_by_label_contains("actions.toml: ").is_none());

    std::fs::write(&file, ACTIONS).unwrap();
    harness.get_by_label("Reload").click();
    step_until_visible(&mut harness, "4 actions");
    harness.get_by_label("custom.row-count");
    harness.get_by_label("table, view");
    harness.get_by_label("SELECT count(*) AS n FROM {qualified_name}");

    harness.get_by_label("Create actions.toml").click();
    step_until(&mut harness, "the second create being refused", |harness| {
        harness
            .query_by_label_contains("could not create actions.toml")
            .is_some()
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), ACTIONS);
    harness.get_by_label("actions.toml: 1 problem");
}

#[test]
fn view_actions_shows_a_command_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_shop(dir.path(), None);
    let jq = "\n[[action]]\nid = \"rows-to-jq\"\nname = \"Send rows to jq\"\n\
              applies_to = [\"grid.selection\"]\ncommand = [\"jq\", \"-r\", \".[] | .email\"]\n\
              stdin = \"json\"\n";
    std::fs::write(dir.path().join("actions.toml"), format!("{ACTIONS}{jq}")).unwrap();
    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Custom actions…").click();
    harness.run();
    harness.get_by_label("Reload").click();

    step_until_visible(&mut harness, "5 actions");
    harness.get_by_label(r#"["jq", "-r", ".[] | .email"]"#);
    harness.get_by_label("stdin json · output viewer · timeout 1m");
}
