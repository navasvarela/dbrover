//! Command actions (H2d), driven through the whole app against SQLite with a temporary config
//! folder. The engine runs `ScriptedPrograms` instead of real processes, so each output can be
//! tested without depending on what a CI machine has installed; the engine's own tests run
//! real programs.

use super::*;

/// A table whose name a shell would split at its spaces and end the command at.
const HOSTILE: &str = "it's; DROP TABLE orders; --";

const ACTIONS: &str = r#"
[[action]]
id = "echo-rows"
name = "Echo the rows"
applies_to = ["grid.selection"]
command = ["echo"]
stdin = "json"

[[action]]
id = "count-sql"
name = "Count in a console"
applies_to = ["table"]
command = ["print", "SELECT count(*) AS n FROM {qualified_name};"]
output = "console"

[[action]]
id = "log-table"
name = "Log the table"
applies_to = ["table"]
command = ["print", "{table}", "{data_source}"]
output = "log"

[[action]]
id = "break"
name = "Break"
applies_to = ["table"]
command = ["fail"]
output = "none"

[[action]]
id = "hang"
name = "Hang"
applies_to = ["table"]
command = ["hang"]
output = "none"

[[action]]
id = "hang-briefly"
name = "Hang briefly"
applies_to = ["table"]
command = ["hang"]
timeout = "200ms"
"#;

fn start_shop(dir: &Path, feed: LogFeed) -> Harness<'static, DbRoverApp> {
    save_shop(
        dir,
        &format!(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
             INSERT INTO orders VALUES (1, 'ada'), (2, 'bob'), (3, 'cy');
             CREATE TABLE \"{}\" (id INTEGER PRIMARY KEY);",
            HOSTILE.replace('"', "\"\"")
        ),
    );
    std::fs::write(dir.join("actions.toml"), ACTIONS).unwrap();
    let mut harness = start_app(dir, feed);
    for expand in ["Expand shop", "Expand main", "Expand tables"] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
    }
    step_until_visible(&mut harness, "Expand orders");
    harness
}

/// Right-clicks `table` in the explorer and chooses `action`.
fn choose_on_table(harness: &mut Harness<'_, DbRoverApp>, table: &str, action: &str) {
    harness.get_by_label(table).click_secondary();
    harness.run();
    harness.get_by_label(action).click();
}

/// Answers the question before a command's first run with **Run**.
fn run_it(harness: &mut Harness<'_, DbRoverApp>) {
    step_until_visible(harness, "Run a program?");
    // The modal sizes itself over its first frames; a click before then misses.
    harness.run();
    // A console's toolbar has a Run button too; the question is drawn after it.
    let Some(run) = harness.query_all_by_label("Run").last() else {
        panic!("the question has no Run button");
    };
    run.click();
    harness.step();
}

fn text_inputs(harness: &Harness<'_, DbRoverApp>) -> Vec<String> {
    harness
        .query_all_by_role(Role::MultilineTextInput)
        .filter_map(|input| input.value())
        .collect()
}

#[test]
fn asks_before_a_command_first_runs_and_shows_its_output_in_a_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_shop(dir.path(), feed);
    choose_on_table(&mut harness, "orders", "Open data");
    step_until_visible(&mut harness, "3 rows");
    harness.get_by_label("ada").click();
    harness.run();
    harness
        .get_by_label("bob")
        .click_modifiers(egui::Modifiers::SHIFT);
    harness.run();
    let echo = |harness: &mut Harness<'_, DbRoverApp>| {
        harness.get_by_label("bob").click_secondary();
        harness.run();
        harness.get_by_label("Echo the rows").click();
    };

    echo(&mut harness);
    step_until_visible(&mut harness, "Run a program?");
    harness.get_by_label("\u{201c}Echo the rows\u{201d} runs this command from actions.toml:");
    harness.get_by_label(r#"["echo"]"#);
    harness.get_by_label("the selected cells, as JSON");
    harness.get_by_label("shown in a window");
    harness.run();
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(harness.query_by_label("Run a program?").is_none());
    assert!(harness.query_by_label_contains("Output of").is_none());
    let trust_file = dir.path().join("trusted_commands.toml");
    assert!(!trust_file.exists(), "cancelling trusts nothing");

    echo(&mut harness);
    run_it(&mut harness);
    step_until_visible(&mut harness, "Output of Echo the rows");
    harness.get_by_label("JSON, formatted for reading; Copy copies it as stored.");
    let shown = text_inputs(&harness);
    assert!(
        shown
            .iter()
            .any(|text| text.contains("\"customer\": \"ada\"")
                && text.contains("\"customer\": \"bob\"")
                && !text.contains("\"id\"")),
        "{shown:?}"
    );
    step_until(&mut harness, "the command being trusted", |_| {
        trust_file.exists()
    });

    echo(&mut harness);
    harness.step();
    assert!(
        harness.query_by_label("Run a program?").is_none(),
        "a trusted command runs without asking"
    );
    step_until(&mut harness, "a second output window", |harness| {
        harness
            .query_all_by_label("Output of Echo the rows")
            .count()
            == 2
    });
}

