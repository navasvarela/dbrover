//! Runs with a source in memory: what they write, what they report, and each limit that
//! stops one, leaving no file behind.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use calamine::{Data, Reader, Xlsx, open_workbook};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, Row, SafetyLevel, SqliteConfig, ValueKind,
};
use dbrover_export::ExportColumn;

use super::*;

/// Pages of rows handed over one at a time, then perhaps an error.
pub(crate) struct MemoryReader {
    pub(crate) columns: Vec<ExportColumn>,
    pub(crate) pages: VecDeque<Vec<Row>>,
    pub(crate) then: Option<SourceError>,
}

impl SourceReader for MemoryReader {
    fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        match self.pages.pop_front() {
            Some(page) => Ok(Some(page)),
            None => self.then.take().map_or(Ok(None), Err),
        }
    }

    async fn close(self) {}
}

/// Hands over the readers of each source by its id, one a statement, in order, and keeps the
/// statements it was sent.
#[derive(Default)]
pub(crate) struct MemoryOpener {
    readers: RefCell<Vec<(String, MemoryReader)>>,
    pub(crate) sent: RefCell<Vec<(String, String)>>,
}

impl MemoryOpener {
    fn with(id: &str, reader: MemoryReader) -> Self {
        Self::of(vec![(id, reader)])
    }

    pub(crate) fn of(readers: Vec<(&str, MemoryReader)>) -> Self {
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
}

impl SourceOpener for MemoryOpener {
    type Reader = MemoryReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<MemoryReader, SourceError> {
        let source = statement.source;
        self.sent
            .borrow_mut()
            .push((source.id.clone(), statement.query.clone()));
        let mut readers = self.readers.borrow_mut();
        let found = readers.iter().position(|(id, _)| *id == source.id);
        found
            .map(|index| readers.remove(index).1)
            .ok_or_else(|| SourceError::new(format!("no rows for {:?}", source.id)))
    }
}

pub(crate) fn column(name: &str) -> ExportColumn {
    ExportColumn {
        name: name.to_owned(),
        type_name: None,
        kind: ValueKind::Undeclared,
        excel_format: None,
    }
}

/// `count` accounts, each a row of its code and a balance, in pages of `page`.
fn ledger(count: usize, page: usize) -> MemoryReader {
    let rows: Vec<Row> = (0..count)
        .map(|n| vec![Some(format!("{}", 1000 + n)), Some(format!("{n}.505"))])
        .collect();
    MemoryReader {
        columns: vec![column("account"), column("balance")],
        pages: rows.chunks(page).map(<[Row]>::to_vec).collect(),
        then: None,
    }
}

fn plan(source_limits: &str, outputs: &str) -> Plan {
    let text = format!(
        "[report]\nname = \"Balances\"\nparams = {{ since = \"date\" }}\n\
         [[source]]\nid = \"balances\"\ndata_source = \"ledger\"\n\
         query = \"SELECT * FROM b WHERE d >= :since\"\n{source_limits}\n{outputs}"
    );
    let report = read_report(&text).unwrap();
    let ledger = DataSource {
        id: DataSourceId::generate(),
        name: "ledger".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/ledger.db"),
            read_only: false,
        }),
    };
    Plan::new(report, &[ledger], &[], &LimitDefaults::default()).unwrap()
}

