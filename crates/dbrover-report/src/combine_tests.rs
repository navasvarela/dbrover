//! Runs with a combine step, on sources in memory and a DuckDB file: what they load, what they
//! write to the last digit, what they report, and each of DuckDB's limits stopping one.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use calamine::{Data, Reader, Xlsx, open_workbook};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, Row,
    SafetyLevel, SqliteConfig,
};
use dbrover_export::ExportColumn;

use super::*;
use crate::run_tests::{MemoryOpener, MemoryReader, column, files_in};

fn data_source(name: &str, config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config,
    }
}

fn ledger() -> DataSource {
    data_source(
        "ledger",
        ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/ledger.db"),
            read_only: false,
        }),
    )
}

fn prices_file(path: &Path) -> DataSource {
    data_source(
        "prices",
        ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path: path.to_owned(),
            read_only: true,
        })),
    )
}

/// A DuckDB file at `path`, made by `statements`.
async fn duckdb_file(path: &Path, statements: &[&str]) {
    dbrover_driver_duckdb::create_database(path).await.unwrap();
    let mut connection = dbrover_driver_duckdb::connect(&DuckDbConfig::new(DuckDbDatabase::File {
        path: path.to_owned(),
        read_only: false,
    }))
    .await
    .unwrap();
    for statement in statements {
        connection
            .execute(statement)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
    }
    Box::new(connection).close().await.unwrap();
}

fn reader(columns: &[&str], rows: &[&[Option<&str>]]) -> MemoryReader {
    let rows: Vec<Row> = rows
        .iter()
        .map(|row| row.iter().map(|value| value.map(str::to_owned)).collect())
        .collect();
    MemoryReader {
        columns: columns.iter().map(|name| column(name)).collect(),
        pages: VecDeque::from([rows]),
        then: None,
    }
}

/// Transactions with 18-decimal quantities, as PostgreSQL's text protocol hands them over.
fn transactions() -> MemoryReader {
    reader(
        &["id", "asset", "quantity", "executed_at"],
        &[
            &[
                Some("1"),
                Some("BTC"),
                Some("1.234567890123456789"),
                Some("2026-10-01 10:15:00"),
            ],
            &[
                Some("2"),
                Some("BTC"),
                Some("0.000000000000000001"),
                Some("2026-10-01 11:30:00"),
            ],
            &[
                Some("3"),
                Some("ETH"),
                Some("250.5"),
                Some("2026-10-02 09:00:00"),
            ],
        ],
    )
}

fn prices() -> MemoryReader {
    reader(
        &["asset", "price"],
        &[
            &[Some("BTC"), Some("61234.12345678")],
            &[Some("ETH"), Some("2345.6789")],
        ],
    )
}

const TWO_SOURCES: &str = r##"
[report]
name = "Values"
params = { since = "date" }

[[source]]
id = "tx"
data_source = "ledger"
query = "SELECT * FROM tx WHERE executed_at >= :since"
{tx_limits}

[[source]]
id = "prices"
data_source = "ledger"
query = "SELECT * FROM prices"

[combine]
query = """
{query}
"""
threads = 2
{combine_limits}

[[output]]
format = "csv"
file = "values.csv"
columns = [
  { name = "Id",       from = "id" },
  { name = "Quantity", from = "quantity", type = "decimal" },
  { name = "Value",    from = "value",    type = "decimal" },
]

[[output]]
format = "xlsx"
file = "values.xlsx"
columns = [
  { name = "Id",    from = "id",    type = "integer" },
  { name = "Value", from = "value", type = "decimal(38,2)", format = "#,##0.00" },
]
"##;

const VALUES: &str = "SELECT tx.id, tx.quantity, tx.quantity * prices.price AS value \
                      FROM tx JOIN prices USING (asset) \
                      WHERE tx.executed_at >= CAST(:since AS DATE) ORDER BY tx.id";