#[test]
fn a_program_that_writes_sql_opens_it_in_a_console_with_a_hostile_name_as_one_argument() {
    let dir = tempfile::tempdir().unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_shop(dir.path(), feed);

    choose_on_table(&mut harness, HOSTILE, "Count in a console");
    run_it(&mut harness);

    step_until(&mut harness, "the program's console opening", |harness| {
        harness.query_by_role(Role::MultilineTextInput).is_some()
    });
    assert_eq!(
        editor_text(&harness),
        "SELECT count(*) AS n FROM \"main\".\"it's; DROP TABLE orders; --\";"
    );
    harness.run();
    assert!(
        harness.query_by_label_contains("rows").is_none(),
        "the SQL is opened, not run"
    );
}

#[test]
fn a_program_can_write_to_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let (layer, feed) = log_console::pipeline();
    // Thread-local, so it only captures events logged on this (the UI) thread, where the
    // output is logged.
    let _subscriber = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
    let mut harness = start_shop(dir.path(), feed);

    choose_on_table(&mut harness, "orders", "Log the table");
    run_it(&mut harness);

    step_until_visible(&mut harness, "Log the table: orders");
    harness.get_by_label_contains("Log the table: shop");
}

#[test]
fn a_failure_opens_a_window_with_what_went_wrong_whatever_the_output() {
    let dir = tempfile::tempdir().unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_shop(dir.path(), feed);

    choose_on_table(&mut harness, "orders", "Break");
    run_it(&mut harness);

    step_until_visible(&mut harness, "Break failed");
    harness.get_by_label("fail failed (exit status: 3)");
    harness.get_by_label("It wrote to stderr:");
    assert!(text_inputs(&harness).contains(&"it went wrong".to_owned()));
}

#[test]
fn a_runaway_command_is_stopped_by_stop_by_the_cancel_key_and_by_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_shop(dir.path(), feed);

    choose_on_table(&mut harness, "orders", "Hang");
    run_it(&mut harness);
    step_until_visible(&mut harness, "Running Hang…");
    harness.get_by_label("Stop").click();
    step_until_visible(&mut harness, "Hang stopped");

    choose_on_table(&mut harness, "orders", "Hang");
    step_until_visible(&mut harness, "Running Hang…");
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Period);
    step_until_visible(&mut harness, "Hang stopped");
    assert!(harness.query_by_label_contains("Running").is_none());

    choose_on_table(&mut harness, "orders", "Hang briefly");
    run_it(&mut harness);
    step_until_visible(&mut harness, "Hang briefly failed");
    harness.get_by_label("stopped after 200ms without finishing");
}

#[test]
fn view_actions_says_which_commands_are_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_shop(dir.path(), feed);
    choose_on_table(&mut harness, "orders", "Break");
    run_it(&mut harness);
    step_until_visible(&mut harness, "Break failed");

    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Custom actions…").click();
    harness.run();

    harness.get_by_label(r#"["fail"]"#);
    assert_eq!(
        harness
            .query_all_by_label("Trusted: runs without asking")
            .count(),
        1
    );
    assert_eq!(
        harness
            .query_all_by_label("Asks before it next runs")
            .count(),
        5
    );
}
