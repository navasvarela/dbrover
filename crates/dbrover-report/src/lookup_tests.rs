//! Runs with lookups and declared columns (`docs/reporting.md`, RP3d), on sources in memory:
//! the statements each batch sends, the table its batches make, an empty lookup, the caps and
//! the deadline across batches, and documents taken into declared columns.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, MongoConfig, ResultShape, Row, SafetyLevel,
    ServerConfig, SqliteConfig, TlsMode,
};
use dbrover_export::ExportColumn;

use super::*;
use crate::run_tests::{MemoryReader, column, files_in};

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

fn data_sources() -> Vec<DataSource> {
    vec![
        data_source(
            "ledger",
            ConnectionConfig::Sqlite(SqliteConfig {
                path: PathBuf::from("/tmp/ledger.db"),
                read_only: false,
            }),
        ),
        data_source(
            "crm",
            ConnectionConfig::Mongo(MongoConfig {
                server: ServerConfig {
                    host: "localhost".to_owned(),
                    port: 27017.into(),
                    database: "crm".to_owned(),
                    user: String::new(),
                    tls: TlsMode::Disable,
                    ssh: None,
                    password_command: None,
                    cloud_sign_in: None,
                },
                auth_database: String::new(),
                connection_string: None,
            }),
        ),
    ]
}

/// A reader of rows or of documents.
struct TestReader {
    inner: MemoryReader,
    shape: ResultShape,
}

impl SourceReader for TestReader {
    fn columns(&self) -> &[ExportColumn] {
        self.inner.columns()
    }

    fn shape(&self) -> ResultShape {
        self.shape
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        self.inner.next_page().await
    }

    async fn close(self) {}
}

fn rows(columns: &[&str], pages: Vec<Vec<Row>>) -> TestReader {
    TestReader {
        inner: MemoryReader {
            columns: columns.iter().map(|name| column(name)).collect(),
            pages: VecDeque::from(pages),
            then: None,
        },
        shape: ResultShape::Rows,
    }
}

fn documents(documents: &[&str]) -> TestReader {
    TestReader {
        inner: MemoryReader {
            columns: vec![column("document")],
            pages: VecDeque::from([documents
                .iter()
                .map(|document| vec![Some((*document).to_owned())])
                .collect()]),
            then: None,
        },
        shape: ResultShape::Documents,
    }
}

/// A statement as the opener was sent it.
#[derive(Debug, Clone)]
struct Sent {
    id: String,
    query: String,
    deadline: Instant,
}

/// Hands over each source's readers in order, one a statement, and keeps what it was sent.
#[derive(Default)]
struct Scripted {
    readers: RefCell<Vec<(String, TestReader)>>,
    sent: RefCell<Vec<Sent>>,
}

impl Scripted {
    fn of(readers: Vec<(&str, TestReader)>) -> Self {
        Self {
            readers: RefCell::new(
                readers
                    .into_iter()
                    .map(|(id, reader)| (id.to_owned(), reader))
                    .collect(),
            ),
            sent: RefCell::default(),
        }
    }

    fn sent_by(&self, id: &str) -> Vec<Sent> {
        self.sent
            .borrow()
            .iter()
            .filter(|sent| sent.id == id)
            .cloned()
            .collect()
    }
}

impl SourceOpener for Scripted {
    type Reader = TestReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<TestReader, SourceError> {
        let id = &statement.source.id;
        self.sent.borrow_mut().push(Sent {
            id: id.clone(),
            query: statement.query.clone(),
            deadline: statement.deadline,
        });
        let mut readers = self.readers.borrow_mut();
        let found = readers.iter().position(|(source, _)| source == id);
        found
            .map(|index| readers.remove(index).1)
            .ok_or_else(|| SourceError::new(format!("no more rows for {id:?}")))
    }
}