fn report(tx_limits: &str, query: &str, combine_limits: &str) -> Plan {
    let text = TWO_SOURCES
        .replace("{tx_limits}", tx_limits)
        .replace("{query}", query)
        .replace("{combine_limits}", combine_limits);
    Plan::new(
        read_report(&text).unwrap(),
        &[ledger()],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap()
}

fn october(plan: &Plan) -> RunValues {
    plan.values(
        &[("since".to_owned(), "2026-10-01".to_owned())]
            .into_iter()
            .collect(),
    )
    .unwrap()
}

async fn run_with(
    plan: &Plan,
    opener: &MemoryOpener,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let mut events = Vec::new();
    let staging = folder.join(".run");
    let folders = RunFolders {
        folder,
        staging: &staging,
    };
    let outcome = run(plan, &october(plan), opener, folders, &mut |event| {
        events.push(event);
    })
    .await;
    assert!(!staging.exists(), "the run leaves its staging folder");
    (outcome, events)
}

/// [`VALUES`] in PRQL, with its parameter in PRQL's form.
const VALUES_IN_PRQL: &str = "from tx
join prices (==asset)
filter tx.executed_at >= ($since | as date)
select {tx.id, tx.quantity, value = tx.quantity * prices.price}
sort tx.id";

#[tokio::test]
async fn a_prql_combine_writes_what_its_sql_twin_writes() {
    let mut written = Vec::new();
    for prql in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let plan = if prql {
            let text = TWO_SOURCES
                .replace("{tx_limits}", "")
                .replace(
                    "query = \"\"\"\n{query}",
                    &format!("prql = \"\"\"\n{VALUES_IN_PRQL}"),
                )
                .replace("{combine_limits}", "");
            Plan::new(
                read_report(&text).unwrap(),
                &[ledger()],
                &[],
                &LimitDefaults::default(),
            )
            .unwrap()
        } else {
            report("", VALUES, "")
        };
        let Logic::Combine(combined) = &plan.logic else {
            panic!("{:?}", plan.logic);
        };
        assert_eq!(combined.combine.query.compiled().is_some(), prql);
        assert_eq!(plan.params.len(), 1, "{:?}", plan.params);
        let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
        let (outcome, _) = run_with(&plan, &opener, folder.path()).await;
        assert_eq!(outcome.unwrap().rows, 3);
        written.push(std::fs::read_to_string(folder.path().join("values.csv")).unwrap());
    }
    assert_eq!(written[0], written[1]);
}

