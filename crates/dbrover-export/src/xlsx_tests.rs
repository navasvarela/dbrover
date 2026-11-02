//! Excel files written and read back with `calamine`: each cell's type and value, the header,
//! and sheets past a sheet's worth of rows.

use std::io::Read as _;
use std::path::Path;

use calamine::{Data, Reader, Xlsx, open_workbook};
use dbrover_core::{Cell, ValueKind};

use super::*;
use crate::xlsx::{XlsxFile, xlsx_sheets};

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

fn column(name: &str, kind: ValueKind) -> ExportColumn {
    ExportColumn {
        name: name.to_owned(),
        type_name: None,
        kind,
        excel_format: None,
    }
}

/// Excel's serial number of 2024-01-31, the day the dates below fall on.
const JANUARY_31: f64 = 45_322.0;

fn sheets(path: &Path) -> Vec<(String, Vec<Vec<Data>>)> {
    let mut book: Xlsx<_> = open_workbook(path).unwrap();
    book.sheet_names()
        .into_iter()
        .map(|name| {
            let range = book.worksheet_range(&name).unwrap();
            let rows = range.rows().map(<[Data]>::to_vec).collect();
            (name, rows)
        })
        .collect()
}

/// A sheet's XML, for what `calamine` doesn't read: frozen panes and the filter.
fn sheet_xml(path: &Path, sheet: usize) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut xml = String::new();
    archive
        .by_name(&format!("xl/worksheets/sheet{sheet}.xml"))
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

fn date(data: &Data) -> Option<f64> {
    match data {
        Data::DateTime(value) => Some(value.as_f64()),
        _ => None,
    }
}