fn plan(text: &str) -> Plan {
    Plan::new(
        read_report(text).unwrap(),
        &data_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap()
}

async fn run_plan(
    plan: &Plan,
    values: &[(&str, &str)],
    opener: &Scripted,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let values = plan
        .values(
            &values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
        .unwrap();
    let staging = folder.join(".run");
    let mut events = Vec::new();
    let outcome = run(
        plan,
        &values,
        opener,
        RunFolders {
            folder,
            staging: &staging,
        },
        &mut |event| events.push(event),
    )
    .await;
    assert!(!staging.exists(), "the run leaves its staging folder");
    (outcome, events)
}

fn csv(folder: &Path, file: &str) -> String {
    std::fs::read_to_string(folder.join(file)).unwrap()
}

fn cells(values: &[&str]) -> Row {
    values
        .iter()
        .map(|value| Some((*value).to_owned()))
        .collect()
}

/// Accounts in one source, and the balances of those accounts in another, read by them.
const BALANCES: &str = r#"
[report]
name = "Balances"

[[source]]
id = "accounts"
data_source = "ledger"
query = "SELECT id FROM accounts"

[[source]]
id = "balances"
data_source = "ledger"
lookup = { from = "accounts", column = "id", as = "ids" {batch_size} }
query = "SELECT account, balance FROM balances WHERE account IN :ids"
{balances}

[combine]
query = "SELECT count(*) AS accounts, sum(balance) AS total FROM balances"

[[output]]
format = "csv"
file = "total.csv"
"#;

fn balances(batch_size: &str, more: &str) -> Plan {
    plan(
        &BALANCES
            .replace("{batch_size}", batch_size)
            .replace("{balances}", more),
    )
}

/// `count` accounts, numbered from 1, in pages of 1,000.
fn accounts(count: usize) -> TestReader {
    let ids: Vec<Row> = (1..=count).map(|n| cells(&[&n.to_string()])).collect();
    rows(&["id"], ids.chunks(1_000).map(<[Row]>::to_vec).collect())
}

/// The balances of accounts `from` to `to`, each 0.5.
fn balance_rows(from: usize, to: usize) -> TestReader {
    let rows_read: Vec<Row> = (from..=to)
        .map(|n| cells(&[&n.to_string(), "0.5"]))
        .collect();
    rows(&["account", "balance"], vec![rows_read])
}

#[tokio::test]
async fn reads_a_lookup_of_2500_values_in_three_batches_into_one_table() {
    let folder = tempfile::tempdir().unwrap();
    let plan = balances("", "");
    let opener = Scripted::of(vec![
        ("accounts", accounts(2_500)),
        ("balances", balance_rows(1, 1_000)),
        ("balances", balance_rows(1_001, 2_000)),
        ("balances", balance_rows(2_001, 2_500)),
    ]);
    let (outcome, events) = run_plan(&plan, &[], &opener, folder.path()).await;
    outcome.unwrap();
    assert_eq!(
        csv(folder.path(), "total.csv"),
        "accounts,total\r\n2500,1250.0\r\n"
    );

    let sent = opener.sent_by("balances");
    assert_eq!(sent.len(), 3);
    let counts: Vec<usize> = sent
        .iter()
        .map(|sent| {
            let (_, list) = sent.query.split_once("IN (").unwrap();
            list.matches(", ").count() + 1
        })
        .collect();
    assert_eq!(counts, [1_000, 1_000, 500]);
    assert!(
        sent[0]
            .query
            .starts_with("SELECT account, balance FROM balances WHERE account IN (1, 2, 3, "),
        "numbers go in unquoted and in order: {}",
        &sent[0].query[..80]
    );
    assert!(sent[2].query.ends_with(", 2499, 2500)"));
    assert!(
        sent.iter().all(|batch| batch.deadline == sent[0].deadline),
        "the source's timeout runs across its batches"
    );
    let done = events.iter().find_map(|event| match event {
        RunEvent::SourceDone {
            id, rows, batches, ..
        } if id == "balances" => Some((*rows, *batches)),
        _ => None,
    });
    assert_eq!(done, Some((2_500, Some(3))));
    let accounts_done = events.iter().find_map(|event| match event {
        RunEvent::SourceDone { id, batches, .. } if id == "accounts" => Some(*batches),
        _ => None,
    });
    assert_eq!(
        accounts_done,
        Some(None),
        "a source without a lookup runs no batches"
    );
}

#[tokio::test]
async fn a_lookup_with_batch_size_sends_smaller_batches() {
    let folder = tempfile::tempdir().unwrap();
    let plan = balances(", batch_size = 2", "");
    let opener = Scripted::of(vec![
        ("accounts", accounts(5)),
        ("balances", balance_rows(1, 2)),
        ("balances", balance_rows(3, 4)),
        ("balances", balance_rows(5, 5)),
    ]);
    let (outcome, _) = run_plan(&plan, &[], &opener, folder.path()).await;
    outcome.unwrap();
    let queries: Vec<String> = opener
        .sent_by("balances")
        .into_iter()
        .map(|sent| sent.query)
        .collect();
    assert_eq!(
        queries,
        [
            "SELECT account, balance FROM balances WHERE account IN (1, 2)",
            "SELECT account, balance FROM balances WHERE account IN (3, 4)",
            "SELECT account, balance FROM balances WHERE account IN (5)",
        ]
    );
}

#[tokio::test]
async fn an_empty_lookup_sends_nothing_when_columns_are_declared() {
    let folder = tempfile::tempdir().unwrap();
    let declared = "columns = [{ name = \"account\", type = \"integer\" }, \
                    { name = \"balance\", type = \"decimal(18,2)\" }]";
    let plan = balances("", declared);
    let opener = Scripted::of(vec![("accounts", accounts(0))]);
    let (outcome, events) = run_plan(&plan, &[], &opener, folder.path()).await;
    outcome.unwrap();
    assert!(opener.sent_by("balances").is_empty());
    assert_eq!(csv(folder.path(), "total.csv"), "accounts,total\r\n0,\r\n");
    let table = events.iter().find_map(|event| match event {
        RunEvent::SourceDone {
            id, table, batches, ..
        } if id == "balances" => Some((table.clone(), *batches)),
        _ => None,
    });
    assert_eq!(
        table,
        Some((
            vec![
                LoadedColumn {
                    name: "account".to_owned(),
                    type_name: "BIGINT".to_owned(),
                    declared: true,
                },
                LoadedColumn {
                    name: "balance".to_owned(),
                    type_name: "DECIMAL(18,2)".to_owned(),
                    declared: true,
                },
            ],
            Some(0)
        ))
    );
}

#[tokio::test]
async fn an_empty_lookup_on_sql_without_columns_matches_nothing_once() {
    let folder = tempfile::tempdir().unwrap();
    // With no rows, nothing types the undeclared columns: they stay text, which `sum` refuses.
    let plan = plan(
        &BALANCES
            .replace("{batch_size}", "")
            .replace("{balances}", "")
            .replace("sum(balance) AS total", "count(balance) AS total"),
    );
    let opener = Scripted::of(vec![
        ("accounts", accounts(0)),
        ("balances", rows(&["account", "balance"], Vec::new())),
    ]);
    let (outcome, _) = run_plan(&plan, &[], &opener, folder.path()).await;
    outcome.unwrap();
    let queries: Vec<String> = opener
        .sent_by("balances")
        .into_iter()
        .map(|sent| sent.query)
        .collect();
    assert_eq!(
        queries,
        ["SELECT account, balance FROM balances WHERE account IN (NULL)"]
    );
}

#[tokio::test]
async fn caps_count_every_batch() {
    let folder = tempfile::tempdir().unwrap();
    let plan = balances(", batch_size = 2", "max_rows = 3");
    let opener = Scripted::of(vec![
        ("accounts", accounts(4)),
        ("balances", balance_rows(1, 2)),
        ("balances", balance_rows(3, 4)),
    ]);
    let (outcome, _) = run_plan(&plan, &[], &opener, folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "balances".to_owned()
        }
    );
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("max_rows".to_owned())
    );
    assert_eq!(opener.sent_by("balances").len(), 2);
    assert!(files_in(folder.path()).is_empty());
}

