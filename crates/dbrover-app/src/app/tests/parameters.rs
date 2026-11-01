//! `:name` parameters (W6d), driven through the whole app against SQLite.

use super::*;

/// A parameter's value field, by its label, `:name`.
fn parameter_field<'a>(
    harness: &'a Harness<'_, DbRoverApp>,
    label: &'a str,
) -> egui_kittest::Node<'a> {
    harness.get_by_role_and_label(Role::TextInput, label)
}

/// Clicks the toolbar's Run and waits for the parameters dialog to be placed.
fn run_and_wait_for_dialog(harness: &mut Harness<'_, DbRoverApp>) {
    harness.get_by_label("Run").click();
    step_until_visible(harness, "Parameter values");
    // Two frames for the modal to be placed; see the rollback test.
    harness.run_steps(2);
}

#[test]
fn asks_for_parameter_values_and_remembers_them_in_the_console() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY);
         INSERT INTO orders VALUES (1), (2), (3);",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_console(&mut harness);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("SELECT id FROM orders WHERE id >= :min AND id < :min + 1");
    harness.run();

    run_and_wait_for_dialog(&mut harness);
    parameter_field(&harness, ":min").type_text("2");
    harness.run();
    harness.key_press(egui::Key::Enter);
    step_until_visible(&mut harness, "1 row");
    assert!(harness.query_by_label("Parameter values").is_none());
    step_until_runs_finish(&mut harness);

    run_and_wait_for_dialog(&mut harness);
    assert_eq!(
        parameter_field(&harness, ":min").value().as_deref(),
        Some("2"),
        "the console remembers the value"
    );
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(harness.query_by_label("Parameter values").is_none());
}
