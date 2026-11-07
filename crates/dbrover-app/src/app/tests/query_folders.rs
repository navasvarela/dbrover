//! Query folders (VC2), driven through the whole app on a temporary clone of a repository.

use std::fs;
use std::path::PathBuf;

use egui::ViewportId;

use super::*;

/// A clone with queries in `queries/`, a scratch folder its `.gitignore` leaves out, and
/// Git's own folder, which holds a `.sql` file only to show it isn't entered.
fn clone_of_shop(dir: &Path) -> PathBuf {
    let clone = dir.join("shop-repo");
    for (file, text) in [
        ("queries/sales.sql", "select sum(total) from orders;"),
        ("queries/admin/users.sql", "select * from users;"),
        ("queries/scratch/try.sql", "select 1;"),
        ("queries/notes.md", "# Notes"),
        (".git/hooks/check.sql", "select 1;"),
        (".gitignore", "scratch/\n"),
    ] {
        let path = clone.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    clone
}

/// Starts the app with one `shop` data source, **Add query folder…** answering `folder`.
fn start_with_folder(dir: &Path, folder: &Path) -> Harness<'static, DbRoverApp> {
    save_shop(
        dir,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL)",
    );
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir, feed);
    let picked = folder.to_owned();
    harness.state_mut().pick_query_folder = Box::new(move || Some(picked.clone()));
    step_until_visible(&mut harness, "Expand shop");
    harness
}

fn add_from_the_menu(harness: &mut Harness<'_, DbRoverApp>) {
    harness.get_by_label("File").click();
    harness.run();
    harness.get_by_label_contains("Add query folder…").click();
    harness.run();
}

fn shows(harness: &Harness<'_, DbRoverApp>, label: &str) -> bool {
    harness.query_by_label(label).is_some()
}

fn open_folder(harness: &mut Harness<'_, DbRoverApp>, name: &str) {
    step_until_visible(harness, name);
    harness.get_by_label(name).click();
    harness.run();
}

/// What `git checkout` in a terminal looks like to the app: the window loses the focus, and
/// gets it back.
fn switch_away_and_back(harness: &mut Harness<'_, DbRoverApp>) {
    for focused in [false, true] {
        harness
            .input_mut()
            .viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .focused = Some(focused);
        harness.step();
    }
}

fn settings_text(dir: &Path) -> String {
    fs::read_to_string(dir.join("settings.toml")).unwrap_or_default()
}

#[test]
fn lists_a_clones_sql_files_and_reads_them_again_when_the_window_is_in_front() {
    let dir = tempfile::tempdir().unwrap();
    let clone = clone_of_shop(dir.path());
    let mut harness = start_with_folder(dir.path(), &clone);
    assert!(shows(&harness, "Query folders"));

    add_from_the_menu(&mut harness);
    open_folder(&mut harness, "queries");
    open_folder(&mut harness, "admin");
    step_until_visible(&mut harness, "users.sql");

    assert!(shows(&harness, "shop-repo"));
    assert!(shows(&harness, "sales.sql"));
    for hidden in [
        ".git",
        "hooks",
        "check.sql",
        "scratch",
        "try.sql",
        "notes.md",
    ] {
        assert!(!shows(&harness, hidden), "{hidden} is listed");
    }
    step_until(&mut harness, "the folder being saved", |_| {
        settings_text(dir.path()).contains("shop-repo")
    });

    fs::write(clone.join("queries").join("refunds.sql"), "select 1;").unwrap();
    switch_away_and_back(&mut harness);
    step_until_visible(&mut harness, "refunds.sql");
}

#[test]
fn removing_a_folder_from_the_list_leaves_its_files_alone() {
    let dir = tempfile::tempdir().unwrap();
    let clone = clone_of_shop(dir.path());
    let mut harness = start_with_folder(dir.path(), &clone);
    add_from_the_menu(&mut harness);
    step_until_visible(&mut harness, "queries");

    harness.get_by_label("shop-repo").click_secondary();
    harness.run();
    harness.get_by_label("Remove from list").click();
    harness.run();

    assert!(!shows(&harness, "shop-repo"));
    step_until(&mut harness, "the list being saved", |_| {
        !settings_text(dir.path()).contains("shop-repo")
    });
    assert!(clone.join("queries").join("sales.sql").exists());
}