const BOTH: &str = r##"
[[output]]
format = "xlsx"
file = "balances-{since:%Y-%m}.xlsx"
columns = [
  { name = "Account", from = "account", type = "text" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00" },
]
[[output]]
format = "csv"
file = "balances-{since:%Y-%m}.csv"
columns = "same"
"##;

fn values(plan: &Plan) -> RunValues {
    plan.values(
        &[("since".to_owned(), "2026-10-01".to_owned())]
            .into_iter()
            .collect(),
    )
    .unwrap()
}

/// Runs `plan` with its source reading `reader`, into `folder`, with the events it sent.
async fn run_into(
    plan: &Plan,
    reader: MemoryReader,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let mut events = Vec::new();
    let staging = folder.join(".run");
    let folders = RunFolders {
        folder,
        staging: &staging,
    };
    let opener = MemoryOpener::with("balances", reader);
    let outcome = run(plan, &values(plan), &opener, folders, &mut |event| {
        events.push(event);
    })
    .await;
    (outcome, events)
}

/// The names of the files in `folder`.
pub(crate) fn files_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn writes_every_output_with_its_schema_and_reports_each_step() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan("", BOTH);
    let (outcome, events) = run_into(&plan, ledger(250, 100), folder.path()).await;
    let summary = outcome.unwrap();
    assert_eq!(summary.rows, 250);
    assert_eq!(
        files_in(folder.path()),
        ["balances-2026-10.csv", "balances-2026-10.xlsx"]
    );

    let csv = std::fs::read_to_string(folder.path().join("balances-2026-10.csv")).unwrap();
    let mut lines = csv.lines();
    assert_eq!(lines.next(), Some("Account,Balance"));
    assert_eq!(lines.next(), Some("1000,0.51"));
    assert_eq!(lines.next(), Some("1001,1.51"));
    assert_eq!(csv.lines().count(), 251);

    let mut book: Xlsx<_> = open_workbook(folder.path().join("balances-2026-10.xlsx")).unwrap();
    let range = book.worksheet_range("Rows").unwrap();
    assert_eq!(range.get((2, 1)), Some(&Data::Float(1.51)));
    assert_eq!(range.height(), 251);

    let kinds: Vec<&str> = events
        .iter()
        .map(|event| match event {
            RunEvent::Progress { .. } => "progress",
            RunEvent::SourceDone { .. } => "source done",
            RunEvent::Preview(_) => "preview",
            RunEvent::OutputWritten(_) => "written",
            other => panic!("the run sent {other:?}"),
        })
        .filter(|kind| *kind != "progress")
        .collect();
    assert_eq!(kinds, ["source done", "preview", "written", "written"]);
    let Some(RunEvent::Preview(preview)) = events
        .iter()
        .find(|event| matches!(event, RunEvent::Preview(_)))
    else {
        unreachable!("the kinds above include a preview");
    };
    assert_eq!(preview.columns, ["account", "balance"]);
    assert_eq!(preview.rows.len(), PREVIEW_ROWS);
    assert_eq!(
        preview.rows[0],
        [Some("1000".to_owned()), Some("0.505".to_owned())]
    );
    assert!(preview.more);
    let written: Vec<&WrittenFile> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::OutputWritten(file) => Some(file),
            _ => None,
        })
        .collect();
    assert_eq!(written[0].path, folder.path().join("balances-2026-10.xlsx"));
    assert_eq!(written[0].rows, 250);
    assert!(written[1].size > 0);
}

/// Each guardrail a run can hit, the key that raises it, and that the folder is left as it was,
/// an older file under an output's name included.
#[tokio::test]
async fn stops_at_each_limit_and_leaves_no_file() {
    let cases: [(&str, &str, &str); 3] = [
        ("max_rows = 120", BOTH, "max_rows"),
        ("max_bytes = 900", BOTH, "max_bytes"),
        (
            "",
            "[[output]]\nformat = \"csv\"\nfile = \"balances-{since:%Y-%m}.csv\"\nmax_size = 500",
            "max_size",
        ),
    ];
    for (source_limits, outputs, key) in cases {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("balances-2026-10.csv"), "last month's").unwrap();
        let plan = plan(source_limits, outputs);
        let (outcome, _) = run_into(&plan, ledger(250, 100), folder.path()).await;
        let failure = outcome.unwrap_err();
        assert_eq!(
            failure.limit.as_ref().map(|limit| limit.key.as_str()),
            Some(key),
            "{failure:?}"
        );
        assert_eq!(files_in(folder.path()), ["balances-2026-10.csv"], "{key}");
        assert_eq!(
            std::fs::read_to_string(folder.path().join("balances-2026-10.csv")).unwrap(),
            "last month's"
        );
    }
}

#[tokio::test]
async fn names_the_limit_and_where_to_raise_it() {
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_into(
        &plan("max_rows = 120", BOTH),
        ledger(250, 100),
        folder.path(),
    )
    .await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.to_string(),
        "source \"balances\": the source returned more than 120 rows, its max_rows; filter in \
         its query, or raise max_rows"
    );
    assert_eq!(
        failure.limit,
        Some(Box::new(LimitHit {
            key: "max_rows".to_owned(),
            value: "120 rows".to_owned(),
            raise_in: "the [[source]] \"balances\"".to_owned(),
            set_in: None,
        }))
    );
}

#[tokio::test]
async fn fails_on_a_value_that_doesnt_fit_and_on_a_missing_column() {
    let folder = tempfile::tempdir().unwrap();
    let mut reader = ledger(3, 10);
    reader.pages[0][1][1] = Some("n/a".to_owned());
    let (outcome, _) = run_into(&plan("", BOTH), reader, folder.path()).await;
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "output 1 (balances-2026-10.xlsx): row 2, column \"Balance\": \"n/a\" isn't a decimal \
         number"
    );
    assert_eq!(files_in(folder.path()), Vec::<String>::new());

    let missing = BOTH.replace("from = \"balance\"", "from = \"total\"");
    let (outcome, _) = run_into(&plan("", &missing), ledger(3, 10), folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Output {
            number: 1,
            file: "balances-2026-10.xlsx".to_owned()
        }
    );
    assert!(
        failure.message.contains("comes from \"total\""),
        "{failure}"
    );
    assert_eq!(files_in(folder.path()), Vec::<String>::new());
}

