//! A report used as another report's source: read and planned
//! (`docs/reports-at-scale.md`, RS9b).

use std::path::PathBuf;

use dbrover_core::{
    BlobId, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, SafetyLevel,
    SqliteConfig,
};
use dbrover_export::ColumnType;

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

/// A DuckDB data source whose file an `attach = true` source reads.
fn duckdb(name: &str, path: &str) -> DataSource {
    DataSource {
        name: name.to_owned(),
        config: ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path: PathBuf::from(path),
            read_only: true,
        })),
        ..shop()
    }
}

/// The inner report: one SQLite source over a period, with its own combine and an output.
const SALES: &str = r#"
[report]
name = "Sales"
params = { since = "date", until = "date" }

[[source]]
id = "lines"
data_source = "shop"
query = "SELECT category, amount FROM lines WHERE ordered_on >= :since AND ordered_on < :until"

[combine]
query = "SELECT category, sum(amount) AS amount FROM lines GROUP BY category"

[[output]]
format = "csv"
file = "sales-{since:%Y-%m}.csv"
"#;

/// The outer report: the inner report's result, grouped again.
const BY_CATEGORY: &str = r#"
[report]
name = "By category"
params = { since = "date", until = "date" }

[[source]]
id = "sales"
report = "sales"
params = { since = ":since", until = ":until" }

[combine]
query = "SELECT category, amount FROM sales ORDER BY amount DESC"

[[output]]
format = "csv"
file = "by-category.csv"
"#;

fn named(reports: &[(&str, &str)]) -> Vec<NamedReport> {
    reports
        .iter()
        .map(|(name, text)| NamedReport {
            name: (*name).to_owned(),
            text: (*text).to_owned(),
        })
        .collect()
}

/// `text` planned against the shop and `reports`.
fn plan(text: &str, reports: &[(&str, &str)]) -> Result<Plan, Vec<String>> {
    Plan::new(
        read_report(text).expect(text),
        &[shop()],
        &[],
        &named(reports),
        &LimitDefaults::default(),
    )
}

/// What planning `text` against `reports` refuses, as sentences.
fn refused(text: &str, reports: &[(&str, &str)]) -> Vec<String> {
    plan(text, reports).expect_err(text)
}

/// The problems reading `text` on its own gives, as sentences.
fn read_problems(text: &str) -> Vec<String> {
    read_report(text).expect_err(text)
}

/// Whether some problem holds `piece`, for a message checked by its substance.
fn says(problems: &[String], piece: &str) -> bool {
    problems.iter().any(|problem| problem.contains(piece))
}

#[test]
fn reads_a_report_source_and_its_bound_parameters() {
    let report = read_report(BY_CATEGORY).unwrap();
    let [source] = report.sources.as_slice() else {
        panic!("{:?}", report.sources);
    };
    assert_eq!(source.id, "sales");
    assert_eq!(source.data_source, None, "a report source names none");
    let SourceRead::Report(read) = &source.read else {
        panic!("{:?}", source.read);
    };
    assert_eq!(read.report, "sales");
    assert_eq!(
        read.params,
        vec![
            BoundParam {
                name: "since".to_owned(),
                from: ParamSource::Param("since".to_owned()),
            },
            BoundParam {
                name: "until".to_owned(),
                from: ParamSource::Param("until".to_owned()),
            },
        ]
    );
}

#[test]
fn plans_the_inner_report_inside_the_outer_source() {
    let plan = plan(BY_CATEGORY, &[("sales", SALES)]).unwrap();
    let [source] = plan.sources() else {
        panic!("{:?}", plan.sources());
    };
    let PlannedSource::Report(source) = source else {
        panic!("{source:?}");
    };
    assert_eq!(source.id, "sales");
    assert_eq!(source.report, "sales");
    assert_eq!(source.version, BlobId::of(SALES.as_bytes()));
    assert_eq!(source.plan.name, "Sales");
    assert_eq!(source.plan.sources().len(), 1, "the inner report's own");
    assert!(
        source.plan.outputs.is_empty(),
        "an inner report writes no file: {:?}",
        source.plan.outputs
    );
    assert!(source.plan.charts.is_empty());
    assert_eq!(
        plan.reports_used()
            .iter()
            .map(|used| used.report.as_str())
            .collect::<Vec<_>>(),
        ["sales"]
    );
}

