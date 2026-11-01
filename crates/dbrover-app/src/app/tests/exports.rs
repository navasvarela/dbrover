//! Formats of the user's own from `exports.toml` (W8), driven through the whole app against
//! SQLite with a temporary config folder.

use super::*;

const EXPORTS: &str = r#"
[[export]]
id = "semicolon"
name = "Semicolon CSV"
description = "For Excel in a language that separates lists with semicolons"
format = "csv"
separator = ";"
null_text = "NULL"
line_ending = "lf"

[[export]]
id = "orders-sheet"
name = "Orders workbook"
format = "xlsx"
sheet_name = "Orders"

[[export]]
id = "confluence"
name = "Confluence table"
extension = "txt"
header = "{{ data_source }}: {{ table }}\n||{% for column in columns %} {{ column.name }} ||{% endfor %}\n"
row = "|{% for value in row %} {{ value }} |{% endfor %}\n"
footer = "{{ count }} rows\n"

[[export]]
id = "typo"
name = "Broken template"
extension = "txt"
row = "{{ recrod.id }}\n"
"#;

/// The shop with two orders, `exports.toml` and, if given, `keymap.toml`, opened on the orders.
fn start_with_exports(dir: &Path, keymap: Option<&str>) -> Harness<'static, DbRoverApp> {
    save_shop(
        dir,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
         INSERT INTO orders VALUES (1, 'Zoë; Ltd'), (2, NULL);",
    );
    std::fs::write(dir.join("exports.toml"), EXPORTS).unwrap();
    if let Some(keymap) = keymap {
        std::fs::write(dir.join("keymap.toml"), keymap).unwrap();
    }
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir, feed);
    open_orders(&mut harness, "2 rows");
    step_until_visible(&mut harness, "Add row");
    harness
}

#[test]
fn view_exports_creates_the_file_once_and_lists_every_format_after_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "Expand shop");
    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Exports…").click();
    harness.run();
    harness.get_by_label_contains("No formats yet.");
    harness.get_by_label("0 formats");

    harness.get_by_label("Create exports.toml").click();
    let file = dir.path().join("exports.toml");
    // The file exists before its text is in, and writing over it then would mix the two.
    step_until(&mut harness, "exports.toml being written", |_| {
        std::fs::read_to_string(&file).is_ok_and(|text| text == crate::custom_exports::TEMPLATE)
    });
    assert!(harness.query_by_label_contains("exports.toml: ").is_none());

    std::fs::write(&file, EXPORTS).unwrap();
    harness.get_by_label("Reload").click();
    step_until_visible(&mut harness, "4 formats");
    harness.get_by_label("export.semicolon");
    harness.get_by_label("Preset of CSV");
    harness.get_by_label("separator = \";\"");
    harness.get_by_label("Preset of Excel");
    harness.get_by_label("files only");
    assert_eq!(harness.get_all_by_label("Template").count(), 2);
    harness.get_by_label("{{ recrod.id }}");

    harness.get_by_label("Create exports.toml").click();
    step_until(&mut harness, "the second create being refused", |harness| {
        harness
            .query_by_label_contains("could not create exports.toml")
            .is_some()
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), EXPORTS);
    harness.get_by_label("exports.toml: 1 problem");
}

/// A broken entry is skipped and listed; its link in the status bar opens the Exports page.
#[test]
fn lists_what_exports_toml_gets_wrong_on_its_page() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
    let broken = format!(
        "{EXPORTS}\n[[export]]\nid = \"tabs\"\nname = \"Tabs\"\nformat = \"tsv\"\nseparator = \"\\t\"\n"
    );
    std::fs::write(dir.path().join("exports.toml"), broken).unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    step_until_visible(&mut harness, "exports.toml: 1 problem");
    harness.get_by_label("exports.toml: 1 problem").click();
    step_until_visible(&mut harness, "4 formats");
    harness.get_by_label(
        "export \"tabs\": \"separator\" is not an option of TSV; its options are header, \
         null_text",
    );
}

