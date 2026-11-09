//! Changes a `git pull` makes to the settings folder while the app runs (VC1b): taken in, never
//! undone by the next save.

use std::fs;

use dbrover_core::DataSource;

use super::*;

fn data_sources(dir: &Path) -> DataSourceFile {
    DataSourceFile::new(dir.join("data_sources.toml"))
}

fn named(data_source: &DataSource, name: &str) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        ..data_source.clone()
    }
}

fn names_saved(dir: &Path) -> Vec<String> {
    data_sources(dir)
        .load()
        .unwrap()
        .into_iter()
        .map(|data_source| data_source.name)
        .collect()
}

fn rename(harness: &mut Harness<'_, DbRoverApp>, from: &str, to: &str) {
    harness.get_by_label(from).click_secondary();
    harness.run();
    harness.get_by_label("Edit…").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Name")
        .focus();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Name")
        .type_text(to);
    harness.run();
    harness.get_by_label("Save").click();
}

#[test]
fn a_pull_while_the_app_runs_is_shown_and_kept_by_the_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let warehouse = named(&shop, "warehouse");
    data_sources(dir.path())
        .save(&[shop.clone(), warehouse.clone()])
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Expand warehouse");

    // Pulled while the window is in front: the next save finds it.
    let crm = named(&shop, "crm");
    data_sources(dir.path())
        .save(&[shop.clone(), warehouse.clone(), crm.clone()])
        .unwrap();
    rename(&mut harness, "warehouse", "stock");
    step_until_visible(&mut harness, "Expand crm");
    step_until_visible(&mut harness, "Expand stock");
    assert_eq!(names_saved(dir.path()), ["shop", "stock", "crm"]);

    // Pulled from a terminal: the window coming back to the front finds it.
    let mut pulled = data_sources(dir.path()).load().unwrap();
    pulled[0].name = "shop eu".to_owned();
    pulled.push(named(&shop, "billing"));
    data_sources(dir.path()).save(&pulled).unwrap();
    switch_away_and_back(&mut harness);
    step_until_visible(&mut harness, "Expand shop eu");
    step_until_visible(&mut harness, "Expand billing");

    rename(&mut harness, "crm", "sales");
    step_until(&mut harness, "the rename being saved", |_| {
        names_saved(dir.path()).contains(&"sales".to_owned())
    });
    assert_eq!(
        names_saved(dir.path()),
        ["shop eu", "stock", "sales", "billing"]
    );
    assert!(
        fs::read_to_string(dir.path().join("data_sources.toml"))
            .unwrap()
            .contains("picks up changes when its window comes back to the front")
    );
}
