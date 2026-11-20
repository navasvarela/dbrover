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

// Running a report source (RS9c): the inner report's sources load into a schema of their own,
// its combine makes the table the outer report reads, and only that result is visible.

use std::collections::VecDeque;
use std::path::Path;

use dbrover_core::Row;

use crate::run_tests::{MemoryOpener, MemoryReader, column};

fn rows(columns: &[&str], rows: &[&[Option<&str>]]) -> MemoryReader {
    let rows: Vec<Row> = rows
        .iter()
        .map(|row| row.iter().map(|value| value.map(str::to_owned)).collect())
        .collect();
    MemoryReader {
        columns: columns.iter().map(|name| column(name)).collect(),
        pages: VecDeque::from([rows]),
        then: None,
        scanned: None,
    }
}

/// The order lines the inner report reads, three categories over two days.
fn lines() -> MemoryReader {
    rows(
        &["category", "amount"],
        &[
            &[Some("Tools"), Some("10.00")],
            &[Some("Tools"), Some("2.50")],
            &[Some("Seeds"), Some("7.25")],
        ],
    )
}

/// The inner report, as the run reads it: one source and a combine that groups it.
const INNER: &str = r#"
[report]
name = "Sales"
params = { since = "date", until = "date" }

[[source]]
id = "lines"
data_source = "shop"
query = "SELECT category, amount FROM lines WHERE ordered_on >= :since AND ordered_on < :until"
columns = [
  { name = "category", type = "text" },
  { name = "amount",   type = "decimal(18,2)" },
]

[combine]
query = "SELECT category, sum(amount) AS amount FROM lines GROUP BY category"

[[output]]
format = "csv"
file = "sales.csv"
"#;

/// The outer report, reading the inner report's result as the table `sales`.
const OUTER: &str = r#"
[report]
name = "By category"
params = { since = "date", until = "date" }

[[source]]
id = "sales"
report = "sales"
params = { since = ":since", until = ":until" }

[combine]
query = "SELECT category, amount FROM sales ORDER BY category"

[[output]]
format = "csv"
file = "by-category.csv"
"#;

/// Runs `plan` over `opener` into `folder` for October.
async fn run_into(
    plan: &Plan,
    opener: &MemoryOpener,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .expect("the values");
    let staging = folder.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder,
        staging: &staging,
        work: &work,
    };
    let mut events = Vec::new();
    let outcome = run(plan, &values, opener, folders, &mut |event| {
        events.push(event);
    })
    .await;
    (outcome, events)
}

