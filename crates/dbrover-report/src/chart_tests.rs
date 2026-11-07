//! A report's `[[chart]]` read and planned (`docs/charts-as-code.md`, CH3c), and drawn in a run
//! (CH3d).

use std::path::{Path, PathBuf};

use dbrover_chart::{ChartKind, ChartSpec};
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, Row, SafetyLevel, SqliteConfig};

use super::*;
use crate::run_tests::{MemoryOpener, MemoryReader, column, files_in};

fn shop() -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "shop".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/shop.db"),
            read_only: false,
        }),
    }
}

/// A report of net sales by category, with `charts` after its one output.
fn with_charts(charts: &str) -> String {
    format!(
        "[report]\nname = \"Net\"\nparams = {{ since = \"date\" }}\n\
         [[source]]\nid = \"lines\"\ndata_source = \"shop\"\n\
         query = \"SELECT category, net FROM sales WHERE month = :since\"\n\
         [[output]]\nformat = \"csv\"\nfile = \"net-{{since:%Y-%m}}.csv\"\n{charts}"
    )
}

fn problems(charts: &str) -> Vec<String> {
    read_report(&with_charts(charts)).expect_err(charts)
}

#[test]
fn reads_a_chart_with_its_keys_and_the_files() {
    let report = read_report(&with_charts(
        "[[chart]]\nfile = \"net-{since:%Y-%m}.svg\"\ntitle = \"Net sales\"\nx = \"category\"\n\
         y = \"net\"\nkind = \"bars\"\n\
         [[chart]]\nfile = \"net.PNG\"\nwidth = 600\nheight = 300\nscale = 3",
    ))
    .unwrap();

    let [svg, png] = report.charts.as_slice() else {
        panic!("two charts: {:?}", report.charts);
    };
    assert_eq!(svg.file.as_str(), "net-{since:%Y-%m}.svg");
    assert_eq!(svg.picture, ChartPicture::Svg);
    assert_eq!((svg.width, svg.height), (800, 450));
    assert_eq!(
        svg.spec,
        ChartSpec {
            kind: Some(ChartKind::Bars),
            x: Some("category".to_owned()),
            y: vec!["net".to_owned()],
            title: Some("Net sales".to_owned()),
            ..ChartSpec::default()
        }
    );
    assert_eq!(png.picture, ChartPicture::Png { scale: 3 });
    assert_eq!((png.width, png.height), (600, 300));
    assert_eq!(png.spec, ChartSpec::default());
}

#[test]
fn a_pngs_scale_is_two_unless_it_says() {
    let report = read_report(&with_charts("[[chart]]\nfile = \"net.png\"")).unwrap();

    assert_eq!(report.charts[0].picture, ChartPicture::Png { scale: 2 });
}

#[test]
fn names_each_charts_problems_and_the_chart_they_are_in() {
    let cases = [
        (
            "[[chart]]\nx = \"category\"",
            vec!["chart 1: needs a file name, as file = \"net-by-category.svg\""],
        ),
        (
            "[[chart]]\nfile = \"net.pdf\"",
            vec!["chart 1: file: a chart writes an .svg or a .png file"],
        ),
        (
            "[[chart]]\nfile = \"net-{month}.svg\"",
            vec!["chart 1: file: {month} names no parameter of the report"],
        ),
        (
            "[[chart]]\nfile = \"net.svg\"\nscale = 2",
            vec![
                "chart 1: scale sizes a PNG's pixels, and this chart writes SVG; take it out, or \
                 name a .png file",
            ],
        ),
        (
            "[[chart]]\nfile = \"net.png\"\nscale = 4",
            vec!["chart 1: scale must be 1, 2 or 3"],
        ),
        (
            "[[chart]]\nfile = \"net.svg\"\nwidth = 100\nheight = 5000",
            vec![
                "chart 1: width is a number of points from 200 to 4,000",
                "chart 1: height is a number of points from 200 to 4,000",
            ],
        ),
        (
            "[[chart]]\nfile = \"net.png\"\nwidth = 4000\nheight = 3000\nscale = 3",
            vec![
                "chart 1: a PNG of 4000 × 3000 points at 3× is too large to draw; make the \
                 chart smaller, lower scale, or write SVG",
            ],
        ),
        (
            "[[chart]]\nfile = \"net.svg\"\nfiel = \"net.svg\"\nkind = \"area\"",
            vec![
                "chart 1: no key `fiel`; did you mean `file`?",
                "chart 1: `kind` is bars, lines or scatter, not \"area\"",
            ],
        ),
        (
            "[[chart]]\nfile = \"net-{since:%Y-%m}.csv\"",
            vec!["chart 1: file: a chart writes an .svg or a .png file"],
        ),
        (
            "[[chart]]\nfile = \"net.svg\"\n[[chart]]\nfile = \"net.svg\"",
            vec!["chart 2: another output or chart already writes \"net.svg\""],
        ),
    ];
    for (charts, expected) in cases {
        assert_eq!(problems(charts), expected, "{charts}");
    }
}

