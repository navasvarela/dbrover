//! View › Settings… as a tab in the dock: opening it, its categories and search, and the
//! settings it saves.

use egui::ThemePreference;
use egui_kittest::kittest::NodeT;

use super::*;

fn start_without_data_sources(dir: &Path) -> Harness<'static, DbRoverApp> {
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir, feed);
    step_until_visible(&mut harness, "Engine running");
    harness
}

fn open_settings(harness: &mut Harness<'_, DbRoverApp>) {
    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label_contains("Settings…").click();
    harness.run();
}

fn settings_tabs(harness: &Harness<'_, DbRoverApp>) -> usize {
    harness
        .state()
        .dock
        .iter_all_tabs()
        .filter(|(_, tab)| **tab == Tab::Settings)
        .count()
}

fn search(harness: &mut Harness<'_, DbRoverApp>, query: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, "Search settings")
        .focus();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Search settings")
        .type_text(query);
    harness.run();
}

/// Settings opens once, beside the consoles, and comes back to the front when asked again.
#[test]
fn opens_settings_as_a_tab_beside_the_consoles() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_without_data_sources(dir.path());

    open_settings(&mut harness);
    assert_eq!(settings_tabs(&harness), 1);
    harness.get_by_role_and_label(Role::Button, "Appearance");
    // egui_dock's tab buttons have no accessible names to click them by.
    let dock = &mut harness.state_mut().dock;
    let welcome = dock.find_tab(&Tab::Welcome).expect("the Welcome tab");
    dock.set_active_tab(welcome).expect("Welcome's area");
    harness.run();
    open_settings(&mut harness);

    assert_eq!(settings_tabs(&harness), 1, "the same tab, not a second one");
    let welcome_area = harness
        .state()
        .dock
        .find_tab(&Tab::Welcome)
        .map(|path| path.node_path());
    let settings_area = harness
        .state()
        .dock
        .find_tab(&Tab::Settings)
        .map(|path| path.node_path());
    assert_eq!(settings_area, welcome_area, "in the consoles' area");
    assert_eq!(
        harness
            .state_mut()
            .dock
            .find_active_focused()
            .map(|(_, tab)| tab.clone()),
        Some(Tab::Settings)
    );
}

/// A search lists the settings it finds from every category, counted in the sidebar; a
/// category clicked clears it.
#[test]
fn searches_every_category() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_without_data_sources(dir.path());
    open_settings(&mut harness);

    search(&mut harness, "history");

    harness.get_by_label("2 settings");
    harness.get_by_label("Keep statements for");
    harness.get_by_label("Keep per data source");
    assert!(
        harness.query_by_label("Theme").is_none(),
        "Appearance's isn't found"
    );

    harness.get_by_label("Editor").click();
    harness.run();
    harness.get_by_label("Keywords");
    assert!(harness.query_by_label("Keep statements for").is_none());
}

/// The theme chosen in Settings is the one the View menu shows, and a switch saved at once.
#[test]
fn changes_the_theme_and_saves_a_switch() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_without_data_sources(dir.path());
    open_settings(&mut harness);

    harness.get_by_label("Dark").click();
    harness.run();
    assert_eq!(
        harness.ctx.options(|options| options.theme_preference),
        ThemePreference::Dark
    );

    harness.get_by_label("Updates").click();
    harness.run();
    harness
        .get_by_role_and_label(
            Role::CheckBox,
            "Check for a new release when DbRover starts",
        )
        .click();
    step_until(&mut harness, "the settings file", |_| {
        std::fs::read_to_string(dir.path().join("settings.toml"))
            .is_ok_and(|text| text.contains("check_on_start = false"))
    });
    assert!(
        !harness
            .get_by_role_and_label(
                Role::CheckBox,
                "Check for a new release when DbRover starts"
            )
            .accesskit_node()
            .toggled()
            .is_some_and(|toggled| toggled == egui::accesskit::Toggled::True)
    );
}

/// Keymap… in the sidebar opens the keymap's window.
#[test]
fn opens_the_keymap_from_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_without_data_sources(dir.path());
    open_settings(&mut harness);

    harness.get_by_label("Keymap…").click();
    harness.run();

    assert!(harness.state().keymap_window_open);
}