#[tokio::test]
async fn combines_two_sources_and_writes_every_digit() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
    let (outcome, events) = run_with(&plan, &opener, folder.path()).await;
    let summary = outcome.unwrap();
    assert_eq!(summary.rows, 3);
    assert_eq!(files_in(folder.path()), ["values.csv", "values.xlsx"]);
    assert_eq!(
        std::fs::read_to_string(folder.path().join("values.csv")).unwrap(),
        "Id,Quantity,Value\r\n\
         1,1.234567890123456789,75597.68259959615905340363907942\r\n\
         2,0.000000000000000001,0.00000000000006123412345678\r\n\
         3,250.500000000000000000,587592.56445000000000000000000000\r\n"
    );
    let mut book: Xlsx<_> = open_workbook(folder.path().join("values.xlsx")).unwrap();
    let range = book.worksheet_range("Rows").unwrap();
    assert_eq!(range.get((1, 1)), Some(&Data::Float(75597.68)));
    assert_eq!(range.get((3, 1)), Some(&Data::Float(587_592.56)));

    let loaded: Vec<(&str, Vec<(&str, &str)>)> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::SourceDone { id, table, .. } => Some((
                id.as_str(),
                table
                    .iter()
                    .map(|column| (column.name.as_str(), column.type_name.as_str()))
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(loaded.len(), 2, "{events:#?}");
    let tx = loaded
        .iter()
        .find(|(id, _)| *id == "tx")
        .map(|(_, table)| table.clone());
    assert_eq!(
        tx,
        Some(vec![
            ("id", "BIGINT"),
            ("asset", "VARCHAR"),
            ("quantity", "DECIMAL(21,18)"),
            ("executed_at", "TIMESTAMP"),
        ])
    );
    let last: Vec<&str> = events
        .iter()
        .skip_while(|event| !matches!(event, RunEvent::CombineDone { .. }))
        .map(|event| match event {
            RunEvent::CombineDone { .. } => "combine done",
            RunEvent::Preview(_) => "preview",
            RunEvent::OutputWritten(_) => "written",
            other => panic!("{other:?} after the combine step"),
        })
        .collect();
    assert_eq!(last, ["combine done", "preview", "written", "written"]);
}

#[tokio::test]
async fn reads_an_attached_duckdb_file_in_place() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("prices.duckdb");
    duckdb_file(
        &path,
        &[
            "CREATE TABLE hourly (asset VARCHAR, price DECIMAL(13,8))",
            "INSERT INTO hourly VALUES ('BTC', 61234.12345678), ('ETH', 2345.6789)",
        ],
    )
    .await;
    let text = TWO_SOURCES
        .replace("{tx_limits}", "")
        .replace(
            "data_source = \"ledger\"\nquery = \"SELECT * FROM prices\"",
            "data_source = \"prices\"\nattach = true",
        )
        .replace(
            "{query}",
            &VALUES.replace(
                "JOIN prices USING (asset)",
                "JOIN prices.hourly prices USING (asset)",
            ),
        )
        .replace("{combine_limits}", "");
    let plan = Plan::new(
        read_report(&text).unwrap(),
        &[ledger(), prices_file(&path)],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let out = folder.path().join("out");
    let opener = MemoryOpener::of(vec![("tx", transactions())]);
    let (outcome, events) = run_with(&plan, &opener, &out).await;
    outcome.unwrap();
    assert!(
        std::fs::read_to_string(out.join("values.csv"))
            .unwrap()
            .contains("1,1.234567890123456789,75597.68259959615905340363907942\r\n")
    );
    assert!(events.contains(&RunEvent::SourceAttached {
        id: "prices".to_owned(),
        path,
    }));
}

/// A combine query that runs for minutes, and one that holds far more than the limits below
/// in memory, and can't spill it.
const SLOW: &str = "SELECT count(*) AS id, 1 AS quantity, 1 AS value \
                    FROM range(100000000) a, range(100000) b";
const UNSPILLABLE: &str = "SELECT 1 AS id, 1 AS quantity, len(list(range)) AS value \
                           FROM range(20000000)";
const SPILLS: &str = "SELECT range AS id, repeat('x', 100) AS quantity, 1 AS value \
                      FROM range(1000000) ORDER BY hash(range)";

/// Each limit a run with a combine step can hit, the key that raises it, and that the folder
/// is left as it was.
#[tokio::test]
async fn stops_at_each_limit_and_leaves_no_file() {
    let cases = [
        (
            "max_rows = 2",
            VALUES,
            "",
            "max_rows",
            "the [[source]] \"tx\"",
        ),
        ("", SLOW, "timeout = \"1s\"", "timeout", "[combine]"),
        (
            "",
            UNSPILLABLE,
            "memory_limit = \"32 MiB\"",
            "memory_limit",
            "[combine]",
        ),
        (
            "",
            SPILLS,
            "memory_limit = \"64 MiB\"\nmax_temp_size = \"2 MiB\"",
            "max_temp_size",
            "[combine]",
        ),
    ];
    for (tx_limits, query, combine_limits, key, raise_in) in cases {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("values.csv"), "last month's").unwrap();
        let plan = report(tx_limits, query, combine_limits);
        let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
        let (outcome, _) = run_with(&plan, &opener, folder.path()).await;
        let failure = outcome.unwrap_err();
        let limit = failure.limit.clone().unwrap_or_else(|| panic!("{failure}"));
        assert_eq!(
            (limit.key.as_str(), limit.raise_in.as_str()),
            (key, raise_in)
        );
        assert_eq!(files_in(folder.path()), ["values.csv"], "{key}");
    }
}

/// A combine query whose window sorts far more than the 64 MiB below holds, returning one row.
const SORTS_PAST_MEMORY: &str = "SELECT max(rn) AS id, 1 AS quantity, 1 AS value FROM ( \
                                 SELECT row_number() OVER (ORDER BY hash(range), \
                                 repeat('x', 100) || range) AS rn FROM range(1000000))";

/// DuckDB's peaks in `combine_done`: memory always, and a spill only past `memory_limit`.
#[tokio::test]
async fn says_how_much_memory_and_disk_the_combine_step_used() {
    let cases = [
        ("", VALUES, false),
        ("memory_limit = \"64 MiB\"", SORTS_PAST_MEMORY, true),
    ];
    for (combine_limits, query, spills) in cases {
        let folder = tempfile::tempdir().unwrap();
        let plan = report("", query, combine_limits);
        let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
        let (outcome, events) = run_with(&plan, &opener, folder.path()).await;
        outcome.unwrap();
        let peaks = events.iter().find_map(|event| match event {
            RunEvent::CombineDone {
                peak_memory,
                peak_temp_size,
                ..
            } => Some((*peak_memory, *peak_temp_size)),
            _ => None,
        });
        let Some((Some(memory), Some(temp_size))) = peaks else {
            panic!("{query}: {peaks:?}");
        };
        assert!(memory > 0, "{query}");
        assert_eq!(temp_size > 0, spills, "{query}: {temp_size} bytes spilled");
    }
}