#[test]
fn a_chart_table_that_isnt_a_list_of_tables_is_a_problem() {
    let text = format!("chart = 3\n{}", with_charts(""));

    assert_eq!(
        read_report(&text).unwrap_err(),
        ["chart must be a list of [[chart]] tables"]
    );
}

#[test]
fn places_a_charts_problem_on_its_key() {
    let text = with_charts("[[chart]]\nfile = \"net.svg\"\nkind = \"area\"");
    let problems = read_with_places(&text).unwrap_err();
    let locator = Locator::new(&text);

    let underlined: Vec<&str> = problems
        .iter()
        .map(|problem| &text[locator.locate(&problem.place)])
        .collect();
    assert_eq!(underlined, ["kind"]);
}

#[test]
fn names_each_charts_file_with_the_runs_values() {
    let report = read_report(&with_charts(
        "[[chart]]\nfile = \"net-{since:%Y-%m}.svg\"\n[[chart]]\nfile = \"net.png\"",
    ))
    .unwrap();
    let plan = Plan::new(report, &[shop()], &[], &LimitDefaults::default()).unwrap();
    let typed: TypedValues = [("since".to_owned(), "2026-10-01".to_owned())].into();

    let values = plan.values(&typed).unwrap();

    assert_eq!(values.files, ["net-2026-10.csv"]);
    assert_eq!(values.charts, ["net-2026-10.svg", "net.png"]);
}

#[test]
fn refuses_values_that_make_a_chart_an_outputs_file() {
    let report = read_report(
        &with_charts("[[chart]]\nfile = \"{name}.svg\"")
            .replace("since = \"date\"", "since = \"date\", name = \"text\"")
            .replace("net-{since:%Y-%m}.csv", "{since}.svg"),
    );
    let plan = Plan::new(report.unwrap(), &[shop()], &[], &LimitDefaults::default()).unwrap();
    let typed: TypedValues = [
        ("since".to_owned(), "2026-10-01".to_owned()),
        ("name".to_owned(), "2026-10-01".to_owned()),
    ]
    .into();

    assert_eq!(
        plan.values(&typed).unwrap_err(),
        ["chart 1: another output or chart already writes \"2026-10-01.svg\" with these values"]
    );
}

/// `count` categories, each a row of its name and its net, in pages of 100.
fn categories(count: usize) -> MemoryReader {
    let rows: Vec<Row> = (0..count)
        .map(|n| vec![Some(format!("category {n:03}")), Some(format!("{n}.5"))])
        .collect();
    MemoryReader {
        columns: vec![column("category"), column("net")],
        pages: rows.chunks(100).map(<[Row]>::to_vec).collect(),
        then: None,
    }
}

/// Runs the report with `charts` on `reader`, in October 2026, into `folder`.
async fn run_charts(
    charts: &str,
    reader: MemoryReader,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let plan = Plan::new(
        read_report(&with_charts(charts)).unwrap(),
        &[shop()],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let typed: TypedValues = [("since".to_owned(), "2026-10-01".to_owned())].into();
    let values = plan.values(&typed).unwrap();
    let staging = folder.join(".run");
    let folders = RunFolders {
        folder,
        staging: &staging,
    };
    let opener = MemoryOpener::of(vec![("lines", reader)]);
    let mut events = Vec::new();
    let outcome = run(&plan, &values, &opener, folders, &mut |event| {
        events.push(event)
    })
    .await;
    (outcome, events)
}

fn written(events: &[RunEvent]) -> Vec<&WrittenFile> {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::OutputWritten(file) => Some(file),
            _ => None,
        })
        .collect()
}