#[tokio::test]
async fn a_lookup_names_a_column_its_source_doesnt_have() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(
        &BALANCES
            .replace("column = \"id\"", "column = \"code\"")
            .replace("{batch_size}", "")
            .replace("{balances}", ""),
    );
    let opener = Scripted::of(vec![("accounts", accounts(3))]);
    let (outcome, _) = run_plan(&plan, &[], &opener, folder.path()).await;
    assert_eq!(
        outcome.unwrap_err(),
        RunFailure::new(
            Step::Source {
                id: "balances".to_owned()
            },
            "lookup: \"accounts\" has no column \"code\"; it has id"
        )
    );
}

/// Ledger entries, and their counterparties read from MongoDB by the entries' ids.
const COUNTERPARTIES: &str = r#"
[report]
name = "Counterparties"
params = { since = "timestamp" }

[[source]]
id = "entries"
data_source = "ledger"
query = "SELECT counterparty_id FROM entries"

[[source]]
id = "counterparties"
data_source = "crm"
lookup = { from = "entries", column = "counterparty_id", as = "ids" }
query = "db.counterparties.find({_id: {$in: :ids}, since: {$gte: :since}})"
columns = [
  { name = "id",      from = "_id" },
  { name = "name" },
  { name = "country", from = "address.country", default = "unknown" },
  { name = "credit",  from = "limits.credit", type = "decimal(18,2)" },
  { name = "orders",  type = "integer" },
  { name = "since",   type = "timestamp" },
  { name = "tags" },
]

[combine]
query = "SELECT * FROM counterparties ORDER BY id"

[[output]]
format = "csv"
file = "counterparties.csv"
"#;