#[test]
fn a_run_binds_the_inner_reports_parameters_and_shares_as_of() {
    let plan = plan(BY_CATEGORY, &[("sales", SALES)]).unwrap();
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .unwrap();
    let [PlannedSource::Report(source)] = plan.sources() else {
        panic!("{:?}", plan.sources());
    };
    let inner = source.values(&values).unwrap();
    assert_eq!(
        inner.parameters.get("since").cloned().flatten().as_deref(),
        Some("2026-10-01")
    );
    assert_eq!(
        inner.parameters.get("until").cloned().flatten().as_deref(),
        Some("2026-11-01")
    );
    assert_eq!(
        inner.parameters.get(AS_OF),
        values.parameters.get(AS_OF),
        "every report in a run reads the data as of one moment"
    );
}

#[test]
fn binds_a_literal_of_the_inner_parameters_type() {
    let outer = BY_CATEGORY.replace("until = \":until\"", "until = \"2026-11-01\"");
    let plan = plan(&outer, &[("sales", SALES)]).unwrap();
    let [PlannedSource::Report(source)] = plan.sources() else {
        panic!("{:?}", plan.sources());
    };
    assert_eq!(
        source.params,
        vec![
            BoundParam {
                name: "since".to_owned(),
                from: ParamSource::Param("since".to_owned()),
            },
            BoundParam {
                name: "until".to_owned(),
                from: ParamSource::Literal("2026-11-01".to_owned()),
            },
        ]
    );
}

#[test]
fn refuses_a_source_that_names_a_data_source_and_a_report() {
    let text = BY_CATEGORY.replace(
        "report = \"sales\"",
        "report = \"sales\"\ndata_source = \"shop\"",
    );
    let problems = read_problems(&text);
    assert!(
        says(
            &problems,
            "data_source names the data source a source reads"
        ),
        "{problems:?}"
    );
    assert!(says(&problems, "\"sales\""), "{problems:?}");
}

#[test]
fn refuses_a_report_source_with_the_keys_of_a_query() {
    let cases = [
        (
            "query = \"SELECT 1\"",
            "query is the query a data source answers",
        ),
        ("max_rows = 10", "a report source"),
        ("columnar = false", "columnar says how a query's rows"),
        ("attach = true", "attach attaches a DuckDB file"),
    ];
    for (key, expected) in cases {
        let text = BY_CATEGORY.replace("report = \"sales\"", &format!("report = \"sales\"\n{key}"));
        let problems = read_problems(&text);
        assert!(says(&problems, expected), "{key}: {problems:?}");
    }
}

#[test]
fn refuses_a_report_source_without_a_combine_step() {
    let text = "[report]\nname = \"R\"\n\n[[source]]\nid = \"sales\"\nreport = \"sales\"\n\
                params = { }\n\n[[output]]\nformat = \"csv\"\nfile = \"r.csv\"\n";
    let problems = read_problems(text);
    assert!(
        says(
            &problems,
            "a report source's rows are read by a [combine] query"
        ),
        "{problems:?}"
    );
}

#[test]
fn refuses_a_report_the_folder_doesnt_hold() {
    let problems = refused(BY_CATEGORY, &[]);
    assert!(
        says(
            &problems,
            "there is no report \"sales\" in the reports folder"
        ),
        "{problems:?}"
    );
    assert!(says(&problems, "source \"sales\""), "{problems:?}");
}

#[test]
fn a_cycle_of_two_reports_is_a_problem_in_both_files() {
    // Each reads the other's result.
    let first = r#"
[report]
name = "First"
params = { since = "date" }

[[source]]
id = "other"
report = "second"
params = { since = ":since" }

[combine]
query = "SELECT * FROM other"

[[output]]
format = "csv"
file = "r.csv"
"#;
    let second = first
        .replace("name = \"First\"", "name = \"Second\"")
        .replace("report = \"second\"", "report = \"first\"");
    let folder = [("first", first), ("second", second.as_str())];
    for (name, text) in folder {
        let problems = refused(text, &folder);
        assert!(
            says(&problems, "read each other in a circle"),
            "{name}: {problems:?}"
        );
        assert!(says(&problems, "first"), "{name}: {problems:?}");
        assert!(says(&problems, "second"), "{name}: {problems:?}");
    }
}