/// The texts of an SVG's `<text>` elements.
fn texts(svg: &str) -> Vec<&str> {
    svg.lines()
        .filter_map(|line| line.split_once('>')?.1.strip_suffix("</text>"))
        .collect()
}

#[tokio::test]
async fn writes_a_chart_beside_the_outputs_and_reports_it_as_a_file() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, events) = run_charts(
        "[[chart]]\nfile = \"net-{since:%Y-%m}.svg\"\ntitle = \"Net by category\"",
        categories(3),
        folder.path(),
    )
    .await;

    outcome.unwrap();
    assert_eq!(
        files_in(folder.path()),
        ["net-2026-10.csv", "net-2026-10.svg"]
    );
    let svg = std::fs::read_to_string(folder.path().join("net-2026-10.svg")).unwrap();
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"800\" height=\"450\"")
    );
    let texts = texts(&svg);
    assert!(texts.contains(&"Net by category"), "{texts:?}");
    assert!(texts.contains(&"category 002"), "{texts:?}");
    assert_eq!(texts.last(), Some(&"Charting 3 rows"));
    let written = written(&events);
    assert_eq!(written.len(), 2);
    assert_eq!(written[1].path, folder.path().join("net-2026-10.svg"));
    assert_eq!(written[1].rows, 3);
    assert_eq!(written[1].size, svg.len() as u64);
    assert!(written[1].warnings.is_empty());
}

#[tokio::test]
async fn the_same_rows_make_the_same_bytes() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let chart = "[[chart]]\nfile = \"net.svg\"";
    run_charts(chart, categories(40), first.path())
        .await
        .0
        .unwrap();
    run_charts(chart, categories(40), second.path())
        .await
        .0
        .unwrap();

    assert_eq!(
        std::fs::read(first.path().join("net.svg")).unwrap(),
        std::fs::read(second.path().join("net.svg")).unwrap()
    );
}

#[tokio::test]
async fn sorts_and_cuts_to_the_top_rows_as_the_pages_pass() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_charts(
        "[[chart]]\nfile = \"top.svg\"\nsort = \"net desc\"\nlimit = 3",
        categories(1_000),
        folder.path(),
    )
    .await;

    outcome.unwrap();
    let svg = std::fs::read_to_string(folder.path().join("top.svg")).unwrap();
    let texts = texts(&svg);
    for top in ["category 999", "category 998", "category 997"] {
        assert!(texts.contains(&top), "{top} in {texts:?}");
    }
    assert!(!texts.contains(&"category 996"));
    assert_eq!(
        texts.last(),
        Some(&"Charting 3 rows · sorted by net, largest first, the first 3")
    );
}

#[tokio::test]
async fn fails_past_its_kinds_rows_and_leaves_no_file() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, events) = run_charts(
        "[[chart]]\nfile = \"net.svg\"\nkind = \"bars\"",
        categories(251),
        folder.path(),
    )
    .await;

    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.to_string(),
        "chart 1 (net.svg): it draws bars, at most 250 rows, and the result has 251; group the \
         rows in the query, or set limit to draw the first ones"
    );
    let limit = failure.limit.unwrap();
    assert_eq!(
        (limit.key.as_str(), limit.value.as_str()),
        ("limit", "250 rows of bars")
    );
    assert_eq!(
        limit.advice(),
        "It stopped at 250 rows of bars, the most a chart of its kind draws. Group the rows in \
         the query, or set limit in chart 1 of the report's file to draw the first ones."
    );
    assert_eq!(files_in(folder.path()), Vec::<String>::new());
    assert!(written(&events).is_empty());
}

#[tokio::test]
async fn a_limit_draws_the_first_rows_and_cant_lift_the_kinds() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_charts(
        "[[chart]]\nfile = \"first.svg\"\nlimit = 250",
        categories(1_204),
        folder.path(),
    )
    .await;
    outcome.unwrap();

    let (outcome, _) = run_charts(
        "[[chart]]\nfile = \"all.svg\"\nlimit = 1000",
        categories(1_204),
        folder.path(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().message,
        "it draws bars, at most 250 rows, and its limit is 1,000 of the result's 1,204; lower \
         limit to 250 or less"
    );
}