/// The combine query's plan in `combine_done`, each source's scan with the rows it passed on,
/// and each loaded column's bytes in `source_done` (RS2c).
#[tokio::test]
async fn reports_the_combines_plan_and_each_columns_bytes() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
    let (outcome, events) = run_with(&plan, &opener, folder.path()).await;
    outcome.unwrap();
    let text = events
        .iter()
        .find_map(|event| match event {
            RunEvent::CombineDone { plan, .. } => plan.clone(),
            _ => None,
        })
        .unwrap();
    let combine = dbrover_plan::PlanFormat::DuckDbJson.parse(&text).unwrap();
    let reads = combine.table_reads();
    let tx = reads.iter().find(|read| read.table == "tx").unwrap();
    assert!(tx.columns.contains(&"quantity".to_owned()), "{text}");
    assert!(tx.rows.is_some(), "{text}");
    let tx_columns = events
        .iter()
        .find_map(|event| match event {
            RunEvent::SourceDone { id, table, .. } if id == "tx" => Some(table.clone()),
            _ => None,
        })
        .unwrap();
    let bytes: Vec<(&str, Option<u64>)> = tx_columns
        .iter()
        .map(|column| (column.name.as_str(), column.bytes))
        .collect();
    // "1", "2", "3"; "BTC", "BTC", "ETH"; the quantities; three times of 19 characters.
    assert_eq!(
        bytes,
        [
            ("id", Some(3)),
            ("asset", Some(9)),
            ("quantity", Some(20 + 20 + 5)),
            ("executed_at", Some(57)),
        ]
    );
}

#[tokio::test]
async fn names_the_source_that_fails_to_load_and_the_failing_query() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let mut broken = transactions();
    broken.then = Some(SourceError::new("the server went away"));
    let opener = MemoryOpener::of(vec![("tx", broken), ("prices", prices())]);
    let (outcome, _) = run_with(&plan, &opener, folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "tx".to_owned()
        }
    );
    assert_eq!(failure.message, "the server went away");

    let plan = report("", "SELECT id, quantity, nothing AS value FROM tx", "");
    let opener = MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]);
    let (outcome, _) = run_with(&plan, &opener, folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(failure.step, Step::Combine);
    assert!(failure.message.contains("nothing"), "{failure}");
    assert_eq!(files_in(folder.path()), Vec::<String>::new());
}

/// A reader that takes `first` before its first page and `later` before each other, and says
/// opening it took `opening`, as a slow database would.
struct Paced {
    inner: MemoryReader,
    opening: Opening,
    first: Duration,
    later: Duration,
    pages_read: usize,
}

impl SourceReader for Paced {
    fn columns(&self) -> &[ExportColumn] {
        self.inner.columns()
    }

    fn opening(&self) -> Opening {
        self.opening
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        let pause = if self.pages_read == 0 {
            self.first
        } else {
            self.later
        };
        tokio::time::sleep(pause).await;
        self.pages_read += 1;
        self.inner.next_page().await
    }

    async fn close(self) {}
}

/// Paces every source of a [`MemoryOpener`] the same way.
struct PacedOpener {
    inner: MemoryOpener,
    opening: Opening,
    first: Duration,
    later: Duration,
}

impl SourceOpener for PacedOpener {
    type Reader = Paced;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<Paced, SourceError> {
        Ok(Paced {
            inner: self.inner.open(statement).await?,
            opening: self.opening,
            first: self.first,
            later: self.later,
            pages_read: 0,
        })
    }
}

#[tokio::test]
async fn says_where_each_sources_time_went() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let mut tx = transactions();
    // Two pages, so the second is a transfer, and an empty end.
    let rows = tx.pages.pop_front().unwrap();
    tx.pages = VecDeque::from([rows[..1].to_vec(), rows[1..].to_vec()]);
    let opener = PacedOpener {
        inner: MemoryOpener::of(vec![("tx", tx), ("prices", prices())]),
        opening: Opening {
            connect: Duration::from_millis(7),
            start: Duration::from_millis(11),
        },
        first: Duration::from_millis(60),
        later: Duration::from_millis(30),
    };
    let staging = folder.path().join(".run");
    let folders = RunFolders {
        folder: folder.path(),
        staging: &staging,
    };
    let mut events = Vec::new();
    let plan_values = october(&plan);
    run(&plan, &plan_values, &opener, folders, &mut |event| {
        events.push(event);
    })
    .await
    .unwrap();
    let phases = |id: &str| {
        events
            .iter()
            .find_map(|event| match event {
                RunEvent::SourceDone {
                    id: done, phases, ..
                } if done == id => *phases,
                _ => None,
            })
            .unwrap()
    };
    let tx = phases("tx");
    assert_eq!(tx.connect_ms, 7, "{tx:?}");
    // The statement's start, then its first page.
    assert!(tx.server_ms >= 11 + 60, "{tx:?}");
    // The second page, then the end of the rows.
    assert!(tx.transfer_ms >= 30 + 30, "{tx:?}");
    let query_ms = events.iter().find_map(|event| match event {
        RunEvent::CombineDone { query_ms, .. } => *query_ms,
        _ => None,
    });
    assert!(query_ms.is_some());
}

