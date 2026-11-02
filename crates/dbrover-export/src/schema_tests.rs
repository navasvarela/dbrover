//! Output schemas: each type's conversions, the columns they map, and the CSV and Excel files
//! written through them.

use std::io::Read as _;
use std::path::Path;

use calamine::{Data, Reader, Xlsx, open_workbook};
use dbrover_core::{Cell, ValueKind};

use super::*;

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

fn source(name: &str, kind: ValueKind) -> ExportColumn {
    ExportColumn {
        name: name.to_owned(),
        type_name: None,
        kind,
        excel_format: None,
    }
}

fn typed(name: &str, from: &str, column_type: &str) -> SchemaColumn {
    SchemaColumn {
        name: name.to_owned(),
        column_type: Some(column_type.parse().unwrap()),
        ..SchemaColumn::same(from)
    }
}

/// `value` converted alone in a column of `column`'s type.
fn convert_one(column: &SchemaColumn, target: CellTarget, value: &str) -> Result<Cell, String> {
    let result = [source(&column.from, ValueKind::Undeclared)];
    let schema = OutputSchema::new(std::slice::from_ref(column), &result).unwrap();
    let mut converter = RowConverter::new(schema, target);
    converter
        .convert(&[text(value)])
        .map(|mut row| row.remove(0))
        .map_err(|error| error.reason)
}

#[test]
fn reads_type_names_as_a_report_file_writes_them() {
    let decimal = |precision, scale| ColumnType::Decimal(DecimalSpec::new(precision, scale));
    let cases = [
        ("text", Some(ColumnType::Text)),
        ("INTEGER", Some(ColumnType::Integer)),
        ("decimal", Some(ColumnType::Decimal(None))),
        ("decimal(18,2)", Some(decimal(18, 2))),
        ("Decimal( 38 , 18 )", Some(decimal(38, 18))),
        ("decimal(76,0)", Some(decimal(76, 0))),
        ("boolean", Some(ColumnType::Boolean)),
        ("date", Some(ColumnType::Date)),
        ("timestamp", Some(ColumnType::Timestamp)),
        ("decimal(0,0)", None),
        ("decimal(2,3)", None),
        ("decimal(77,2)", None),
        ("decimal(18)", None),
        ("decimal(a,b)", None),
        ("float", None),
        ("", None),
    ];
    for (name, expected) in cases {
        assert_eq!(name.parse::<ColumnType>().ok(), expected, "{name:?}");
    }
    for name in ["decimal(18,2)", "decimal", "timestamp"] {
        assert_eq!(name.parse::<ColumnType>().unwrap().to_string(), name);
    }
}

/// What each type makes of the values databases print, written for a text format.
#[test]
fn converts_each_type_to_its_plain_form() {
    let cases: &[(&str, &str, Result<&str, &str>)] = &[
        ("text", "  007 ", Ok("  007 ")),
        ("integer", "42", Ok("42")),
        ("integer", "-0042", Ok("-42")),
        ("integer", "15.000", Ok("15")),
        ("integer", "1.5e3", Ok("1500")),
        ("integer", "15.5", Err("isn't a whole number")),
        ("integer", "forty", Err("isn't a whole number")),
        ("decimal", "12.50", Ok("12.50")),
        (
            "decimal",
            "+0.000000000000000001",
            Ok("0.000000000000000001"),
        ),
        ("decimal", "-1.5E-3", Ok("-0.0015")),
        ("decimal", "1,5", Err("isn't a decimal number")),
        ("decimal(18,2)", "1.005", Ok("1.01")),
        ("decimal(18,2)", "-2.345", Ok("-2.35")),
        ("decimal(18,2)", "7", Ok("7.00")),
        ("decimal(18,2)", "0.004", Ok("0.00")),
        (
            "decimal(4,2)",
            "99.995",
            Err("has 3 digits before the point"),
        ),
        ("decimal(4,2)", "99.994", Ok("99.99")),
        ("decimal(2,2)", "0.5", Ok("0.50")),
        (
            "decimal(38,18)",
            "123.456789012345678901234",
            Ok("123.456789012345678901"),
        ),
        ("boolean", "t", Ok("true")),
        ("boolean", "No", Ok("false")),
        ("boolean", "1", Ok("true")),
        ("boolean", "maybe", Err("isn't true or false")),
        ("date", "2024-02-29", Ok("2024-02-29")),
        ("date", "2024-02-29 00:00:00", Ok("2024-02-29")),
        ("date", "2024-02-29 08:00:00", Err("has a time of day")),
        ("date", "2023-02-29", Err("isn't a date")),
        ("date", "29/02/2024", Err("isn't a date")),
        (
            "timestamp",
            "2024-01-31 23:59:58",
            Ok("2024-01-31 23:59:58"),
        ),
        ("timestamp", "2024-01-31T08:00", Ok("2024-01-31 08:00:00")),
        ("timestamp", "2024-01-31", Ok("2024-01-31 00:00:00")),
        (
            "timestamp",
            "2024-01-31 08:00:00.123456",
            Ok("2024-01-31 08:00:00.123456"),
        ),
        (
            "timestamp",
            "2024-01-31 08:00:00+02",
            Err("has a time zone offset"),
        ),
        (
            "timestamp",
            "2024-01-31T08:00:00Z",
            Err("has a time zone offset"),
        ),
        ("timestamp", "yesterday", Err("isn't a date and time")),
    ];
    for (column_type, value, expected) in cases {
        let column = typed("c", "c", column_type);
        let converted = convert_one(&column, CellTarget::Text, value);
        match expected {
            Ok(written) => assert_eq!(converted, Ok(text(written)), "{column_type} {value:?}"),
            Err(reason) => {
                let reason_given = converted.expect_err(&format!("{column_type} {value:?}"));
                assert!(
                    reason_given.contains(reason),
                    "{column_type} {value:?}: {reason_given}"
                );
            }
        }
    }
}

