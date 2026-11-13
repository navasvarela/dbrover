//! Sources whose engine answers in Arrow batches (ADR 0007), on sources in memory: the table
//! their batches make, which sources ask for them, their caps, and a partitioned source's
//! ranges into one table.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, Decimal128Array, Int32Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::SchemaRef;
use dbrover_core::{Row, ValueKind};
use dbrover_export::ExportColumn;

use super::*;
use crate::partition_tests::{plan as events_plan, typed};
use crate::run_tests::{MemoryOpener, MemoryReader};

/// Batches handed over one at a time. Reading it as pages is a mistake the loader mustn't make.
struct BatchReader {
    columns: Vec<ExportColumn>,
    schema: SchemaRef,
    batches: VecDeque<RecordBatch>,
}

impl BatchReader {
    fn new(columns: Vec<ExportColumn>, batches: Vec<RecordBatch>) -> Self {
        let schema = batches.first().map(RecordBatch::schema).unwrap();
        Self {
            columns,
            schema,
            batches: batches.into(),
        }
    }
}

impl SourceReader for BatchReader {
    fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        Err(SourceError::new("read as pages"))
    }

    fn batch_schema(&self) -> Option<SchemaRef> {
        Some(Arc::clone(&self.schema))
    }

    async fn next_part(&mut self) -> Result<Option<SourcePart>, SourceError> {
        Ok(self.batches.pop_front().map(SourcePart::Batch))
    }

    async fn close(self) {}
}

/// Hands over each source's readers in order, and keeps how each statement wanted its rows.
#[derive(Default)]
struct BatchOpener {
    readers: RefCell<VecDeque<(String, BatchReader)>>,
    sent: RefCell<Vec<(String, SourceRows)>>,
}

impl BatchOpener {
    fn of(readers: Vec<(&str, BatchReader)>) -> Self {
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

impl SourceOpener for BatchOpener {
    type Reader = BatchReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<BatchReader, SourceError> {
        let id = &statement.source.id;
        self.sent.borrow_mut().push((id.clone(), statement.rows));
        let mut readers = self.readers.borrow_mut();
        let found = readers.iter().position(|(reader, _)| reader == id);
        found
            .and_then(|index| readers.remove(index))
            .map(|(_, reader)| reader)
            .ok_or_else(|| SourceError::new(format!("no batches for {id:?}")))
    }
}

fn column(name: &str, kind: ValueKind) -> ExportColumn {
    ExportColumn {
        name: name.to_owned(),
        type_name: None,
        kind,
        excel_format: None,
    }
}

/// The columns an engine reports: an integer, text holding numbers its driver gives no type,
/// text, a decimal and a timestamp.
fn columns() -> Vec<ExportColumn> {
    vec![
        column("id", ValueKind::Integer),
        column("code", ValueKind::Undeclared),
        column("label", ValueKind::Text),
        column("amount", ValueKind::Decimal),
        column("paid_at", ValueKind::Timestamp),
    ]
}

/// Rows `first` to `first + count - 1` of the payments, as an engine's batch.
fn payments(first: i32, count: i32) -> RecordBatch {
    let ids: Vec<i32> = (first..first + count).collect();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from(ids.clone())),
        Arc::new(StringArray::from_iter(
            ids.iter()
                .map(|id| (id % 2 == 0).then(|| format!("{}", id * 7))),
        )),
        Arc::new(StringArray::from_iter_values(
            ids.iter().map(|id| format!("{id:03}")),
        )),
        Arc::new(
            Decimal128Array::from_iter_values(ids.iter().map(|id| i128::from(*id) * 150))
                .with_precision_and_scale(18, 2)
                .unwrap(),
        ),
        Arc::new(TimestampMicrosecondArray::from_iter_values(
            ids.iter()
                .map(|id| 1_790_000_000_000_000 + i64::from(*id) * 1_000_000),
        )),
    ];
    let names = ["id", "code", "label", "amount", "paid_at"];
    RecordBatch::try_from_iter(names.into_iter().zip(arrays)).unwrap()
}

/// The same rows as text, as an engine without batches gives them.
fn payments_as_text(first: i32, count: i32) -> Vec<Row> {
    (first..first + count)
        .map(|id| {
            let seconds = 1_790_000_000 + i64::from(id);
            let at = jiff::Timestamp::from_second(seconds)
                .unwrap()
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string();
            vec![
                Some(id.to_string()),
                (id % 2 == 0).then(|| format!("{}", id * 7)),
                Some(format!("{id:03}")),
                Some(format!("{}.{:02}", id * 150 / 100, id * 150 % 100)),
                Some(at),
            ]
        })
        .collect()
}

const PAYMENTS: &str = r#"
[report]
name = "Payments"

[[source]]
id = "payments"
data_source = "ledger"
query = "SELECT * FROM payments"
{more}

[combine]
query = """
SELECT id, code, code + 1 AS next_code, label, amount, paid_at FROM payments ORDER BY id
"""

[[output]]
format = "csv"
file = "payments.csv"
"#;