#[tokio::test]
async fn says_each_second_what_a_source_waits_on_before_its_first_row() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let opener = PacedOpener {
        inner: MemoryOpener::of(vec![("tx", transactions()), ("prices", prices())]),
        opening: Opening::default(),
        // Past a progress event's second, with no row yet.
        first: Duration::from_millis(1_300),
        later: Duration::ZERO,
    };
    let staging = folder.path().join(".run");
    let folders = RunFolders {
        folder: folder.path(),
        staging: &staging,
    };
    let mut events = Vec::new();
    let plan_values = october(&plan);
    run(&plan, &plan_values, &opener, folders, &mut |event| {
        events.push(event);
    })
    .await
    .unwrap();
    let waits: Vec<(String, u64, Busy)> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::Progress {
                step: Step::Source { id },
                rows,
                busy: Some(busy),
                ..
            } => Some((id.clone(), *rows, *busy)),
            _ => None,
        })
        .collect();
    // Both sources of the round wait at once, each saying so with no row read.
    assert!(
        waits
            .iter()
            .any(|(id, rows, busy)| id == "tx" && *rows == 0 && busy.waiting_pct >= 90),
        "{waits:?}"
    );
    assert_eq!(
        waits[0]
            .2
            .describe("PostgreSQL")
            .as_deref()
            .map(|text| text.starts_with("waiting on PostgreSQL")),
        Some(true)
    );
}

/// Explains each source as its statement, with nothing to plan it with.
struct EchoExplainer;

impl SourceExplainer for EchoExplainer {
    async fn explain(&self, statement: &SourceStatement<'_>) -> SourceExplained {
        SourceExplained {
            statement: statement.query.clone(),
            plan: ExplainedPlan::Text {
                text: format!("explained {}", statement.source.id),
            },
            estimate: None,
        }
    }
}

fn loaded(id: &str, columns: &[(&str, &str)]) -> KnownTable {
    KnownTable {
        id: id.to_owned(),
        columns: columns
            .iter()
            .map(|(name, type_name)| LoadedColumn {
                name: (*name).to_owned(),
                type_name: (*type_name).to_owned(),
                declared: false,
                bytes: None,
            })
            .collect(),
    }
}

/// RS2d: each source explained by its explainer, and the combine planned over empty tables of
/// the columns the last run loaded, with what it reads of each.
#[tokio::test]
async fn explains_each_source_and_plans_the_combine_over_the_last_runs_tables() {
    let folder = tempfile::tempdir().unwrap();
    let plan = report("", VALUES, "");
    let known = [
        loaded(
            "tx",
            &[
                ("id", "BIGINT"),
                ("asset", "VARCHAR"),
                ("quantity", "DECIMAL(38,18)"),
                ("executed_at", "TIMESTAMP"),
            ],
        ),
        loaded(
            "prices",
            &[("asset", "VARCHAR"), ("price", "DECIMAL(18,8)")],
        ),
    ];
    let explanation = explain(
        &plan,
        &october(&plan),
        &EchoExplainer,
        &known,
        &folder.path().join("spill"),
    )
    .await;
    let ids: Vec<&str> = explanation
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect();
    assert_eq!(ids, ["tx", "prices"]);
    assert_eq!(
        explanation.sources[0].explained.statement,
        "SELECT * FROM tx WHERE executed_at >= :since"
    );
    let Some(CombineExplanation::Planned { text }) = &explanation.combine else {
        panic!("{:?}", explanation.combine);
    };
    let combine = dbrover_plan::PlanFormat::DuckDbJson.parse(text).unwrap();
    assert!(!combine.is_analysed());
    let reads = combine.table_reads();
    let tx = reads.iter().find(|read| read.table == "tx").expect(text);
    assert!(tx.columns.contains(&"quantity".to_owned()), "{text}");

    let unknown = explain(
        &plan,
        &october(&plan),
        &EchoExplainer,
        &known[1..],
        &folder.path().join("spill"),
    )
    .await;
    let Some(CombineExplanation::Unknown { reason }) = unknown.combine else {
        panic!("{:?}", unknown.combine);
    };
    assert!(
        reason.starts_with("Source tx declares no columns"),
        "{reason}"
    );
}
