//! Golden tests of every text format on awkward values, and of the file writer.

use dbrover_core::{Cell, ValueKind};

use super::*;

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

/// A repeated column name, and values each format has to take care with: separators, quotes,
/// line breaks of both kinds, `|`, markup, an empty string, `NULL` and emoji.
fn sample() -> (Vec<ExportColumn>, Vec<Vec<Cell>>) {
    let columns = ["id", "note", "id"].map(ExportColumn::text).to_vec();
    let rows = vec![
        vec![text("1"), text("plain"), text("7")],
        vec![text("2"), text("a, \"quoted\"\nline"), None],
        vec![text("3"), text("x | <b>&</b>\ttab;"), text("")],
        vec![text("4"), text("🦀\r\ncrab"), None],
    ];
    (columns, rows)
}

/// Rows of no known table, from SQLite.
fn source() -> RowSource {
    RowSource::untabled(dbrover_sql::Dialect::Sqlite)
}

/// `format` with its defaults, for rows from [`source`].
fn spec(format: ExportFormat) -> ExportSpec {
    format.default_spec(&source()).unwrap()
}

fn rendered(spec: &ExportSpec) -> String {
    let (columns, rows) = sample();
    render(spec, &columns, &rows).unwrap()
}

#[test]
fn writes_rfc_4180_csv_by_default() {
    assert_eq!(
        rendered(&spec(ExportFormat::Csv)),
        "id,note,id\r\n\
         1,plain,7\r\n\
         2,\"a, \"\"quoted\"\"\nline\",\r\n\
         3,x | <b>&</b>\ttab;,\r\n\
         4,\"🦀\r\ncrab\",\r\n"
    );
}

#[test]
fn writes_csv_with_each_option_changed() {
    let options = CsvOptions {
        separator: FieldSeparator::SEMICOLON,
        quoting: Quoting::Always,
        header: false,
        null_text: "\\N".to_owned(),
        line_ending: LineEnding::Lf,
        bom: true,
    };
    assert_eq!(
        rendered(&ExportSpec::Csv(options)),
        "\u{feff}\"1\";\"plain\";\"7\"\n\
         \"2\";\"a, \"\"quoted\"\"\nline\";\\N\n\
         \"3\";\"x | <b>&</b>\ttab;\";\"\"\n\
         \"4\";\"🦀\r\ncrab\";\\N\n"
    );
}

#[test]
fn quotes_csv_null_text_only_where_it_needs_it() {
    let options = CsvOptions {
        separator: FieldSeparator::PIPE,
        null_text: "a|b".to_owned(),
        ..CsvOptions::default()
    };
    let columns = [ExportColumn::text("x")];
    assert_eq!(
        render(&ExportSpec::Csv(options), &columns, &[vec![None]]).unwrap(),
        "x\r\n\"a|b\"\r\n"
    );
}

#[test]
fn csv_for_excel_starts_with_a_byte_order_mark() {
    let excel = export_choices(&source())
        .into_iter()
        .find(|choice| choice.name == "CSV for Excel")
        .and_then(|choice| choice.spec.ok());
    let Some(excel @ ExportSpec::Csv(_)) = excel else {
        panic!("no CSV for Excel among {:?}", export_choices(&source()));
    };
    let csv = rendered(&excel);
    assert_eq!(
        csv.strip_prefix('\u{feff}'),
        Some(rendered(&spec(ExportFormat::Csv)).as_str())
    );
}

#[test]
fn refuses_a_quote_or_a_line_break_as_a_separator() {
    for refused in ['"', '\n', '\r'] {
        assert!(FieldSeparator::try_from(refused).is_err(), "{refused:?}");
    }
    assert_eq!(FieldSeparator::try_from('\t'), Ok(FieldSeparator::TAB));
}

#[test]
fn writes_tsv_as_the_clipboard_takes_it() {
    assert_eq!(
        rendered(&spec(ExportFormat::Tsv)),
        "id\tnote\tid\n\
         1\tplain\t7\n\
         2\t\"a, \"\"quoted\"\"\nline\"\tNULL\n\
         3\t\"x | <b>&</b>\ttab;\"\t\n\
         4\t\"🦀\r\ncrab\"\tNULL\n"
    );
}

#[test]
fn writes_a_markdown_table() {
    assert_eq!(
        rendered(&spec(ExportFormat::Markdown)),
        "| id | note | id |\n\
         | --- | --- | --- |\n\
         | 1 | plain | 7 |\n\
         | 2 | a, \"quoted\"<br>line | NULL |\n\
         | 3 | x \\| <b>&</b>\ttab; |  |\n\
         | 4 | 🦀<br>crab | NULL |\n"
    );
    assert_eq!(
        render(&spec(ExportFormat::Markdown), &[], &[vec![]]).unwrap(),
        ""
    );
}

