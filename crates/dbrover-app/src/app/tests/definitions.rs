//! An object's definition in a dock tab, opened from the explorer, through the whole app
//! against SQLite.

use super::*;

/// How many text editors, the definition's or a console's, hold `text`.
fn editors_showing(harness: &Harness<'_, DbRoverApp>, text: &str) -> usize {
    let holds = egui_kittest::kittest::By::new().predicate(move |node| {
        node.role() == Role::MultilineTextInput
            && node.value().is_some_and(|value| value.contains(text))
    });
    harness.query_all(holds).count()
}

fn show_definition(harness: &mut Harness<'_, DbRoverApp>) {
    harness.get_by_label("big_orders").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Show definition").click();
    harness.run();
}

#[test]
fn shows_a_definition_in_one_tab_and_opens_it_in_a_console() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(
        dir.path(),
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL);
         CREATE VIEW big_orders AS SELECT id FROM orders WHERE total > 100;",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    for expand in ["Expand shop", "Expand main", "Expand views"] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
    }
    step_until_visible(&mut harness, "big_orders");

    show_definition(&mut harness);
    step_until(&mut harness, "the definition loading", |harness| {
        editors_showing(harness, "CREATE VIEW big_orders") == 1
    });
    // Asked again, the same tab comes to the front rather than a second one.
    show_definition(&mut harness);
    let definitions = harness
        .state()
        .dock
        .iter_all_tabs()
        .filter(|(_, tab)| matches!(tab, Tab::Definition(_)))
        .count();
    assert_eq!(definitions, 1);

    harness.get_by_label("Open in console").click();
    step_until(&mut harness, "a console with the definition", |harness| {
        harness
            .state()
            .consoles
            .open_consoles()
            .any(|console| console.text().contains("CREATE VIEW big_orders"))
    });
}