#[test]
fn refuses_a_report_that_reads_itself() {
    let itself = r#"
[report]
name = "Itself"

[[source]]
id = "again"
report = "itself"
params = { }

[combine]
query = "SELECT * FROM again"

[[output]]
format = "csv"
file = "r.csv"
"#;
    let problems = refused(itself, &[("itself", itself)]);
    assert!(
        says(&problems, "reads itself; a report can't be its own source"),
        "{problems:?}"
    );
}

#[test]
fn refuses_a_chain_of_reports_deeper_than_four() {
    // r1 reads r2, r2 reads r3, and so on; the last reads the shop.
    let link = |number: usize| {
        format!(
            "[report]\nname = \"R{number}\"\n\n[[source]]\nid = \"next\"\n\
             report = \"r{}\"\nparams = {{ }}\n\n[combine]\nquery = \"SELECT * FROM \
             next\"\n\n[[output]]\nformat = \"csv\"\nfile = \"r.csv\"\n",
            number + 1
        )
    };
    let last = "[report]\nname = \"Last\"\n\n[[source]]\nid = \"s\"\ndata_source = \"shop\"\n\
                query = \"SELECT 1 AS n\"\n\n[[output]]\nformat = \"csv\"\nfile = \"r.csv\"\n"
        .to_owned();
    let texts: Vec<String> = (1..=5).map(link).chain([last]).collect();
    let names = ["r1", "r2", "r3", "r4", "r5", "r6"];
    let folder: Vec<(&str, &str)> = names
        .into_iter()
        .zip(texts.iter().map(String::as_str))
        .collect();
    // Four reports below it plans: r1 to r4 and then the shop's own.
    let four = link(3).replace("R3", "Three below");
    assert!(plan(&four, &folder).is_ok(), "{:?}", plan(&four, &folder));
    // Five does not.
    let five = link(1).replace("R1", "Five below");
    let problems = refused(&five, &folder);
    assert!(says(&problems, "reports deep"), "{problems:?}");
    assert!(
        says(&problems, "at most 4 reports below it"),
        "{problems:?}"
    );
}

#[test]
fn refuses_each_parameter_bound_wrongly() {
    let cases = [
        (
            "params = { since = \":since\" }",
            "takes until, which nothing here gives it",
        ),
        (
            "params = { since = \":since\", until = \":until\", extra = 1 }",
            "has no parameter \"extra\"",
        ),
        (
            "params = { since = \":since\", until = \"not a date\" }",
            "the report \"sales\" takes until as date",
        ),
        (
            "params = { since = \":since\", until = \":missing\" }",
            "[report] params doesn't declare",
        ),
    ];
    for (params, expected) in cases {
        let text = BY_CATEGORY.replace(
            "params = { since = \":since\", until = \":until\" }",
            params,
        );
        let problems = refused(&text, &[("sales", SALES)]);
        assert!(says(&problems, expected), "{params}: {problems:?}");
    }
}

#[test]
fn refuses_a_parameter_bound_to_one_of_another_type() {
    let outer = BY_CATEGORY.replace(
        "params = { since = \"date\", until = \"date\" }",
        "params = { since = \"date\", until = \"text\" }",
    );
    let problems = refused(&outer, &[("sales", SALES)]);
    assert!(
        says(
            &problems,
            "is date in the report \"sales\" and :until is text here"
        ),
        "{problems:?}"
    );
}

#[test]
fn shows_a_problem_of_the_inner_report_in_the_outer_file() {
    let broken = SALES.replace("data_source = \"shop\"", "data_source = \"missing\"");
    let problems = refused(BY_CATEGORY, &[("sales", broken.as_str())]);
    assert!(says(&problems, "report \"sales\""), "{problems:?}");
    assert!(says(&problems, "missing"), "{problems:?}");
}

