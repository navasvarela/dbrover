//! Tests of custom formats: their ids, and presets' options.

use dbrover_core::Cell;
use serde_json::json;

use super::*;

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

fn preset(id: &str, preset: Preset) -> CustomFormat {
    CustomFormat {
        id: CustomFormatId::new(id).unwrap(),
        name: id.to_owned(),
        description: None,
        extension: preset.format.extension().to_owned(),
        kind: CustomFormatKind::Preset(preset),
    }
}

#[test]
fn ids_read_the_same_in_toml_menus_and_the_keymap() {
    for id in ["excel-semicolon", "pdf_report", "2025"] {
        let parsed = CustomFormatId::new(id).unwrap();
        assert_eq!(parsed.as_str(), id);
        assert_eq!(parsed.keymap_id(), format!("export.{id}"));
    }
    for id in ["", "Excel", "-csv", "a b", "a.b", "é"] {
        assert_eq!(CustomFormatId::new(id), None, "{id:?}");
    }
}

#[test]
fn names_each_format_s_options_as_app_ron_saves_them() {
    let cases: [(ExportFormat, &[&str]); 10] = [
        (
            ExportFormat::Csv,
            &[
                "separator",
                "quoting",
                "header",
                "null_text",
                "line_ending",
                "bom",
            ],
        ),
        (
            ExportFormat::Xlsx,
            &["header", "sheet_name", "freeze_header", "autofilter"],
        ),
        (ExportFormat::Tsv, &["header", "null_text"]),
        (ExportFormat::Markdown, &["null_text"]),
        (ExportFormat::SqlInsert, &["rows_per_statement"]),
        (ExportFormat::Json, &[]),
        (ExportFormat::JsonLines, &[]),
        (ExportFormat::Parquet, &["compression"]),
        (ExportFormat::Html, &["document", "null_text"]),
        (ExportFormat::SqlUpdate, &[]),
    ];
    for (format, names) in cases {
        assert_eq!(Preset::option_names(format), names, "{format:?}");
    }
}

#[test]
fn a_preset_writes_its_format_with_its_options() {
    let semicolon = Preset::read(
        ExportFormat::Csv,
        json!({ "separator": ";", "bom": true, "null_text": "NULL" }),
    )
    .unwrap();
    assert_eq!(
        semicolon.options.csv,
        CsvOptions {
            separator: FieldSeparator::SEMICOLON,
            bom: true,
            null_text: "NULL".to_owned(),
            ..CsvOptions::default()
        }
    );
    assert_eq!(semicolon.options.xlsx, XlsxOptions::default());
    assert_eq!(
        semicolon.changed_options(),
        [
            ("separator".to_owned(), "\";\"".to_owned()),
            ("null_text".to_owned(), "\"NULL\"".to_owned()),
            ("bom".to_owned(), "true".to_owned()),
        ]
    );

    let format = preset("excel-semicolon", semicolon);
    assert!(format.writes_text());
    assert_eq!(format.builtin(), ExportFormat::Csv);
    assert_eq!(format.summary(), "CSV with options from exports.toml");
    let spec = format
        .spec(&RowSource::untabled(dbrover_sql::Dialect::Sqlite))
        .unwrap();
    let columns = ["id", "note"].map(ExportColumn::text).to_vec();
    let rows = vec![vec![text("1"), text("a;b")], vec![text("2"), None]];
    assert_eq!(
        render(&spec, &columns, &rows).unwrap(),
        "\u{feff}id;note\r\n1;\"a;b\"\r\n2;NULL\r\n"
    );
}

#[test]
fn a_preset_of_a_file_format_writes_no_text() {
    let excel = Preset::read(ExportFormat::Xlsx, json!({ "sheet_name": "Orders" })).unwrap();
    assert_eq!(excel.options.xlsx.sheet_name.as_str(), "Orders");
    assert!(!preset("orders", excel).writes_text());
}

#[test]
fn says_what_a_preset_s_options_get_wrong() {
    let cases = [
        (
            ExportFormat::Csv,
            json!({ "separator": "\"" }),
            "can't separate fields",
        ),
        (
            ExportFormat::Csv,
            json!({ "header": "yes" }),
            "expected a boolean",
        ),
        (
            ExportFormat::Csv,
            json!({ "quoting": "never" }),
            "unknown variant",
        ),
        (ExportFormat::Xlsx, json!({ "sheet_name": "a/b" }), "/"),
    ];
    for (format, options, problem) in cases {
        let error = Preset::read(format, options).unwrap_err().to_string();
        assert!(error.contains(problem), "{format:?}: {error}");
    }
}

#[test]
fn a_preset_of_a_table_format_needs_the_table() {
    let update = preset(
        "updates",
        Preset::read(ExportFormat::SqlUpdate, json!({})).unwrap(),
    );
    assert!(
        update
            .spec(&RowSource::untabled(dbrover_sql::Dialect::Sqlite))
            .is_err()
    );
}

#[test]
fn reads_formats_by_their_ids() {
    for format in ExportFormat::ALL {
        assert_eq!(ExportFormat::from_id(format.id()), Some(format));
    }
    assert_eq!(ExportFormat::from_id("excel"), None);
}