#[tokio::test]
async fn fails_past_eight_series_naming_the_values_past_the_eighth() {
    let rows: Vec<Row> = (0..10)
        .map(|n| {
            vec![
                Some("2026-10".to_owned()),
                Some(format!("c{n}")),
                Some("1".to_owned()),
            ]
        })
        .collect();
    let reader = MemoryReader {
        columns: vec![column("month"), column("category"), column("net")],
        pages: [rows].into(),
        then: None,
    };
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_charts(
        "[[chart]]\nfile = \"net.svg\"\nx = \"month\"\ny = \"net\"\nseries = \"category\"",
        reader,
        folder.path(),
    )
    .await;

    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.message,
        "category has more values than the chart's 8 colours; past the eighth: c8, c9"
    );
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("series".to_owned())
    );
}

#[tokio::test]
async fn a_result_without_rows_still_has_its_chart_saying_so() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, events) = run_charts(
        "[[chart]]\nfile = \"net.svg\"\ntitle = \"Net\"\ny = \"net\"",
        categories(0),
        folder.path(),
    )
    .await;

    outcome.unwrap();
    let svg = std::fs::read_to_string(folder.path().join("net.svg")).unwrap();
    assert_eq!(texts(&svg), ["Net", "No rows to chart"]);
    assert_eq!(
        written(&events)[1].warnings,
        ["the result has no rows, so the chart says \"No rows to chart\""]
    );
}

#[tokio::test]
async fn a_column_the_result_doesnt_have_fails_the_charts_step() {
    for rows in [0, 3] {
        let folder = tempfile::tempdir().unwrap();
        let (outcome, _) = run_charts(
            "[[chart]]\nfile = \"net.svg\"\ny = \"nett\"",
            categories(rows),
            folder.path(),
        )
        .await;

        let failure = outcome.unwrap_err();
        assert_eq!(
            failure.to_string(),
            "chart 1 (net.svg): `y = \"nett\"` isn't a column of this result: category, net",
            "{rows} rows"
        );
        assert!(failure.limit.is_none());
        assert_eq!(files_in(folder.path()), Vec::<String>::new());
    }
}

#[tokio::test]
async fn gaps_are_warnings_and_said_under_the_chart() {
    let rows: Vec<Row> = vec![
        vec![Some("a".to_owned()), Some("1".to_owned())],
        vec![Some("b".to_owned()), None],
    ];
    let reader = MemoryReader {
        columns: vec![column("category"), column("net")],
        pages: [rows].into(),
        then: None,
    };
    let folder = tempfile::tempdir().unwrap();
    let (outcome, events) =
        run_charts("[[chart]]\nfile = \"net.svg\"", reader, folder.path()).await;

    outcome.unwrap();
    assert_eq!(
        written(&events)[1].warnings,
        ["1 gap in net: null or not a number"]
    );
    let svg = std::fs::read_to_string(folder.path().join("net.svg")).unwrap();
    assert_eq!(
        texts(&svg).last(),
        Some(&"Charting 2 rows · 1 gap in net: null or not a number")
    );
}

