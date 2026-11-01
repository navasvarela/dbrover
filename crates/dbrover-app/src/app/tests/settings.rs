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

    harness.get_by_label("3 matches");
    harness.get_by_label("Keep statements for");
    harness.get_by_label("Keep per data source");
    harness.get_by_label("Keymap › Console");
    harness.get_by_label("Open the data source's query history");
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

/// The Keymap page lists every action's keys in a card per part of the app, with the custom
/// actions' ids.
#[test]
fn lists_the_keymap_as_a_page() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("actions.toml"),
        "[[action]]\nid = \"row-count\"\nname = \"Row count\"\napplies_to = [\"table\"]\n\
         sql = \"SELECT count(*) FROM {qualified_name}\"\n",
    )
    .unwrap();
    let mut harness = start_without_data_sources(dir.path());
    open_settings(&mut harness);

    harness
        .get_by_role_and_label(Role::Button, "Keymap")
        .click();
    harness.run();

    harness
        .get_by_label("The keys for each action. Change them in keymap.toml, then click Reload.");
    harness.get_by_label("Console");
    harness.get_by_label("Format the selection, or the statement at the cursor");
    harness.get_by_label("console.format");
    harness.get_by_label("Custom actions");
    harness.get_by_label("Row count");
    harness.get_by_label("custom.row-count");
}

/// View › Keymap… opens Settings on the Keymap page, and the status bar's link to the file's
/// problems does too; Create keymap.toml writes the file, keeping one that exists.
#[test]
fn opens_the_keymap_page_from_the_menu_and_the_status_bar() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_without_data_sources(dir.path());

    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Keymap…").click();
    harness.run();
    assert_eq!(settings_tabs(&harness), 1);
    harness.get_by_label("Create keymap.toml").click();
    let keymap = dir.path().join("keymap.toml");
    step_until(&mut harness, "keymap.toml to be written", |_| {
        keymap.exists()
    });
    assert!(
        std::fs::read_to_string(&keymap)
            .unwrap()
            .contains("# \"console.format\"")
    );

    std::fs::write(&keymap, "[bindings]\n\"console.walk\" = \"f6\"\n").unwrap();
    harness.get_by_label("Reload").click();
    step_until_visible(&mut harness, "keymap.toml: 1 problem");
    harness
        .get_by_role_and_label(Role::Button, "Appearance")
        .click();
    harness.run();
    harness.get_by_label("keymap.toml: 1 problem").click();
    harness.run();

    harness.get_by_label_contains("\"console.walk\" is not an action");
    harness.get_by_label("console.format");
}