fn payments_plan(more: &str) -> Plan {
    let text = PAYMENTS.replace("{more}", more);
    Plan::new(
        read_report(&text).unwrap(),
        &crate::partition_tests::data_sources()
            .into_iter()
            .map(|data_source| dbrover_core::DataSource {
                name: "ledger".to_owned(),
                ..data_source
            })
            .collect::<Vec<_>>(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap()
}

async fn run_in<O: SourceOpener>(
    plan: &Plan,
    values: &RunValues,
    opener: &O,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let mut events = Vec::new();
    let staging = folder.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder,
        staging: &staging,
        work: &work,
    };
    let outcome = run(plan, values, opener, folders, &mut |event| {
        events.push(event)
    })
    .await;
    (outcome, events)
}

fn no_values(plan: &Plan) -> RunValues {
    plan.values(&TypedValues::default()).unwrap()
}

fn loaded_table(events: &[RunEvent]) -> Vec<LoadedColumn> {
    events
        .iter()
        .find_map(|event| match event {
            RunEvent::SourceDone { table, .. } => Some(table.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn loads_an_engines_batches_as_its_text_would_be_loaded() {
    let plan = payments_plan("");
    let values = no_values(&plan);

    let folder = tempfile::tempdir().unwrap();
    let opener = BatchOpener::of(vec![(
        "payments",
        BatchReader::new(columns(), vec![payments(1, 3), payments(4, 2)]),
    )]);
    let (outcome, events) = run_in(&plan, &values, &opener, folder.path()).await;
    assert_eq!(outcome.unwrap().rows, 5);
    let from_batches = std::fs::read_to_string(folder.path().join("payments.csv")).unwrap();
    let table = loaded_table(&events);

    let folder = tempfile::tempdir().unwrap();
    let mut rows = payments_as_text(1, 3);
    rows.extend(payments_as_text(4, 2));
    let opener = MemoryOpener::of(vec![(
        "payments",
        MemoryReader {
            columns: columns(),
            pages: VecDeque::from([rows]),
            then: None,
            scanned: None,
        },
    )]);
    let (outcome, _) = run_in(&plan, &values, &opener, folder.path()).await;
    outcome.unwrap();
    let from_text = std::fs::read_to_string(folder.path().join("payments.csv")).unwrap();

    assert_eq!(from_batches, from_text);
    assert!(
        from_batches.starts_with(
            "id,code,next_code,label,amount,paid_at\r\n1,,,001,1.50,2026-09-21 14:13:21\r\n\
             2,14,15,002,3.00,"
        ),
        "{from_batches}"
    );
    let types: Vec<(&str, &str, Option<u64>)> = table
        .iter()
        .map(|column| {
            (
                column.name.as_str(),
                column.type_name.as_str(),
                column.bytes,
            )
        })
        .collect();
    assert_eq!(
        types,
        [
            ("id", "BIGINT", Some(20)),
            ("code", "BIGINT", Some(4)),
            ("label", "VARCHAR", Some(15)),
            ("amount", "DECIMAL(18,2)", Some(80)),
            ("paid_at", "TIMESTAMP", Some(40)),
        ]
    );
}

#[tokio::test]
async fn asks_for_batches_unless_the_file_says_text() {
    let declared = "columns = [{ name = \"id\", type = \"integer\" }]";
    let cases = [
        ("", SourceRows::Columnar),
        ("columnar = true", SourceRows::Columnar),
        ("columnar = false", SourceRows::Text),
        (declared, SourceRows::Text),
    ];
    for (more, expected) in cases {
        let plan = payments_plan(more);
        let Logic::Combine(combined) = &plan.logic else {
            panic!("{:?}", plan.logic);
        };
        let Some(PlannedSource::Query(source)) = combined.sources.first() else {
            panic!("{:?}", combined.sources);
        };
        assert_eq!(source.rows_for_loading(), expected, "{more}");

        let folder = tempfile::tempdir().unwrap();
        let opener = BatchOpener::of(vec![(
            "payments",
            BatchReader::new(columns(), vec![payments(1, 1)]),
        )]);
        let _ = run_in(&plan, &no_values(&plan), &opener, folder.path()).await;
        assert_eq!(
            opener.sent.borrow().as_slice(),
            [("payments".to_owned(), expected)],
            "{more}"
        );
    }
}

#[tokio::test]
async fn holds_a_batch_to_the_sources_caps() {
    for (more, key) in [
        ("max_rows = 4", "max_rows"),
        ("max_bytes = 100", "max_bytes"),
    ] {
        let plan = payments_plan(more);
        let folder = tempfile::tempdir().unwrap();
        let opener = BatchOpener::of(vec![(
            "payments",
            BatchReader::new(columns(), vec![payments(1, 3), payments(4, 2)]),
        )]);
        let (outcome, _) = run_in(&plan, &no_values(&plan), &opener, folder.path()).await;
        let failure = outcome.unwrap_err();
        assert_eq!(
            failure.limit.map(|limit| limit.key).as_deref(),
            Some(key),
            "{}",
            failure.message
        );
    }
}

#[tokio::test]
async fn loads_every_ranges_batches_into_one_table() {
    let plan = events_plan(&crate::partition_tests::events_report(
        r#"{ from = ":first", to = ":last", every = 1000 }"#,
        "",
    ))
    .unwrap();
    let values = plan
        .values(&typed(&[
            ("first", "0"),
            ("last", "2500"),
            ("since", "2026-01-01"),
            ("until", "2026-01-02"),
        ]))
        .unwrap();
    let range = |first: i64| {
        let ids: ArrayRef = Arc::new(Int64Array::from(vec![first, first + 1]));
        let amounts: ArrayRef = Arc::new(Int64Array::from(vec![1, 2]));
        let batch = RecordBatch::try_from_iter([("id", ids), ("amount", amounts)]).unwrap();
        BatchReader::new(
            vec![
                column("id", ValueKind::Integer),
                column("amount", ValueKind::Integer),
            ],
            vec![batch],
        )
    };
    let opener = BatchOpener::of(vec![
        ("events", range(0)),
        ("events", range(1000)),
        ("events", range(2000)),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let (outcome, _) = run_in(&plan, &values, &opener, folder.path()).await;
    outcome.unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.path().join("events.csv")).unwrap(),
        "events,total,first,last\r\n6,9,0,2001\r\n"
    );
    assert!(
        opener
            .sent
            .borrow()
            .iter()
            .all(|(_, rows)| *rows == SourceRows::Columnar)
    );
}