#[test]
fn converts_timestamps_with_an_offset_to_the_columns_zone() {
    let mut column = typed("At", "at", "timestamp");
    column.time_zone = Some(jiff::tz::TimeZone::UTC);
    let cases = [
        ("2024-01-31 08:00:00+02", "2024-01-31 06:00:00"),
        ("2024-01-31T08:00:00Z", "2024-01-31 08:00:00"),
        ("2024-01-31 08:00:00.5-05:00", "2024-01-31 13:00:00.5"),
        ("2024-01-31 08:00:00", "2024-01-31 08:00:00"),
    ];
    for (value, written) in cases {
        assert_eq!(
            convert_one(&column, CellTarget::Text, value),
            Ok(text(written)),
            "{value}"
        );
    }
    column.time_zone = Some(jiff::tz::TimeZone::get("Europe/Madrid").unwrap());
    assert_eq!(
        convert_one(&column, CellTarget::Text, "2024-07-01 10:00:00+00"),
        Ok(text("2024-07-01 12:00:00"))
    );
}

#[test]
fn rounds_times_to_a_millisecond_for_excel() {
    let column = typed("At", "at", "timestamp");
    assert_eq!(
        convert_one(&column, CellTarget::Excel, "2024-01-31 08:00:00.123456"),
        Ok(text("2024-01-31 08:00:00.123"))
    );
    assert_eq!(
        convert_one(&column, CellTarget::Excel, "2024-01-31 08:00:00.9996"),
        Ok(text("2024-01-31 08:00:01"))
    );
}