/// A preset is a tile after the built-in formats, with its options shown as set; it writes the
/// file under its own name and is chosen again next time.
#[test]
fn exports_rows_as_a_preset_and_remembers_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_exports(dir.path(), None);
    let picked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (folder, files) = (dir.path().to_owned(), picked.clone());
    harness.state_mut().pick_export_path = Box::new(move |file, what| {
        files.lock().unwrap().push(file.clone());
        Some(folder.join(format!("{what}.{}", file.extension)))
    });

    open_export_dialog(&mut harness, "Loaded rows", Some("Semicolon CSV"));
    assert!(is_chosen(&harness, "Semicolon CSV"));
    assert!(!is_chosen(&harness, "CSV"));
    harness.get_by_label_contains("Set in exports.toml as “semicolon”");
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Exported 2 rows");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("rows.csv")).unwrap(),
        "id;customer\n1;\"Zoë; Ltd\"\n2;NULL\n"
    );
    assert_eq!(
        picked.lock().unwrap().as_slice(),
        [FileType {
            name: "Semicolon CSV".to_owned(),
            extension: "csv".to_owned(),
        }]
    );
    let remembered = &harness.state().export_choices;
    assert_eq!(
        remembered.custom.as_ref().map(|id| id.as_str()),
        Some("semicolon")
    );

    open_export_dialog(&mut harness, "Loaded rows", None);
    assert!(is_chosen(&harness, "Semicolon CSV"));
    harness.get_by_label("CSV").click();
    harness.run_steps(2);
    assert!(is_chosen(&harness, "CSV"));
    assert!(!is_chosen(&harness, "Semicolon CSV"));
}

/// Only presets that write text are under Copy as; copying uses the preset's options, and so
/// does the key `keymap.toml` gives it.
#[test]
fn copies_as_a_preset_from_the_cell_menu_and_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let keymap = "[bindings]\n\"export.semicolon\" = \"cmd+alt+c\"\n";
    let mut harness = start_with_exports(dir.path(), Some(keymap));

    harness.get_by_label("Zoë; Ltd").click_secondary();
    harness.run();
    harness.get_by_label("Copy as ⏵").click();
    harness.run();
    assert!(harness.query_by_label("Orders workbook").is_none());
    harness.get_by_label("Semicolon CSV").click();
    harness.step();
    assert_eq!(
        copied(&harness).as_deref(),
        Some("customer\n\"Zoë; Ltd\"\n")
    );
    harness.run();

    harness.get_by_label("Zoë; Ltd").click();
    harness.run();
    // The press alone, in a frame of its own: a frame for its release would replace the
    // output the copy is in.
    harness.event(egui::Event::Key {
        key: egui::Key::C,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND | egui::Modifiers::ALT,
    });
    harness.step();
    assert_eq!(
        copied(&harness).as_deref(),
        Some("customer\n\"Zoë; Ltd\"\n")
    );
}

/// A template is a tile like any format: it writes its file a part at a time, knowing the data
/// source and the table, and a mistake in it shows in the preview rather than in a file.
#[test]
fn exports_rows_through_a_template_and_previews_its_mistakes() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_exports(dir.path(), None);
    let folder = dir.path().to_owned();
    harness.state_mut().pick_export_path =
        Box::new(move |file, what| Some(folder.join(format!("{what}.{}", file.extension))));

    open_export_dialog(&mut harness, "Loaded rows", Some("Broken template"));
    harness.get_by_label_contains("row 1: the row template, line 1: undefined value");
    harness.get_by_label("Confluence table").click();
    harness.run_steps(2);
    harness.get_by_label_contains("Written by the template “confluence”");
    confirm_export(&mut harness);
    step_until_visible(&mut harness, "Exported 2 rows");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("rows.txt")).unwrap(),
        "shop: main.orders\n|| id || customer ||\n| 1 | Zoë; Ltd |\n| 2 |  |\n2 rows\n"
    );
}

/// Templates are under Copy as, writing the selected cells; one that stops says why where an
/// export's problem shows.
#[test]
fn copies_as_a_template() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_exports(dir.path(), None);
    let copy_as = |harness: &mut Harness<'_, DbRoverApp>, format: &str| {
        harness.get_by_label("Zoë; Ltd").click_secondary();
        harness.run();
        harness.get_by_label("Copy as ⏵").click();
        harness.run();
        harness.get_by_label(format).click();
        harness.step();
        copied(harness)
    };
    assert_eq!(
        copy_as(&mut harness, "Confluence table").as_deref(),
        Some("shop: main.orders\n|| customer ||\n| Zoë; Ltd |\n1 rows\n")
    );
    harness.run();
    assert_eq!(copy_as(&mut harness, "Broken template"), None);
    step_until_visible(
        &mut harness,
        "row 1: the row template, line 1: undefined value",
    );
}