#[tokio::test]
async fn takes_declared_columns_from_documents() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(COUNTERPARTIES);
    let entries = rows(
        &["counterparty_id"],
        vec![vec![cells(&["cp-2"]), cells(&["cp-1"]), cells(&["cp-2"])]],
    );
    let found = documents(&[
        r#"{"_id": "cp-1", "name": "Acme", "address": {"country": "FR"}, "limits": {"credit": {"$numberDecimal": "1000.555"}}, "orders": {"$numberLong": "7"}, "since": {"$date": {"$numberLong": "1790856000000"}}, "tags": ["a", "b"], "ignored": 1}"#,
        r#"{"_id": "cp-2", "name": "Bolt", "address": {}, "limits": null, "orders": 3, "since": {"$date": "2026-10-02T08:30:00+02:00"}}"#,
    ]);
    let opener = Scripted::of(vec![("entries", entries), ("counterparties", found)]);
    let (outcome, events) = run_plan(
        &plan,
        &[("since", "2026-01-01 00:00")],
        &opener,
        folder.path(),
    )
    .await;
    outcome.unwrap();
    assert_eq!(
        opener.sent_by("counterparties")[0].query,
        "db.counterparties.find({_id: {$in: [\"cp-1\", \"cp-2\"]}, since: {$gte: \
         ISODate(\"2026-01-01T00:00:00Z\")}})",
        "a language without SQL gets every parameter written in"
    );
    assert_eq!(
        csv(folder.path(), "counterparties.csv"),
        "id,name,country,credit,orders,since,tags\r\n\
         cp-1,Acme,FR,1000.56,7,2026-10-01 12:00:00,\"[\"\"a\"\",\"\"b\"\"]\"\r\n\
         cp-2,Bolt,unknown,,3,2026-10-02 06:30:00,\r\n"
    );
    let types: Option<Vec<String>> = events.iter().find_map(|event| match event {
        RunEvent::SourceDone { id, table, .. } if id == "counterparties" => Some(
            table
                .iter()
                .map(|column| format!("{} {}", column.name, column.type_name))
                .collect(),
        ),
        _ => None,
    });
    assert_eq!(
        types.unwrap(),
        [
            "id VARCHAR",
            "name VARCHAR",
            "country VARCHAR",
            "credit DECIMAL(18,2)",
            "orders BIGINT",
            "since TIMESTAMP",
            "tags VARCHAR"
        ]
    );
}

#[tokio::test]
async fn a_value_that_doesnt_fit_names_its_item() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(COUNTERPARTIES);
    let entries = rows(&["counterparty_id"], vec![vec![cells(&["cp-1"])]]);
    let found = documents(&[
        r#"{"_id": "cp-1", "orders": 1}"#,
        r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}, "orders": "many"}"#,
    ]);
    let opener = Scripted::of(vec![("entries", entries), ("counterparties", found)]);
    let (outcome, _) = run_plan(
        &plan,
        &[("since", "2026-01-01 00:00")],
        &opener,
        folder.path(),
    )
    .await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "counterparties".to_owned()
        }
    );
    assert!(
        failure.message.starts_with(
            "item 2 (_id \"5f1d7f0e2b5c3a0001a1b2c3\"): column \"orders\": \"many\" isn't"
        ),
        "{}",
        failure.message
    );
    assert!(files_in(folder.path()).is_empty());

    let structured = documents(&[r#"{"_id": "cp-1", "orders": {"n": 1}}"#]);
    let opener = Scripted::of(vec![
        (
            "entries",
            rows(&["counterparty_id"], vec![vec![cells(&["cp-1"])]]),
        ),
        ("counterparties", structured),
    ]);
    let (outcome, _) = run_plan(
        &plan,
        &[("since", "2026-01-01 00:00")],
        &opener,
        folder.path(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().message,
        "item 1 (_id \"cp-1\"): column \"orders\": {\"n\":1} is an object or an array, which \
         only a text column takes"
    );
}

#[tokio::test]
async fn a_source_of_rows_declares_its_columns_by_name() {
    let folder = tempfile::tempdir().unwrap();
    let text = r#"
[report]
name = "Codes"

[[source]]
id = "codes"
data_source = "ledger"
query = "SELECT * FROM codes"
columns = [
  { name = "code", from = "CODE" },
  { name = "amount", from = "value", type = "decimal(10,2)" },
]

[[output]]
format = "csv"
file = "codes.csv"
"#;
    let codes = plan(text);
    let opener = Scripted::of(vec![(
        "codes",
        rows(
            &["code", "value", "ignored"],
            vec![vec![
                cells(&["0042", "1.005", "x"]),
                vec![Some("1000".to_owned()), None, None],
            ]],
        ),
    )]);
    let (outcome, _) = run_plan(&codes, &[], &opener, folder.path()).await;
    outcome.unwrap();
    assert_eq!(
        csv(folder.path(), "codes.csv"),
        "code,amount\r\n0042,1.01\r\n1000,\r\n",
        "without a combine step, the declared columns are what the outputs get"
    );

    let missing = text.replace("from = \"value\"", "from = \"total\"");
    let opener = Scripted::of(vec![(
        "codes",
        rows(&["code", "value"], vec![vec![cells(&["1", "2"])]]),
    )]);
    let (outcome, _) = run_plan(&plan(&missing), &[], &opener, folder.path()).await;
    assert_eq!(
        outcome.unwrap_err().message,
        "column \"amount\": the result has no column \"total\"; it has code, value"
    );
}
