//! Getting data sources into projects from the explorer (D9), driven through the whole app with
//! a temporary config folder: created straight into a project from its header's menu.

use super::*;

/// A project with nothing in it, so the explorer shows its header and no data sources.
fn save_project(dir: &Path, name: &str) -> dbrover_store::Project {
    let project = dbrover_store::Project::new(
        dbrover_core::ProjectId::generate(),
        name.to_owned(),
        dbrover_store::ProjectSlug::from_name(name),
    );
    ProjectFile::new(dir.join("projects.toml"))
        .save(&dbrover_store::Projects {
            projects: vec![project.clone()],
            ..dbrover_store::Projects::default()
        })
        .unwrap();
    project
}

/// **New data source ›** on a project header's menu saves the data source in that project, with
/// no move afterwards. Before D9 the same engine picked from the toolbar's `+` landed in
/// **No project**.
#[test]
fn creates_a_data_source_in_the_project_whose_menu_asked_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let project = save_project(dir.path(), "Analytics");
    let database = dir.path().join("chinook.db");
    rusqlite::Connection::open(&database).unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Analytics");

    // The item names its group, because the toolbar's `+` and the Welcome tab's card are both
    // called "New data source" as well.
    harness.get_by_label("Analytics").click_secondary();
    harness.run();
    harness.get_by_label("New data source in Analytics").hover();
    step_until_visible(&mut harness, "SQLite");
    harness.get_by_label("SQLite").click();
    harness.run();
    let file_field = harness.get_by_role_and_label(Role::TextInput, "File");
    file_field.focus();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "File")
        .type_text(&database.display().to_string());
    harness.run();
    harness.get_by_label("Save").click();

    step_until_visible(&mut harness, "Expand chinook.db");
    let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
        .load()
        .unwrap();
    let [saved] = saved.as_slice() else {
        panic!("expected one data source, got {saved:?}");
    };
    assert_eq!(
        saved.project,
        Some(project.id),
        "it was created in the project, not moved into it"
    );
}

/// **No project**'s header offers the same item, where it means what the toolbar's `+` means.
/// Its header only shows once something is in it.
#[test]
fn offers_a_new_data_source_on_the_no_project_header() {
    let dir = tempfile::tempdir().unwrap();
    save_project(dir.path(), "Analytics");
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY);");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "No project");

    harness.get_by_label("No project").click_secondary();
    harness.run();
    harness.get_by_label("New data source in No project");
}