#[tokio::test]
async fn a_report_source_runs_the_report_it_names_and_reads_its_result() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(OUTER, &[("sales", INNER)]).unwrap();
    let opener = MemoryOpener::of(vec![("lines", lines())]);
    let (outcome, events) = run_into(&plan, &opener, folder.path()).await;
    let summary = outcome.expect("the run");
    assert_eq!(summary.rows, 2, "one row a category");
    let written = std::fs::read_to_string(folder.path().join("by-category.csv")).unwrap();
    assert_eq!(written, "category,amount\r\nSeeds,7.25\r\nTools,12.50\r\n");
    assert!(
        !folder.path().join("sales.csv").exists(),
        "the inner report writes no file"
    );
    // Its source's step is named by its path, under the outer source's lane.
    let steps: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::SourceDone { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert!(steps.contains(&"sales › lines".to_owned()), "{steps:?}");
    assert!(steps.contains(&"sales".to_owned()), "{steps:?}");
    // The inner report's statement was sent its period.
    let sent = opener.sent.borrow();
    let [(id, query)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(id, "lines");
    assert!(query.contains("ordered_on"), "{query}");
}

#[tokio::test]
async fn the_outer_combine_cannot_name_the_inner_reports_sources() {
    let folder = tempfile::tempdir().unwrap();
    let text = OUTER.replace(
        "SELECT category, amount FROM sales ORDER BY category",
        "SELECT category, amount FROM lines",
    );
    let plan = plan(&text, &[("sales", INNER)]).unwrap();
    let opener = MemoryOpener::of(vec![("lines", lines())]);
    let (outcome, _) = run_into(&plan, &opener, folder.path()).await;
    let failure = outcome.expect_err("the inner report's sources are its own");
    let said = failure.to_string();
    assert!(said.contains("lines"), "{said}");
}

#[tokio::test]
async fn an_inner_report_without_a_combine_gives_its_one_sources_rows() {
    let folder = tempfile::tempdir().unwrap();
    let inner = r#"
[report]
name = "Lines"
params = { since = "date", until = "date" }

[[source]]
id = "lines"
data_source = "shop"
query = "SELECT category, amount FROM lines WHERE ordered_on >= :since AND ordered_on < :until"
columns = [
  { name = "category", type = "text" },
  { name = "amount",   type = "decimal(18,2)" },
]

[[output]]
format = "csv"
file = "lines.csv"
"#;
    let outer = OUTER.replace(
        "SELECT category, amount FROM sales ORDER BY category",
        "SELECT category, count(*) AS lines FROM sales GROUP BY category ORDER BY category",
    );
    let plan = plan(&outer, &[("sales", inner)]).unwrap();
    let opener = MemoryOpener::of(vec![("lines", lines())]);
    let (outcome, _) = run_into(&plan, &opener, folder.path()).await;
    assert_eq!(outcome.expect("the run").rows, 2);
    let written = std::fs::read_to_string(folder.path().join("by-category.csv")).unwrap();
    assert_eq!(written, "category,lines\r\nSeeds,1\r\nTools,2\r\n");
}

#[tokio::test]
async fn two_reports_of_one_run_keep_their_sources_apart_under_one_id() {
    let folder = tempfile::tempdir().unwrap();
    // Both reports have a source called `lines`; the outer one's is its own.
    let outer = r#"
[report]
name = "Both"
params = { since = "date", until = "date" }

[[source]]
id = "lines"
data_source = "shop"
query = "SELECT category, amount FROM other WHERE ordered_on >= :since AND ordered_on < :until"
columns = [
  { name = "category", type = "text" },
  { name = "amount",   type = "decimal(18,2)" },
]

[[source]]
id = "sales"
report = "sales"
params = { since = ":since", until = ":until" }

[combine]
query = """
SELECT (SELECT count(*) FROM lines) AS mine, (SELECT count(*) FROM sales) AS theirs
"""

[[output]]
format = "csv"
file = "both.csv"
"#;
    let plan = plan(outer, &[("sales", INNER)]).unwrap();
    let mine = rows(&["category", "amount"], &[&[Some("Pots"), Some("1.00")]]);
    let opener = MemoryOpener::of(vec![("lines", mine), ("lines", lines())]);
    let (outcome, _) = run_into(&plan, &opener, folder.path()).await;
    outcome.expect("the run");
    let written = std::fs::read_to_string(folder.path().join("both.csv")).unwrap();
    assert_eq!(
        written, "mine,theirs\r\n1,2\r\n",
        "the outer report's one row, and the inner report's two categories"
    );
}

#[tokio::test]
async fn a_report_three_deep_runs_innermost_first() {
    let folder = tempfile::tempdir().unwrap();
    let middle = r#"
[report]
name = "Middle"
params = { since = "date", until = "date" }

[[source]]
id = "sales"
report = "sales"
params = { since = ":since", until = ":until" }

[combine]
query = "SELECT category, amount * 2 AS amount FROM sales"

[[output]]
format = "csv"
file = "middle.csv"
"#;
    let outer = OUTER.replace("report = \"sales\"", "report = \"middle\"");
    let plan = plan(&outer, &[("middle", middle), ("sales", INNER)]).unwrap();
    let opener = MemoryOpener::of(vec![("lines", lines())]);
    let (outcome, events) = run_into(&plan, &opener, folder.path()).await;
    outcome.expect("the run");
    let written = std::fs::read_to_string(folder.path().join("by-category.csv")).unwrap();
    assert_eq!(
        written, "category,amount\r\nSeeds,14.50\r\nTools,25.00\r\n",
        "each category's sum, doubled by the middle report"
    );
    let steps: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::SourceDone { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let at = |id: &str| steps.iter().position(|step| step == id);
    assert!(
        at("sales › sales") < at("sales"),
        "the innermost combine first: {steps:?}"
    );
}

/// RS9c: explaining a composed report explains each report's sources under its own path, and
/// plans the outer combine over the table the inner report's own combine leaves behind.
#[tokio::test]
async fn explains_every_reports_sources_and_plans_the_outer_combine() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(OUTER, &[("sales", INNER)]).unwrap();
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .unwrap();
    let explanation = crate::explain(
        &plan,
        &values,
        &crate::combine_tests::EchoExplainer,
        &[],
        &folder.path().join("spill"),
    )
    .await;
    let ids: Vec<&str> = explanation
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect();
    assert_eq!(ids, ["sales › lines"], "the inner report's own source");
    let Some(CombineExplanation::Planned { text }) = &explanation.combine else {
        panic!("{:?}", explanation.combine);
    };
    let combine = dbrover_plan::PlanFormat::DuckDbJson.parse(text).unwrap();
    let reads = combine.table_reads();
    assert!(
        reads.iter().any(|read| read.table == "sales"),
        "the outer combine reads the inner report's result: {text}"
    );
}

/// RS9d: the run's start names every report it used, each with the blob id of the text that
/// ran, so a run says which version of a shared report it read.
#[test]
fn a_runs_start_names_every_report_it_used() {
    let middle = r#"
[report]
name = "Middle"
params = { since = "date", until = "date" }

[[source]]
id = "sales"
report = "sales"
params = { since = ":since", until = ":until" }

[combine]
query = "SELECT * FROM sales"

[[output]]
format = "csv"
file = "middle.csv"
"#;
    let outer = OUTER.replace("report = \"sales\"", "report = \"middle\"");
    let plan = plan(&outer, &[("middle", middle), ("sales", INNER)]).unwrap();
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .unwrap();
    let own = BlobId::of(outer.as_bytes());
    let started = plan.started(
        &values,
        own,
        Machine {
            cores: 8,
            dbrover: "0.36.0".to_owned(),
        },
    );
    let RunEvent::Started {
        version, reports, ..
    } = started
    else {
        panic!("{started:?}");
    };
    assert_eq!(version, Some(own), "the report's own file");
    assert_eq!(
        reports,
        vec![
            UsedReport {
                name: "middle".to_owned(),
                version: BlobId::of(middle.as_bytes()),
            },
            UsedReport {
                name: "sales".to_owned(),
                version: BlobId::of(INNER.as_bytes()),
            },
        ],
        "outermost first"
    );
}