#[test]
fn types_each_cell_and_writes_a_frozen_filtered_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.xlsx");
    let columns = vec![
        column("id", ValueKind::Integer),
        column("price", ValueKind::Decimal),
        column("code", ValueKind::Text),
        column("zip", ValueKind::Undeclared),
        column("born", ValueKind::Date),
        column("seen", ValueKind::Timestamp),
        column("seen_tz", ValueKind::TimestampWithZone),
        column("ok", ValueKind::Boolean),
        column("note", ValueKind::Undeclared),
    ];
    let rows = vec![
        vec![
            text("1"),
            text("9.50"),
            text("007"),
            text("02134"),
            text("2024-01-31"),
            text("2024-01-31 08:00:00"),
            text("2024-01-31 08:00:00+00"),
            text("t"),
            None,
        ],
        vec![
            text("9007199254740993"),
            text("NaN"),
            text("x"),
            text("42"),
            text("1899-12-31"),
            text("2024-01-31 08:00:00.123456"),
            text("2024-01-31 08:00:00+00"),
            text("f"),
            text("2024-01-31"),
        ],
    ];

    export_rows(
        &path,
        &ExportSpec::Xlsx(XlsxOptions::default()),
        &columns,
        &rows,
    )
    .unwrap();

    let sheets = sheets(&path);
    let [(name, cells)] = sheets.as_slice() else {
        panic!("one sheet expected: {sheets:?}");
    };
    assert_eq!(name, "Rows");
    let header: Vec<Data> = columns
        .iter()
        .map(|column| Data::String(column.name.clone()))
        .collect();
    assert_eq!(cells[0], header);
    let first = &cells[1];
    assert_eq!(
        first[..4],
        [
            Data::Float(1.0),
            Data::Float(9.5),
            Data::String("007".to_owned()),
            Data::String("02134".to_owned()),
        ]
    );
    assert_eq!(date(&first[4]), Some(JANUARY_31));
    let eight_o_clock = date(&first[5]).unwrap();
    assert!(
        (eight_o_clock - (JANUARY_31 + 8.0 / 24.0)).abs() < 1e-9,
        "{eight_o_clock}"
    );
    assert_eq!(
        first[6..],
        [
            Data::String("2024-01-31 08:00:00+00".to_owned()),
            Data::Bool(true),
            Data::Empty,
        ]
    );
    let second = &cells[2];
    assert_eq!(
        second[..8],
        [
            Data::String("9007199254740993".to_owned()),
            Data::String("NaN".to_owned()),
            Data::String("x".to_owned()),
            Data::Float(42.0),
            Data::String("1899-12-31".to_owned()),
            Data::String("2024-01-31 08:00:00.123456".to_owned()),
            Data::String("2024-01-31 08:00:00+00".to_owned()),
            Data::Bool(false),
        ]
    );
    assert_eq!(date(&second[8]), Some(JANUARY_31));

    let xml = sheet_xml(&path, 1);
    assert!(
        xml.contains(r#"<pane ySplit="1" topLeftCell="A2""#),
        "{xml}"
    );
    assert!(xml.contains(r#"<autoFilter ref="A1:I3"/>"#), "{xml}");
}

#[test]
fn leaves_out_the_header_freeze_and_filter_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.xlsx");
    let options = XlsxOptions {
        header: false,
        sheet_name: SheetName::try_from("Orders".to_owned()).unwrap(),
        ..XlsxOptions::default()
    };
    let columns = [column("n", ValueKind::Integer)];

    export_rows(
        &path,
        &ExportSpec::Xlsx(options),
        &columns,
        &[vec![text("5")]],
    )
    .unwrap();

    assert_eq!(
        sheets(&path),
        [("Orders".to_owned(), vec![vec![Data::Float(5.0)]])]
    );
    let xml = sheet_xml(&path, 1);
    assert!(!xml.contains("<pane"), "{xml}");
    assert!(!xml.contains("<autoFilter"), "{xml}");
}

/// Past a sheet's rows, the rows go on in `Rows 2`, `Rows 3`, each with the header.
#[test]
fn goes_on_in_a_new_sheet_when_one_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.xlsx");
    let columns = vec![column("n", ValueKind::Integer)];
    let rows: Vec<Vec<Cell>> = (1..=7).map(|n| vec![Some(n.to_string())]).collect();
    let mut writer = ExportWriter::with_format(&path, |partial| {
        let file = XlsxFile::with_max_rows(partial, XlsxOptions::default(), &columns, 4)?;
        Ok(Box::new(file))
    })
    .unwrap();
    writer.write(&rows).unwrap();
    writer.finish().unwrap();

    let header = || vec![Data::String("n".to_owned())];
    let numbers =
        |range: std::ops::RangeInclusive<u8>| range.map(|n| vec![Data::Float(f64::from(n))]);
    let expected: Vec<(String, Vec<Vec<Data>>)> =
        [("Rows", 1..=3), ("Rows 2", 4..=6), ("Rows 3", 7..=7)]
            .into_iter()
            .map(|(name, range)| {
                let rows = std::iter::once(header()).chain(numbers(range)).collect();
                (name.to_owned(), rows)
            })
            .collect();
    assert_eq!(sheets(&path), expected);
    assert!(sheet_xml(&path, 3).contains(r#"<autoFilter ref="A1:A2"/>"#));
}

#[test]
fn checks_sheet_names_and_counts_sheets() {
    for refused in ["", "a/b", "[x]", "'quoted'", &"x".repeat(32)] {
        assert!(
            SheetName::try_from(refused.to_owned()).is_err(),
            "{refused:?}"
        );
    }
    let long = SheetName::try_from("x".repeat(31)).unwrap();
    assert_eq!(long.numbered(12).chars().count(), 31);
    assert!(long.numbered(12).ends_with("x 12"));

    assert_eq!(xlsx_sheets(0, true), 1);
    assert_eq!(xlsx_sheets(1_048_575, true), 1);
    assert_eq!(xlsx_sheets(1_048_576, true), 2);
    assert_eq!(xlsx_sheets(1_048_576, false), 1);
}

/// A real sheet's worth: one row more than an Excel sheet holds goes into a second sheet.
#[test]
#[ignore = "slow: writes more rows than an Excel sheet holds; run with --ignored"]
fn fills_a_whole_sheet_and_starts_another() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.xlsx");
    let columns = vec![column("n", ValueKind::Integer)];
    let mut writer =
        ExportWriter::create(&path, &ExportSpec::Xlsx(XlsxOptions::default()), &columns).unwrap();
    let total = EXCEL_MAX_ROWS;
    let page = 10_000;
    for start in (1..=total).step_by(page) {
        let end = (start + u32::try_from(page).unwrap() - 1).min(total);
        let rows: Vec<Vec<Cell>> = (start..=end).map(|n| vec![Some(n.to_string())]).collect();
        writer.write(&rows).unwrap();
    }
    writer.finish().unwrap();

    let sheets = sheets(&path);
    let names: Vec<&str> = sheets.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Rows", "Rows 2"]);
    assert_eq!(sheets[0].1.len(), 1_048_576);
    assert_eq!(
        sheets[1].1,
        [
            vec![Data::String("n".to_owned())],
            vec![Data::Float(f64::from(total))]
        ]
    );
}