#[test]
fn maps_declared_columns_and_names_a_missing_one() {
    let result = [
        source("id", ValueKind::Integer),
        source("amount", ValueKind::Decimal),
        source("note", ValueKind::Text),
    ];
    let declared = [
        typed("Amount", "amount", "decimal(10,1)"),
        SchemaColumn::same("id"),
    ];
    let schema = OutputSchema::new(&declared, &result).unwrap();
    let columns = schema.export_columns();
    assert_eq!(
        columns
            .iter()
            .map(|column| (
                column.name.as_str(),
                column.kind,
                column.type_name.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            ("Amount", ValueKind::Decimal, Some("decimal(10,1)")),
            ("id", ValueKind::Integer, None),
        ]
    );
    let mut converter = RowConverter::new(schema, CellTarget::Text);
    assert_eq!(
        converter.convert(&[text("7"), text("2.25"), text("x")]),
        Ok(vec![text("2.3"), text("7")])
    );
    assert_eq!(converter.convert(&[None, None, None]), Ok(vec![None, None]));

    let missing = OutputSchema::new(&[SchemaColumn::same("total")], &result).unwrap_err();
    assert_eq!(
        missing.to_string(),
        "column \"total\" comes from \"total\", which the result doesn't have; it has id, \
         amount, note"
    );
    assert_eq!(OutputSchema::all(&result).export_columns(), result.to_vec());
}

#[test]
fn fails_writes_nothing_or_keeps_the_text_of_a_value_that_doesnt_fit() {
    let result = [source("amount", ValueKind::Text)];
    let rows = [text("1.5"), text("n/a"), text("2")];
    let run = |if_invalid| {
        let column = SchemaColumn {
            if_invalid,
            ..typed("Amount", "amount", "decimal(9,2)")
        };
        let schema = OutputSchema::new(&[column], &result).unwrap();
        let mut converter = RowConverter::new(schema, CellTarget::Text);
        rows.iter()
            .map(|cell| {
                converter
                    .convert(std::slice::from_ref(cell))
                    .map(|mut row| row.remove(0))
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let failed = run(IfInvalid::Fail).unwrap_err();
    assert_eq!(
        failed.to_string(),
        "row 2, column \"Amount\": \"n/a\" isn't a decimal number"
    );
    assert_eq!(
        run(IfInvalid::Null),
        Ok(vec![text("1.50"), None, text("2.00")])
    );
    assert_eq!(
        run(IfInvalid::Text),
        Ok(vec![text("1.50"), text("n/a"), text("2.00")])
    );
}

#[test]
fn warns_once_per_column_about_numbers_excel_writes_as_text() {
    let result = [
        source("wei", ValueKind::Text),
        source("usd", ValueKind::Text),
    ];
    let declared = [
        typed("Wei", "wei", "decimal"),
        typed("USD", "usd", "decimal(38,2)"),
    ];
    let mut excel = RowConverter::new(
        OutputSchema::new(&declared, &result).unwrap(),
        CellTarget::Excel,
    );
    for row in [
        [text("1234567890123456789"), text("1234.5678")],
        [text("12"), text("1.005")],
        [text("0.1234567890123456"), text("9")],
    ] {
        excel.convert(&row).unwrap();
    }
    assert_eq!(
        excel.warnings(),
        [
            "column \"Wei\": 2 cells had more digits than the 15 Excel's numbers hold, so were \
             written as text; a type that rounds, such as decimal(38,2), keeps them numbers"
        ]
    );

    let mut csv = RowConverter::new(
        OutputSchema::new(&declared, &result).unwrap(),
        CellTarget::Text,
    );
    csv.convert(&[text("1234567890123456789"), text("1")])
        .unwrap();
    assert!(csv.warnings().is_empty());
}

/// The trial balance's columns, and three accounts' rows as a database prints them.
fn trial_balance() -> (Vec<ExportColumn>, Vec<SchemaColumn>, Vec<Vec<Cell>>) {
    let result = vec![
        source("account", ValueKind::Undeclared),
        source("debit", ValueKind::Undeclared),
        source("credit", ValueKind::Undeclared),
        source("booked", ValueKind::Undeclared),
    ];
    let mut debit = typed("Debit", "debit", "decimal(18,2)");
    debit.excel_format = Some("#,##0.00".to_owned());
    let mut booked = typed("Last entry", "booked", "timestamp");
    booked.excel_format = Some("yyyy-mm-dd hh:mm".to_owned());
    let declared = vec![
        SchemaColumn {
            name: "Account".to_owned(),
            column_type: Some(ColumnType::Text),
            ..SchemaColumn::same("account")
        },
        debit,
        typed("Credit", "credit", "decimal(18,2)"),
        booked,
    ];
    let rows = vec![
        vec![
            text("1000"),
            text("1234.565"),
            text("0"),
            text("2026-10-31 17:45:00"),
        ],
        vec![
            text("0400"),
            text("0"),
            text("99.5"),
            text("2026-10-02 09:00:00.250"),
        ],
        vec![text("2000"), None, text("1135.07"), None],
    ];
    (result, declared, rows)
}

fn write(path: &Path, spec: &ExportSpec, target: CellTarget) {
    let (result, declared, rows) = trial_balance();
    let mut converter = RowConverter::new(OutputSchema::new(&declared, &result).unwrap(), target);
    let converted: Vec<Vec<Cell>> = rows
        .iter()
        .map(|row| converter.convert(row).unwrap())
        .collect();
    export_rows(path, spec, &converter.schema().export_columns(), &converted).unwrap();
}

#[test]
fn writes_csv_in_each_types_plain_form() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("trial-balance.csv");
    write(
        &path,
        &ExportSpec::Csv(CsvOptions::default()),
        CellTarget::Text,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "Account,Debit,Credit,Last entry\r\n\
         1000,1234.57,0.00,2026-10-31 17:45:00\r\n\
         0400,0.00,99.50,2026-10-02 09:00:00.25\r\n\
         2000,,1135.07,\r\n"
    );
}

#[test]
fn writes_excel_numbers_and_dates_in_each_columns_format() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("trial-balance.xlsx");
    write(
        &path,
        &ExportSpec::Xlsx(XlsxOptions::default()),
        CellTarget::Excel,
    );

    let mut book: Xlsx<_> = open_workbook(&path).unwrap();
    let range = book.worksheet_range("Rows").unwrap();
    let rows: Vec<Vec<Data>> = range.rows().map(<[Data]>::to_vec).collect();
    assert_eq!(
        rows[1][..3],
        [
            Data::String("1000".to_owned()),
            Data::Float(1234.57),
            Data::Float(0.0)
        ]
    );
    assert_eq!(rows[2][0], Data::String("0400".to_owned()));
    assert!(matches!(rows[1][3], Data::DateTime(_)), "{:?}", rows[1][3]);
    assert_eq!(rows[3][1], Data::Empty);

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut styles = String::new();
    archive
        .by_name("xl/styles.xml")
        .unwrap()
        .read_to_string(&mut styles)
        .unwrap();
    for format in ["#,##0.00", "yyyy-mm-dd hh:mm"] {
        assert!(
            styles.contains(&format!("formatCode=\"{format}\"")),
            "{format} in {styles}"
        );
    }
}