#[tokio::test]
async fn passes_on_the_sources_own_failure() {
    let folder = tempfile::tempdir().unwrap();
    let mut reader = ledger(150, 100);
    reader.then = Some(SourceError {
        message: "cancelled after its timeout of 10m".to_owned(),
        limit: Some(Box::new(LimitHit {
            key: "timeout".to_owned(),
            value: "10m".to_owned(),
            raise_in: "the [[source]] \"balances\"".to_owned(),
            set_in: None,
        })),
    });
    let (outcome, _) = run_into(&plan("", BOTH), reader, folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "balances".to_owned()
        }
    );
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("timeout".to_owned())
    );
    assert_eq!(files_in(folder.path()), Vec::<String>::new());
}

#[test]
fn events_are_json_lines_the_app_reads_back() {
    let events = [
        RunEvent::Started {
            report: "Trial balance".to_owned(),
            sources: vec![SourceStart {
                id: "balances".to_owned(),
                data_source: "ledger".to_owned(),
                attached: false,
                waits_for: Some("tx".to_owned()),
                engine: Some("PostgreSQL".to_owned()),
                limits: Some(SourceCaps {
                    max_rows: 1_000_000,
                    max_bytes: 1 << 30,
                    timeout_ms: 600_000,
                }),
            }],
            combine: false,
            combine_sql: None,
            version: None,
            combine_limits: Some(CombineCaps {
                memory_limit: 2 << 30,
                max_temp_size: 10 << 30,
                timeout_ms: 600_000,
            }),
            timeout_ms: Some(1_800_000),
            machine: Some(Machine {
                cores: 8,
                dbrover: "0.31.0".to_owned(),
            }),
        },
        RunEvent::Progress {
            step: Step::Combine,
            rows: 10,
            bytes: 100,
            elapsed_ms: 1000,
            batch: Some(Batch { number: 2, of: 7 }),
            busy: Some(Busy {
                waiting_pct: 90,
                loading_pct: 5,
                writing_pct: 0,
            }),
        },
        RunEvent::SourceDone {
            id: "tx".to_owned(),
            rows: 10,
            bytes: 100,
            first_row_ms: Some(5),
            elapsed_ms: 1000,
            table: vec![LoadedColumn {
                name: "quantity".to_owned(),
                type_name: "DECIMAL(38,18)".to_owned(),
                declared: true,
            }],
            warnings: Vec::new(),
            batches: Some(7),
            phases: Some(SourcePhases {
                connect_ms: 20,
                server_ms: 300,
                transfer_ms: 500,
                load_ms: 150,
                type_ms: 30,
            }),
        },
        RunEvent::SourceAttached {
            id: "prices".to_owned(),
            path: PathBuf::from("/tmp/prices.duckdb"),
        },
        RunEvent::CombineDone {
            rows: 10,
            bytes: 100,
            first_row_ms: None,
            elapsed_ms: 1000,
            peak_memory: Some(2 << 30),
            peak_temp_size: Some(0),
            query_ms: Some(900),
        },
        RunEvent::WorkerUsage {
            peak_memory: Some(3 << 30),
        },
        RunEvent::Failed(RunFailure::at_limit(
            Step::Source {
                id: "balances".to_owned(),
            },
            "too many",
            LimitHit {
                key: "max_rows".to_owned(),
                value: "120 rows".to_owned(),
                raise_in: "the [[source]] \"balances\"".to_owned(),
                set_in: None,
            },
        )),
    ];
    let line = serde_json::to_string(&events[6]).unwrap();
    assert_eq!(
        line,
        r#"{"event":"failed","step":{"step":"source","id":"balances"},"message":"too many","limit":{"key":"max_rows","value":"120 rows","raise_in":"the [[source]] \"balances\""}}"#
    );
    for event in events.into_iter().chain([RunEvent::Cancelled]) {
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<RunEvent>(&line).unwrap(), event);
        // A line as the worker writes it carries its time beside the event's own fields, and a
        // reader of plain events still reads it.
        let timed = event.clone().at(Duration::from_millis(1_234));
        let line = serde_json::to_string(&timed).unwrap();
        assert!(line.starts_with(r#"{"at_ms":1234,"event":"#), "{line}");
        assert_eq!(serde_json::from_str::<TimedEvent>(&line).unwrap(), timed);
        assert_eq!(serde_json::from_str::<RunEvent>(&line).unwrap(), event);
    }
}
