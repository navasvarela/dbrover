//! `actions.toml`, `snippets.toml` and `theme.toml` (H1), driven through the whole app against
//! SQLite with a temporary config folder.

use super::*;

#[test]
fn counts_each_files_problems_and_lists_them_until_a_reload_finds_none() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let actions = dir.path().join("actions.toml");
    std::fs::write(
        &actions,
        "[[action]]\nid = \"row-count\"\nname = \"Row count\"\napplies_to = [\"tabel\"]\n\
         sql = \"SELECT count(*) FROM {qualified_name}\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("theme.toml"),
        "[dark]\nprimary = \"#9a8bff\"\nbackground = \"#000000\"\nbase = \"dark\"\n",
    )
    .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "actions.toml: 1 problem");
    // The engine reports each file on its own, so on a slow machine the theme's problems can
    // arrive a frame after the actions'.
    step_until_visible(&mut harness, "theme.toml: 2 problems");
    assert!(harness.query_by_label_contains("snippets.toml").is_none());

    harness.get_by_label("actions.toml: 1 problem").click();
    step_until_visible(&mut harness, "0 actions");
    harness.get_by_label_contains("\"tabel\" is not something actions apply to");

    std::fs::write(
        &actions,
        std::fs::read_to_string(&actions)
            .unwrap()
            .replace("tabel", "table"),
    )
    .unwrap();
    harness.get_by_label("Reload").click();
    step_until_visible(&mut harness, "1 action");
    assert!(
        harness
            .query_by_label_contains("is not something actions apply to")
            .is_none()
    );
    assert!(harness.query_by_label("actions.toml: 1 problem").is_none());
    harness.get_by_label("theme.toml: 2 problems");
}

#[test]
fn completion_offers_the_drivers_snippets_and_tab_walks_their_fields() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    std::fs::write(
        dir.path().join("snippets.toml"),
        r#"
        [[snippet]]
        trigger = "sel"
        description = "Select with a filter"
        body = "SELECT ${1:*}\nFROM ${2:orders}\nWHERE ${3:id = 1};"

        [[snippet]]
        trigger = "selpg"
        body = "SELECT pg_backend_pid();"
        drivers = ["postgres"]
        "#,
    )
    .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    open_console(&mut harness);
    let editor = || Role::MultilineTextInput;

    harness.get_by_role(editor()).type_text("se");
    step_until_visible(&mut harness, "Select with a filter");
    assert!(
        harness.query_by_label("selpg").is_none(),
        "a PostgreSQL snippet isn't offered on SQLite"
    );
    harness.get_by_role(editor()).type_text("l");
    step_until(
        &mut harness,
        "the list narrowing to select and sel",
        |harness| harness.query_by_label("sequence").is_none(),
    );
    // The keyword comes first, being a name that starts with the word.
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert_eq!(
        editor_text(&harness),
        "SELECT *\nFROM orders\nWHERE id = 1;"
    );

    harness.get_by_role(editor()).type_text("count(*)");
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    harness.get_by_role(editor()).type_text("invoices");
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    harness.run_steps(2);
    harness.get_by_role(editor()).type_text("bills");
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    harness.get_by_role(editor()).type_text(" -- done");
    harness.run_steps(2);

    assert_eq!(
        editor_text(&harness),
        "SELECT count(*)\nFROM bills\nWHERE id = 1; -- done"
    );
}

#[test]
fn theme_toml_recolours_each_theme_and_lists_what_it_makes_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join("theme.toml");
    std::fs::write(
        &theme,
        "[light]\nprimary = \"#5b3fe0\"\n\n[dark]\ntext_weak = \"#444444\"\n",
    )
    .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let focus = |harness: &Harness<'_, DbRoverApp>, theme| {
        harness.ctx.style_of(theme).visuals.selection.stroke.color
    };
    step_until(&mut harness, "theme.toml's primary colour", |harness| {
        focus(harness, egui::Theme::Light) == egui::Color32::from_rgb(0x5b, 0x3f, 0xe0)
    });
    assert_eq!(
        harness
            .ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .weak_text_color,
        Some(egui::Color32::from_rgb(0x44, 0x44, 0x44)),
        "an unreadable colour still applies"
    );

    harness.get_by_label_contains("theme.toml: ").click();
    step_until(&mut harness, "the theme's problems listed", |harness| {
        harness
            .query_by_label_contains("dark: weak text on panel is ")
            .is_some()
    });
    assert!(
        harness
            .query_all_by_label_contains("below the 4.5:1 that WCAG AA asks for")
            .count()
            > 1
    );
    harness.get_by_label("1 colour for the light theme, 1 colour for the dark one");

    std::fs::write(&theme, "[light]\nprimary = \"#0a6a5e\"\n").unwrap();
    harness.get_by_label("Reload").click();
    step_until_visible(&mut harness, "No problems.");
    assert_eq!(
        focus(&harness, egui::Theme::Light),
        egui::Color32::from_rgb(0x0a, 0x6a, 0x5e)
    );
    assert_eq!(
        harness
            .ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .weak_text_color,
        Some(theme::Palette::DARK.text_weak)
    );
}