#[test]
fn refuses_an_inner_report_that_partitions_its_own_run() {
    let partitioned = r#"
[report]
name = "Sales"
params = { since = "date", until = "date" }
partition = { every = "1 month", from = ":since", to = ":until" }

[[source]]
id = "lines"
data_source = "shop"
query = "SELECT category, amount FROM lines WHERE ordered_on >= :part_from AND ordered_on < :part_to"

[combine]
query = "SELECT category, sum(amount) AS amount FROM lines GROUP BY category"

[[output]]
format = "csv"
file = "sales.csv"
"#;
    assert!(
        plan(partitioned, &[]).is_ok(),
        "the inner report is fine on its own: {:?}",
        plan(partitioned, &[])
    );
    let problems = refused(BY_CATEGORY, &[("sales", partitioned)]);
    assert!(says(&problems, "partitions its own run"), "{problems:?}");
}

#[test]
fn refuses_two_reports_attaching_different_files_under_one_id() {
    let attaching = |data_source: &str| {
        format!(
            "[report]\nname = \"A\"\n\n[[source]]\nid = \"prices\"\nattach = true\n\
             data_source = \"{data_source}\"\n\n[combine]\nquery = \"SELECT 1 AS n FROM \
             prices.hourly\"\n\n[[output]]\nformat = \"csv\"\nfile = \"a.csv\"\n"
        )
    };
    let inner = attaching("other-prices");
    let outer = "[report]\nname = \"Outer\"\n\n[[source]]\nid = \"prices\"\nattach = true\n\
                 data_source = \"prices\"\n\n[[source]]\nid = \"inner\"\nreport = \"inner\"\n\
                 params = { }\n\n[combine]\nquery = \"SELECT * FROM inner\"\n\n[[output]]\n\
                 format = \"csv\"\nfile = \"o.csv\"\n";
    let problems = Plan::new(
        read_report(outer).expect(outer),
        &[
            duckdb("prices", "/tmp/prices.duckdb"),
            duckdb("other-prices", "/tmp/other.duckdb"),
        ],
        &[],
        &named(&[("inner", inner.as_str())]),
        &LimitDefaults::default(),
    )
    .expect_err("two files under one id");
    assert!(
        says(
            &problems,
            "an attached file keeps its id across the whole run"
        ),
        "{problems:?}"
    );
    assert!(says(&problems, "the report \"inner\""), "{problems:?}");
}

#[test]
fn the_same_file_under_one_id_is_attached_once() {
    let inner = "[report]\nname = \"A\"\n\n[[source]]\nid = \"prices\"\nattach = true\n\
                 data_source = \"prices\"\n\n[combine]\nquery = \"SELECT 1 AS n FROM \
                 prices.hourly\"\n\n[[output]]\nformat = \"csv\"\nfile = \"a.csv\"\n";
    let outer = "[report]\nname = \"Outer\"\n\n[[source]]\nid = \"prices\"\nattach = true\n\
                 data_source = \"prices\"\n\n[[source]]\nid = \"inner\"\nreport = \"inner\"\n\
                 params = { }\n\n[combine]\nquery = \"SELECT * FROM inner\"\n\n[[output]]\n\
                 format = \"csv\"\nfile = \"o.csv\"\n";
    let plan = Plan::new(
        read_report(outer).expect(outer),
        &[duckdb("prices", "/tmp/prices.duckdb")],
        &[],
        &named(&[("inner", inner)]),
        &LimitDefaults::default(),
    );
    assert!(plan.is_ok(), "{plan:?}");
}

#[test]
fn a_text_value_cant_start_with_a_colon() {
    let outer = BY_CATEGORY.replace("until = \":until\"", "until = \":not a name\"");
    let problems = read_problems(&outer);
    assert!(
        says(&problems, "a text value can't start with a colon"),
        "{problems:?}"
    );
}

#[test]
fn a_report_source_takes_no_parameter_of_its_own_type() {
    let report = read_report(BY_CATEGORY).unwrap();
    let params: Vec<(&str, ColumnType)> = report
        .params
        .iter()
        .map(|param| (param.name.as_str(), param.param_type))
        .collect();
    assert_eq!(
        params,
        [("since", ColumnType::Date), ("until", ColumnType::Date)],
        "the outer report's parameters are its own"
    );
}