#[test]
fn writes_json_objects_with_keys_in_column_order() {
    let json = rendered(&ExportSpec::Json);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!([
            {"id": "1", "note": "plain", "id_2": "7"},
            {"id": "2", "note": "a, \"quoted\"\nline", "id_2": null},
            {"id": "3", "note": "x | <b>&</b>\ttab;", "id_2": ""},
            {"id": "4", "note": "🦀\r\ncrab", "id_2": null},
        ])
    );
    assert!(json.find("\"note\"").unwrap() < json.find("\"id_2\"").unwrap());
    assert_eq!(
        json,
        serde_json::to_string_pretty(&parsed).unwrap() + "\n",
        "laid out as serde_json lays out the whole array"
    );
    assert_eq!(render(&ExportSpec::Json, &sample().0, &[]).unwrap(), "[]\n");
}

/// Documents (ADR 0006), one a row of one cell, written whole by the JSON formats.
fn documents() -> Vec<Vec<Cell>> {
    vec![
        vec![text(
            r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}, "price": 1.10, "tags": ["a"]}"#,
        )],
        vec![text(r#"{"_id": 2,"nested":{"deep":[{}]}}"#)],
        vec![text("not json")],
    ]
}

fn documents_source() -> RowSource {
    RowSource::without_dialect(dbrover_core::ResultShape::Documents)
}

#[test]
fn writes_documents_whole_as_they_came() {
    let columns = [ExportColumn::text("document")];
    let lines = render(
        &spec_for(ExportFormat::JsonLines, &documents_source()),
        &columns,
        &documents(),
    )
    .unwrap();
    assert_eq!(
        lines,
        "{\"_id\":{\"$oid\":\"5f1d7f0e2b5c3a0001a1b2c3\"},\"price\":1.10,\"tags\":[\"a\"]}\n\
         {\"_id\":2,\"nested\":{\"deep\":[{}]}}\n\
         \"not json\"\n"
    );
    let array = render(
        &spec_for(ExportFormat::Json, &documents_source()),
        &columns,
        &documents()[1..2],
    )
    .unwrap();
    assert_eq!(
        array,
        "[\n  {\n    \"_id\": 2,\n    \"nested\": {\n      \"deep\": [\n        {}\n      ]\n    }\n  }\n]\n"
    );
    assert_eq!(
        render(&ExportSpec::JsonDocuments, &columns, &[]).unwrap(),
        "[]\n"
    );
}

/// The formats a source offers: every one but the SQL ones without a dialect or for
/// documents, and JSON and JSON Lines writing documents whole only for documents.
#[test]
fn offers_the_sql_formats_only_for_sql_rows() {
    let rows = RowSource::without_dialect(dbrover_core::ResultShape::Rows);
    let sources = [
        (rows, "Only for rows from a SQL database"),
        (documents_source(), "Not for documents, which have no table"),
    ];
    for (source, reason) in sources {
        for choice in export_choices(&source) {
            match choice.format {
                ExportFormat::SqlInsert | ExportFormat::SqlUpdate => {
                    assert_eq!(choice.spec, Err(reason), "{}", choice.name);
                }
                _ => assert!(choice.spec.is_ok(), "{}", choice.name),
            }
        }
    }
    let writes_documents = |source: &RowSource| {
        [ExportFormat::Json, ExportFormat::JsonLines]
            .map(|format| spec_for(format, source).writes_documents())
    };
    assert_eq!(writes_documents(&documents_source()), [true, true]);
    assert_eq!(writes_documents(&source()), [false, false]);
    assert_eq!(
        spec_for(ExportFormat::JsonLines, &documents_source()).format(),
        Some(ExportFormat::JsonLines)
    );
}

fn spec_for(format: ExportFormat, source: &RowSource) -> ExportSpec {
    format.default_spec(source).unwrap()
}

#[test]
fn writes_json_lines() {
    assert_eq!(
        rendered(&ExportSpec::JsonLines),
        "{\"id\":\"1\",\"note\":\"plain\",\"id_2\":\"7\"}\n\
         {\"id\":\"2\",\"note\":\"a, \\\"quoted\\\"\\nline\",\"id_2\":null}\n\
         {\"id\":\"3\",\"note\":\"x | <b>&</b>\\ttab;\",\"id_2\":\"\"}\n\
         {\"id\":\"4\",\"note\":\"🦀\\r\\ncrab\",\"id_2\":null}\n"
    );
}

