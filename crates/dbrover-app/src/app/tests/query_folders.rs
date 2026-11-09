//! Query folders (VC2), driven through the whole app on a temporary clone of a repository.

use std::fs;
use std::path::PathBuf;

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

/// Saves `shop`, and a second SQLite data source named `crm`, so opening a file has a data
/// source to ask about.
fn save_shop_and_crm(dir: &Path) -> (dbrover_core::DataSource, dbrover_core::DataSource) {
    let shop = save_shop(dir, "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let database = dir.join("crm.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("CREATE TABLE customers (id INTEGER PRIMARY KEY)")
        .unwrap();
    let crm = dbrover_core::DataSource {
        id: DataSourceId::generate(),
        name: "crm".to_owned(),
        config: dbrover_core::ConnectionConfig::Sqlite(dbrover_core::SqliteConfig {
            path: database,
            read_only: false,
        }),
        ..shop.clone()
    };
    DataSourceFile::new(dir.join("data_sources.toml"))
        .save(&[shop.clone(), crm.clone()])
        .unwrap();
    (shop, crm)
}

/// The console tabs showing files, and the data source each runs on.
fn open_files(harness: &Harness<'_, DbRoverApp>) -> Vec<(PathBuf, DataSourceId)> {
    harness
        .state()
        .consoles
        .files()
        .map(|(key, path)| (path.to_owned(), key.data_source))
        .collect()
}

#[test]
fn opens_a_file_from_the_tree_saves_it_in_place_and_follows_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let clone = clone_of_shop(dir.path());
    let sales = clone.join("queries").join("sales.sql");
    let mut harness = start_with_folder(dir.path(), &clone);
    add_from_the_menu(&mut harness);
    open_folder(&mut harness, "queries");

    harness.get_by_label("sales.sql").click();
    step_until(&mut harness, "the file opening", |harness| {
        open_files(harness).len() == 1
    });
    harness.run();
    assert_eq!(editor_text(&harness), "select sum(total) from orders;");
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run();
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text(" -- by month");
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    step_until(&mut harness, "the file being saved in place", |_| {
        fs::read_to_string(&sales).unwrap().contains("-- by month")
    });

    // What `git checkout` of a branch that changed the file does to it.
    fs::write(&sales, "select sum(total), count(*) from orders;").unwrap();
    switch_away_and_back(&mut harness);
    step_until(
        &mut harness,
        "the console showing the file's new text",
        |harness| editor_text(harness) == "select sum(total), count(*) from orders;",
    );
}

#[test]
fn new_query_here_creates_an_empty_file_on_the_last_focused_consoles_data_source() {
    let dir = tempfile::tempdir().unwrap();
    let clone = clone_of_shop(dir.path());
    let (_, crm) = save_shop_and_crm(dir.path());
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    let picked = clone.clone();
    harness.state_mut().pick_query_folder = Box::new(move || Some(picked.clone()));
    step_until_visible(&mut harness, "Expand crm");
    harness.get_by_label("crm").click_secondary();
    harness.run();
    harness
        .get_all_by_label("New query console")
        .next()
        .unwrap()
        .click();
    step_until(&mut harness, "the crm console opening", |harness| {
        harness.query_by_role(Role::MultilineTextInput).is_some()
    });
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run();

    add_from_the_menu(&mut harness);
    step_until_visible(&mut harness, "queries");
    harness.get_by_label("queries").click_secondary();
    harness.run();
    harness.get_by_label("New query here…").click();
    step_until_visible(&mut harness, "New query");
    // Two frames for the modal to be placed; see the rollback test.
    harness.run_steps(2);
    harness.get_by_label("File name").type_text("refunds");
    harness.run();
    harness.get_by_label("Create").click();

    let refunds = clone.join("queries").join("refunds.sql");
    step_until(&mut harness, "the new file opening", |harness| {
        open_files(harness) == [(refunds.clone(), crm.id)]
    });
    assert_eq!(fs::read_to_string(&refunds).unwrap(), "");
    step_until_visible(&mut harness, "refunds.sql");
}

#[test]
fn a_folder_missing_on_this_machine_stays_listed_dimmed() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let gone = dir.path().join("elsewhere").join("queries");
    fs::write(
        dir.path().join("settings.toml"),
        format!(
            "[explorer]\nquery_folders = [{:?}]\n",
            gone.display().to_string()
        ),
    )
    .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);

    open_folder(&mut harness, "queries");
    step_until_visible(&mut harness, "Not on this machine");
    assert!(shows(&harness, "queries"));
}
