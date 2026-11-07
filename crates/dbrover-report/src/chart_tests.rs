//! A report's `[[chart]]` read and planned (`docs/charts-as-code.md`, CH3c).

use std::path::PathBuf;

use dbrover_chart::{ChartKind, ChartSpec};
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, SafetyLevel, SqliteConfig};

use super::*;

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
        (
            "[[chart]]\nfile = \"net-{since:%Y-%m}.svg\"\nquery = \"SELECT 1\"",
            vec![
                "chart 1: no key `query`; a chart's keys are `kind`, `x`, `y`, `series`, \
                 `stack`, `title`, `x_title`, `y_title`, `y_zero`, `sort`, `limit`, `file`, \
                 `width`, `height`, `scale`",
            ],
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