#[test]
fn writes_an_html_fragment_with_every_value_escaped() {
    let spec = ExportSpec::Html(HtmlOptions {
        document: HtmlDocument::Fragment,
        null_text: "—".to_owned(),
    });
    assert_eq!(
        rendered(&spec),
        "<table>\n<thead>\n<tr><th>id</th><th>note</th><th>id</th></tr>\n</thead>\n<tbody>\n\
         <tr><td>1</td><td>plain</td><td>7</td></tr>\n\
         <tr><td>2</td><td>a, &quot;quoted&quot;<br>line</td><td class=\"null\">—</td></tr>\n\
         <tr><td>3</td><td>x | &lt;b&gt;&amp;&lt;/b&gt;\ttab;</td><td></td></tr>\n\
         <tr><td>4</td><td>🦀<br>crab</td><td class=\"null\">—</td></tr>\n\
         </tbody>\n</table>\n"
    );
}

#[test]
fn writes_an_html_page_around_the_table() {
    let page = rendered(&spec(ExportFormat::Html));
    assert!(page.starts_with("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n"));
    assert!(page.contains("<body>\n<table>\n"), "{page}");
    assert!(page.ends_with("</table>\n</body>\n</html>\n"), "{page}");
    assert!(page.contains("<td class=\"null\">NULL</td>"), "{page}");
}

#[test]
fn types_columns_from_the_dialect() {
    let column = dbrover_core::ColumnInfo {
        name: "total".to_owned(),
        type_name: Some("numeric(10,2)".to_owned()),
    };
    let exported = ExportColumn::new(&column, dbrover_sql::Dialect::Postgres);
    assert_eq!(exported.kind, ValueKind::Decimal);
    assert_eq!(exported.type_name.as_deref(), Some("numeric(10,2)"));
}

/// Every text format writes the same file a row at a time as all at once, and as [`render`]
/// does.
#[test]
fn writes_the_same_file_a_page_at_a_time_in_every_format() {
    let dir = tempfile::tempdir().unwrap();
    let (columns, rows) = sample();
    for choice in export_choices(&source()) {
        let Ok(spec) = &choice.spec else {
            continue;
        };
        let Some(text) = render(spec, &columns, &rows) else {
            continue;
        };
        let extension = choice.format.extension();
        let paged_path = dir.path().join(format!("paged.{extension}"));
        let mut writer = ExportWriter::create(&paged_path, spec, &columns).unwrap();
        for row in &rows {
            writer.write(std::slice::from_ref(row)).unwrap();
        }
        assert_eq!(writer.rows(), rows.len());
        writer.finish().unwrap();
        let whole_path = dir.path().join(format!("whole.{extension}"));
        export_rows(&whole_path, spec, &columns, &rows).unwrap();

        let paged = std::fs::read_to_string(&paged_path).unwrap();
        assert_eq!(
            paged,
            std::fs::read_to_string(&whole_path).unwrap(),
            "{}",
            choice.name
        );
        assert_eq!(paged, text, "{}", choice.name);
    }
}

#[test]
fn reports_the_path_it_could_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing/rows.csv");

    let error = export_rows(&path, &spec(ExportFormat::Csv), &[], &[]).unwrap_err();

    assert!(error.to_string().contains("missing/rows.csv"), "{error}");
}

#[test]
fn leaves_the_chosen_file_alone_until_finished_and_when_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.csv");
    std::fs::write(&path, "before").unwrap();
    let (columns, rows) = sample();

    let spec = spec(ExportFormat::Csv);
    let mut writer = ExportWriter::create(&path, &spec, &columns).unwrap();
    writer.write(&rows).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    writer.abandon();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no partial file left"
    );
}

/// What a dialog remembers reads back as it was, a missing field takes its default, and an
/// option no format can use is refused.
#[test]
fn saves_and_reads_back_every_formats_options() {
    let mut options = ExportOptions {
        csv: CsvOptions {
            separator: FieldSeparator::SEMICOLON,
            null_text: "\\N".to_owned(),
            ..CsvOptions::for_excel()
        },
        ..ExportOptions::default()
    };
    options.xlsx.sheet_name = SheetName::try_from("Orders".to_owned()).unwrap();
    options.insert.rows_per_statement = RowsPerInsert::clamped(250);
    options.parquet.compression = ParquetCompression::Snappy;
    options.html.document = HtmlDocument::Fragment;

    let saved = serde_json::to_string(&options).unwrap();
    assert_eq!(
        serde_json::from_str::<ExportOptions>(&saved).unwrap(),
        options
    );

    let partial: ExportOptions = serde_json::from_str(r#"{"csv": {"bom": true}}"#).unwrap();
    assert_eq!(partial.csv, CsvOptions::for_excel());
    assert_eq!(partial.xlsx, XlsxOptions::default());
    for refused in [
        r#"{"csv": {"separator": "\""}}"#,
        r#"{"xlsx": {"sheet_name": "a/b"}}"#,
    ] {
        assert!(
            serde_json::from_str::<ExportOptions>(refused).is_err(),
            "{refused}"
        );
    }
}