#[tokio::test]
async fn draws_a_png_at_its_scale() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_charts(
        "[[chart]]\nfile = \"net.png\"\nwidth = 400\nheight = 300\nscale = 1",
        categories(3),
        folder.path(),
    )
    .await;

    outcome.unwrap();
    let png = std::fs::read(folder.path().join("net.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    // The PNG header's width and height, big-endian, after its signature and IHDR's length.
    assert_eq!(png[16..24], [0, 0, 1, 144, 0, 0, 1, 44]);
}

/// A report that combines its one source, with `charts`.
fn combined(charts: &str) -> String {
    with_charts(charts).replace(
        "[[output]]",
        "[combine]\nquery = \"SELECT category, net FROM lines\"\n[[output]]",
    )
}

#[test]
fn a_charts_own_query_needs_a_combine_step_and_reads_only() {
    assert_eq!(
        problems("[[chart]]\nfile = \"n.svg\"\nquery = \"SELECT 1\""),
        [
            "chart 1: query reads the sources' tables in the combine step's DuckDB, which a \
             report without [combine] doesn't have; add a [combine], or leave query out to draw \
             the result"
        ]
    );
    let plan = |charts: &str| {
        Plan::new(
            read_report(&combined(charts)).unwrap(),
            &[shop()],
            &[],
            &LimitDefaults::default(),
        )
    };
    assert_eq!(
        plan("[[chart]]\nfile = \"n.svg\"\nquery = \"DELETE FROM lines\"").unwrap_err(),
        ["chart 1: the query changes data; a chart's query only reads the sources' tables"]
    );
    assert_eq!(
        plan("[[chart]]\nfile = \"n.svg\"\nquery = \"SELECT * FROM lines WHERE net > :least\"")
            .unwrap_err(),
        [
            "chart 1: the query uses :least, which [report] params doesn't declare; add least = \
             \"text\" (or its type) to params"
        ]
    );
}

#[tokio::test]
async fn draws_a_charts_own_query_after_the_combine_step() {
    let rows: Vec<Row> = [("bikes", "1290"), ("bikes", "649"), ("parts", "42")]
        .into_iter()
        .map(|(category, net)| vec![Some(category.to_owned()), Some(net.to_owned())])
        .collect();
    let reader = MemoryReader {
        columns: vec![column("category"), column("net")],
        pages: [rows].into(),
        then: None,
    };
    let plan = Plan::new(
        read_report(&combined(
            "[[chart]]\nfile = \"rows.svg\"\n\
             [[chart]]\nfile = \"by-category.svg\"\nx = \"category\"\ny = \"total\"\n\
             query = \"SELECT category, SUM(CAST(net AS DECIMAL(18,2))) AS total FROM lines \
             WHERE :since IS NOT NULL GROUP BY category ORDER BY category\"",
        ))
        .unwrap(),
        &[shop()],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let typed: TypedValues = [("since".to_owned(), "2026-10-01".to_owned())].into();
    let values = plan.values(&typed).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let staging = folder.path().join(".run");
    let folders = RunFolders {
        folder: folder.path(),
        staging: &staging,
    };
    let opener = MemoryOpener::of(vec![("lines", reader)]);
    let mut events = Vec::new();
    run(&plan, &values, &opener, folders, &mut |event| {
        events.push(event)
    })
    .await
    .unwrap();

    assert_eq!(
        files_in(folder.path()),
        ["by-category.svg", "net-2026-10.csv", "rows.svg"]
    );
    let svg = std::fs::read_to_string(folder.path().join("by-category.svg")).unwrap();
    let texts = texts(&svg);
    assert!(texts.contains(&"total"), "{texts:?}");
    assert_eq!(texts.last(), Some(&"Charting 2 rows"));
    let written: Vec<String> = written(&events)
        .iter()
        .filter_map(|file| Some(file.path.file_name()?.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(written, ["net-2026-10.csv", "rows.svg", "by-category.svg"]);
}

#[tokio::test]
async fn a_charts_failing_query_fails_its_step_and_leaves_no_file() {
    let reader = categories(3);
    let plan = Plan::new(
        read_report(&combined(
            "[[chart]]\nfile = \"n.svg\"\nquery = \"SELECT nope FROM lines\"",
        ))
        .unwrap(),
        &[shop()],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let typed: TypedValues = [("since".to_owned(), "2026-10-01".to_owned())].into();
    let values = plan.values(&typed).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let staging = folder.path().join(".run");
    let folders = RunFolders {
        folder: folder.path(),
        staging: &staging,
    };
    let opener = MemoryOpener::of(vec![("lines", reader)]);
    let failure = run(&plan, &values, &opener, folders, &mut |_| {})
        .await
        .unwrap_err();

    assert_eq!(
        failure.step,
        Step::Chart {
            number: 1,
            file: "n.svg".to_owned()
        }
    );
    assert!(failure.message.contains("nope"), "{failure}");
    assert_eq!(files_in(folder.path()), Vec::<String>::new());
}
