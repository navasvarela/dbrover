//! Partitioned sources (`docs/reports-at-scale.md`, RS4b), on sources in memory: what the file
//! may say, the ranges a run's values make, the statement each range sends and what the cost
//! guard judges, the table the ranges make together, and `max_rows_per_part`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, Row, SafetyLevel, SqliteConfig};

use super::*;
use crate::run_tests::{MemoryReader, column};

fn data_sources() -> Vec<DataSource> {
    vec![DataSource {
        id: DataSourceId::generate(),
        name: "events".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/events.db"),
            read_only: false,
        }),
    }]
}

/// Events read in ranges of their key; `{partition}` and `{more}` are the source's keys.
const EVENTS: &str = r#"
[report]
name = "Events"
params = { first = "integer", last = "integer", since = "date", until = "date" }

[[source]]
id = "events"
data_source = "events"
query = "SELECT id, amount FROM events WHERE id >= :part_from AND id < :part_to"
partition = {partition}
{more}

[combine]
query = "SELECT count(*) AS events, sum(amount) AS total, min(id) AS first, max(id) AS last FROM events"

[[output]]
format = "csv"
file = "events.csv"
"#;

fn events_report(partition: &str, more: &str) -> String {
    EVENTS
        .replace("{partition}", partition)
        .replace("{more}", more)
}

pub(crate) fn plan(text: &str) -> Result<Plan, Vec<String>> {
    let report = read_report(text)?;
    Plan::new(report, &data_sources(), &[], &LimitDefaults::default())
}

fn by_key(more: &str) -> Plan {
    plan(&events_report(
        r#"{ from = ":first", to = ":last", every = 1000 }"#,
        more,
    ))
    .unwrap()
}

pub(crate) fn typed(values: &[(&str, &str)]) -> TypedValues {
    values
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

const KEYS: [(&str, &str); 4] = [
    ("first", "0"),
    ("last", "2500"),
    ("since", "2026-01-01"),
    ("until", "2026-01-02"),
];

/// A statement as the opener was sent it.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    pub(crate) from: Option<String>,
    to: Option<String>,
    cost: CostCheck,
    deadline: std::time::Instant,
}

/// Answers each range's statement with one event a key in the range, each of amount 1, in pages
/// of 400, the range `extra` names getting that many more rows; keeps what it was sent.
#[derive(Default)]
pub(crate) struct Ranged {
    pub(crate) sent: RefCell<Vec<Sent>>,
    pub(crate) extra: Option<(String, usize)>,
    pub(crate) open: Rc<Cell<usize>>,
    pub(crate) most_open: Rc<Cell<usize>>,
}

/// A range's rows, given up a page at a time with a yield between, so ranges read at once
/// interleave; it counts itself open until it is closed.
pub(crate) struct GaugedReader {
    inner: MemoryReader,
    open: Rc<Cell<usize>>,
}

impl SourceReader for GaugedReader {
    fn columns(&self) -> &[dbrover_export::ExportColumn] {
        self.inner.columns()
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        tokio::task::yield_now().await;
        self.inner.next_page().await
    }

    fn scanned(&self) -> Option<u64> {
        self.inner.scanned()
    }

    async fn close(self) {
        self.open.set(self.open.get() - 1);
    }
}

impl SourceOpener for Ranged {
    type Reader = GaugedReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<GaugedReader, SourceError> {
        let bound = |name: &str| statement.parameters.get(name).cloned().flatten();
        let (from, to) = (bound(PART_FROM), bound(PART_TO));
        self.sent.borrow_mut().push(Sent {
            from: from.clone(),
            to: to.clone(),
            cost: statement.cost.clone(),
            deadline: statement.deadline,
        });
        let key = |text: Option<String>| -> Result<u64, SourceError> {
            text.and_then(|text| text.parse().ok())
                .ok_or_else(|| SourceError::new("no range bound"))
        };
        let (from, to) = (key(from)?, key(to)?);
        let extra = match &self.extra {
            Some((at, count)) if *at == from.to_string() => *count,
            _ => 0,
        };
        let rows: Vec<Row> = (from..to)
            .chain(std::iter::repeat_n(from, extra))
            .map(|id| vec![Some(id.to_string()), Some("1".to_owned())])
            .collect();
        self.open.set(self.open.get() + 1);
        self.most_open
            .set(self.most_open.get().max(self.open.get()));
        Ok(GaugedReader {
            inner: MemoryReader {
                columns: vec![column("id"), column("amount")],
                pages: rows
                    .chunks(400)
                    .map(<[Row]>::to_vec)
                    .collect::<VecDeque<_>>(),
                then: None,
                scanned: Some(8 * (to - from)),
            },
            open: Rc::clone(&self.open),
        })
    }
}

async fn run_plan(
    plan: &Plan,
    opener: &Ranged,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let values = plan.values(&typed(&KEYS)).unwrap();
    let staging = folder.join(".run");
    let work = folder.join(".work");
    let mut events = Vec::new();
    let outcome = run(
        plan,
        &values,
        opener,
        RunFolders {
            folder,
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await;
    (outcome, events)
}

#[test]
fn names_every_problem_with_a_partition() {
    let cases = [
        (
            events_report(
                r#"{ from = ":first", to = ":last", every = 1000 }"#,
                r#"lookup = { from = "events", column = "id", as = "ids" }"#,
            ),
            "a source reads by a lookup or in ranges, not both",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last" }"#, ""),
            "partition needs every",
        ),
        (
            events_report(
                r#"{ from = ":first", to = ":last", every = 1000, by = "id" }"#,
                "",
            ),
            r#"partition: "by" is not part of a partition"#,
        ),
        (
            events_report(r#"{ from = ":nope", to = ":last", every = 1000 }"#, ""),
            "from names :nope, which isn't one of [report] params",
        ),
        (
            events_report(r#"{ from = ":since", to = ":last", every = 1000 }"#, ""),
            "from is of dates and to of integers",
        ),
        (
            events_report(
                r#"{ from = ":since", to = ":until", every = "6 hours" }"#,
                "",
            ),
            "ranges of dates are a day or more wide",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last", every = "1 day" }"#, ""),
            "every is an integer",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last", every = 0 }"#, ""),
            "every must be greater than zero",
        ),
        (
            events_report(
                r#"{ from = ":first", to = ":last", every = 1000, parallel = 17 }"#,
                "",
            ),
            "parallel is how many ranges run at once, 1 to 16",
        ),
        (
            events_report(r#"{ from = 2026-01-01T00:00:00Z, to = ":last", every = 1 }"#, ""),
            "has a time zone offset",
        ),
        (
            events_report(
                r#"{ from = ":first", to = ":last", every = 1000 }"#,
                "max_rows_per_part = 0",
            ),
            "max_rows_per_part must be a whole number greater than zero",
        ),
        (
            EVENTS
                .replace("partition = {partition}", "")
                .replace("{more}", "max_rows_per_part = 10"),
            "max_rows_per_part bounds each range of a partitioned source",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last", every = 1000 }"#, "")
                .replace("first = \"integer\"", "first = \"integer\", part_to = \"date\""),
            "[report] params can't name part_to",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last", every = 1000 }"#, "")
                .replace("[combine]", "[unused]")
                .replace("query = \"SELECT count(*)", "unused = \"SELECT count(*)"),
            "a partitioned source loads its ranges into one table for a [combine] query",
        ),
        (
            events_report(r#"{ from = ":first", to = ":last", every = 1000 }"#, "")
                .replace("query = \"SELECT id, amount FROM events WHERE id >= :part_from AND id < :part_to\"", "attach = true"),
            "partition is for a source's query, and an attached source has none",
        ),
    ];
    for (text, problem) in cases {
        let problems = plan(&text).unwrap_err();
        assert!(
            problems.iter().any(|found| found.contains(problem)),
            "{problem}: {problems:?}"
        );
    }
}

#[test]
fn plans_a_query_that_reads_its_range_and_values_that_make_ranges() {
    let unread = events_report(r#"{ from = ":first", to = ":last", every = 1000 }"#, "").replace(
        "WHERE id >= :part_from AND id < :part_to",
        "WHERE id >= :first",
    );
    let problems = plan(&unread).unwrap_err();
    assert!(
        problems[0].contains("the query uses neither :part_from nor :part_to"),
        "{problems:?}"
    );

    let plan = by_key("");
    assert_eq!(
        plan.params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "last", "since", "until"],
        "the ranges' bounds aren't parameters the run asks for"
    );
    let values = plan.values(&typed(&KEYS)).unwrap();
    let ranges = &values.ranges["events"];
    let bounds: Vec<(&str, &str)> = ranges
        .ranges
        .iter()
        .map(|range| (range.from.as_str(), range.to.as_str()))
        .collect();
    assert_eq!(bounds, [("0", "1000"), ("1000", "2000"), ("2000", "2500")]);
    assert_eq!(
        (ranges.whole.from.as_str(), ranges.whole.to.as_str()),
        ("0", "2500")
    );

    let backwards = plan
        .values(&typed(&[
            ("first", "10"),
            ("last", "10"),
            ("since", "2026-01-01"),
            ("until", "2026-01-02"),
        ]))
        .unwrap_err();
    assert_eq!(
        backwards,
        [
            "source \"events\": partition: from (10) isn't before to (10), so there is no range to read"
        ]
    );
}

#[tokio::test]
async fn reads_each_range_into_one_table_judging_the_whole_period_first() {
    let folder = tempfile::tempdir().unwrap();
    let plan = by_key("");
    let opener = Ranged::default();
    let (outcome, events) = run_plan(&plan, &opener, folder.path()).await;
    outcome.unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.path().join("events.csv")).unwrap(),
        "events,total,first,last\r\n2500,2500,0,2499\r\n",
        "every range's rows, once each"
    );

    let sent = opener.sent.borrow();
    let bounds: Vec<(Option<&str>, Option<&str>)> = sent
        .iter()
        .map(|sent| (sent.from.as_deref(), sent.to.as_deref()))
        .collect();
    assert_eq!(
        bounds,
        [
            (Some("0"), Some("1000")),
            (Some("1000"), Some("2000")),
            (Some("2000"), Some("2500")),
        ]
    );
    let CostCheck::WholePeriod(whole) = &sent[0].cost else {
        panic!(
            "the first range judges the whole period: {:?}",
            sent[0].cost
        );
    };
    assert_eq!(
        (
            whole.parameters.get(PART_FROM),
            whole.parameters.get(PART_TO)
        ),
        (Some(&Some("0".to_owned())), Some(&Some("2500".to_owned())))
    );
    assert!(
        sent[1..]
            .iter()
            .all(|later| later.cost == CostCheck::Judged),
        "{sent:?}"
    );
    assert!(
        sent[1].deadline > sent[0].deadline,
        "each range has the source's timeout of its own"
    );

    let done = events.iter().find_map(|event| match event {
        RunEvent::SourceDone {
            rows,
            parts,
            batches,
            ..
        } => Some((*rows, *parts, *batches)),
        _ => None,
    });
    assert_eq!(done, Some((2_500, Some(3), None)));
}

#[tokio::test]
async fn stops_at_a_range_past_max_rows_per_part_naming_it() {
    let folder = tempfile::tempdir().unwrap();
    let plan = by_key("max_rows_per_part = 1200");
    let opener = Ranged {
        extra: Some(("1000".to_owned(), 300)),
        ..Ranged::default()
    };
    let (outcome, _) = run_plan(&plan, &opener, folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "events".to_owned()
        }
    );
    assert!(
        failure.message.starts_with(
            "range 2 of 3 (1000 to 2000) returned more than 1,200 rows, its max_rows_per_part"
        ),
        "{}",
        failure.message
    );
    let limit = failure.limit.unwrap();
    assert_eq!(
        (limit.key.as_str(), limit.raise_in.as_str()),
        ("max_rows_per_part", "the [[source]] \"events\"")
    );
    assert!(!folder.path().join("events.csv").exists());
    assert_eq!(opener.sent.borrow().len(), 2, "range 3 never ran");
}

#[tokio::test]
async fn reads_ranges_at_once_up_to_parallel_into_the_same_table() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(&events_report(
        r#"{ from = ":first", to = ":last", every = 200, parallel = 3 }"#,
        "",
    ))
    .unwrap();
    let opener = Ranged::default();
    let (outcome, events) = run_plan(&plan, &opener, folder.path()).await;
    outcome.unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.path().join("events.csv")).unwrap(),
        "events,total,first,last\r\n2500,2500,0,2499\r\n",
        "the same table as one range at a time"
    );
    assert_eq!(opener.sent.borrow().len(), 13);
    assert_eq!(
        opener.most_open.get(),
        3,
        "never more than parallel at once"
    );
    assert_eq!(opener.open.get(), 0, "every range's statement closed");

    let mut parts: Vec<&PartDone> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::PartDone(part) => Some(part),
            _ => None,
        })
        .collect();
    parts.sort_unstable_by_key(|part| part.number);
    let said = |part: &PartDone| {
        (
            part.number,
            part.of,
            part.from.clone(),
            part.to.clone(),
            part.rows,
            part.scanned_bytes,
        )
    };
    assert_eq!(parts.len(), 13);
    assert_eq!(
        said(parts[0]),
        (1, 13, "0".to_owned(), "200".to_owned(), 200, Some(1_600))
    );
    assert_eq!(
        said(parts[12]),
        (13, 13, "2400".to_owned(), "2500".to_owned(), 100, Some(800))
    );
    let done = events.iter().find_map(|event| match event {
        RunEvent::SourceDone {
            rows,
            parts,
            scanned_bytes,
            ..
        } => Some((*rows, *parts, *scanned_bytes)),
        _ => None,
    });
    assert_eq!(done, Some((2_500, Some(13), Some(20_000))));
}

#[test]
fn says_how_a_partitioned_source_reads_as_the_run_starts() {
    let plan = plan(
        &events_report(
            r#"{ from = ":since", to = ":until", every = "1 month", parallel = 2 }"#,
            "",
        )
        .replace(":part_from", "date(:part_from)"),
    )
    .unwrap();
    let starts = plan.source_starts();
    assert_eq!(
        starts[0].partition,
        Some(PartitionStart {
            unit: "month".to_owned(),
            parallel: 2,
        })
    );
}

/// Events by key, the whole run a range at a time (RS4d); `{report}` goes in `[report]` and
/// `{outputs}` after the combine step.
pub(crate) const BY_RANGE: &str = r#"
[report]
name = "Events by range"
params = { first = "integer", last = "integer" }
{report}

[[source]]
id = "events"
data_source = "events"
query = "SELECT id, amount FROM events WHERE id >= :part_from AND id < :part_to"

[combine]
query = "SELECT id, amount * 2 AS doubled FROM events ORDER BY id"

{outputs}
"#;

pub(crate) fn by_range(report: &str, outputs: &str) -> Result<Plan, Vec<String>> {
    plan(
        &BY_RANGE
            .replace("{report}", report)
            .replace("{outputs}", outputs),
    )
}

pub(crate) const EVERY_THOUSAND: &str =
    r#"partition = { from = ":first", to = ":last", every = 1000 }"#;

pub(crate) const RANGE_KEYS: [(&str, &str); 2] = [("first", "0"), ("last", "2500")];

async fn run_values(
    plan: &Plan,
    values: &[(&str, &str)],
    opener: &Ranged,
    folder: &Path,
) -> (Result<RunSummary, RunFailure>, Vec<RunEvent>) {
    let values = plan.values(&typed(values)).unwrap();
    let staging = folder.join(".run");
    let work = folder.join(".work");
    let mut events = Vec::new();
    let outcome = run(
        plan,
        &values,
        opener,
        RunFolders {
            folder,
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await;
    assert!(!staging.exists(), "the run leaves no staging folder");
    assert!(!work.exists(), "the run leaves no work folder");
    (outcome, events)
}

pub(crate) fn files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn names_every_problem_with_a_partitioned_run() {
    let csv = "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"";
    let cases = [
        (
            by_range(
                r#"partition = { from = ":first", to = ":last", every = 1000, parallel = 2 }"#,
                csv,
            ),
            "a run's ranges go one at a time",
        ),
        (
            by_range(EVERY_THOUSAND, csv).map(|_| ()).and_then(|()| {
                plan(
                    &BY_RANGE
                        .replace("{report}", EVERY_THOUSAND)
                        .replace("{outputs}", csv)
                        .replace(
                            "AND id < :part_to\"",
                            "AND id < :part_to\"\npartition = { from = \":first\", to = \
                             \":last\", every = 10 }",
                        ),
                )
            }),
            "the run is partitioned already",
        ),
        (
            by_range(
                EVERY_THOUSAND,
                &format!("{csv}\n\n[[chart]]\nfile = \"c.svg\"\nquery = \"SELECT 1 AS x\""),
            ),
            "in a partitioned run, the sources' tables hold one range at a time",
        ),
        (
            plan(
                &BY_RANGE
                    .replace("{report}", EVERY_THOUSAND)
                    .replace("{outputs}", csv)
                    .replace(":part_from AND id < :part_to", ":first AND id < :last"),
            ),
            "no query uses :part_from or :part_to",
        ),
        (
            by_range(
                EVERY_THOUSAND,
                "[[output]]\nformat = \"csv\"\nfile = \"events-{part_from:%Y}.csv\"",
            ),
            "{part_from:%Y}: only a date or timestamp parameter takes a format",
        ),
    ];
    for (planned, problem) in cases {
        let problems = planned.unwrap_err();
        assert!(
            problems.iter().any(|found| found.contains(problem)),
            "{problem}: {problems:?}"
        );
    }

    let by_thousands = by_range(
        r#"partition = { from = ":first", to = ":last", every = 1000 }"#,
        "[[output]]\nformat = \"csv\"\nfile = \"events-{last}.csv\"\n\n[[output]]\nformat = \
         \"csv\"\nfile = \"events-{part_from}.csv\"",
    )
    .unwrap();
    let values = by_thousands.values(&typed(&RANGE_KEYS)).unwrap();
    assert_eq!(values.files, ["events-2500.csv", "events-0.csv"]);
    let ranges = values.run_ranges.as_ref().unwrap();
    let second = by_thousands
        .range_values(&values, &ranges.ranges[1])
        .unwrap();
    assert_eq!(second.files, ["events-2500.csv", "events-1000.csv"]);
    assert_eq!(
        second.parameters.get(PART_FROM),
        Some(&Some("1000".to_owned()))
    );
}

#[tokio::test]
async fn a_partitioned_run_writes_what_the_unpartitioned_run_writes() {
    let appended = "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"\n\n\
                    [[output]]\nformat = \"xlsx\"\nfile = \"events.xlsx\"\n\n\
                    [[output]]\nformat = \"csv\"\nfile = \"events-{part_from}.csv\"";
    let partitioned = by_range(EVERY_THOUSAND, appended).unwrap();
    let whole_folder = tempfile::tempdir().unwrap();
    let whole = plan(
        &BY_RANGE
            .replace("{report}", "")
            .replace(
                "params = { first = \"integer\", last = \"integer\" }",
                "params = { part_from = \"integer\", part_to = \"integer\" }",
            )
            .replace(
                "{outputs}",
                "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"",
            ),
    )
    .unwrap();
    let (outcome, _) = run_values(
        &whole,
        &[("part_from", "0"), ("part_to", "2500")],
        &Ranged::default(),
        whole_folder.path(),
    )
    .await;
    outcome.unwrap();

    let folder = tempfile::tempdir().unwrap();
    let opener = Ranged::default();
    let (outcome, events) = run_values(&partitioned, &RANGE_KEYS, &opener, folder.path()).await;
    let summary = outcome.unwrap();
    assert_eq!(summary.rows, 2_500);
    let read = |folder: &Path, name: &str| std::fs::read_to_string(folder.join(name)).unwrap();
    assert_eq!(
        read(folder.path(), "events.csv"),
        read(whole_folder.path(), "events.csv"),
        "every range's rows appended to one file, its header once"
    );
    assert_eq!(
        files(folder.path()),
        [
            "events-0.csv",
            "events-1000.csv",
            "events-2000.csv",
            "events.csv",
            "events.xlsx"
        ]
    );
    assert_eq!(read(folder.path(), "events-2000.csv").lines().count(), 501);
    let mut workbook: calamine::Xlsx<_> =
        calamine::open_workbook(folder.path().join("events.xlsx")).unwrap();
    let sheet = calamine::Reader::worksheet_range_at(&mut workbook, 0)
        .unwrap()
        .unwrap();
    assert_eq!(sheet.height(), 2_501, "a header and every range's rows");

    let written: Vec<(Option<usize>, u64)> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::OutputWritten(file) => Some((file.output, file.rows)),
            _ => None,
        })
        .collect();
    assert_eq!(
        written,
        [
            (Some(3), 1_000),
            (Some(3), 1_000),
            (Some(3), 500),
            (Some(1), 2_500),
            (Some(2), 2_500)
        ],
        "each range's own file as the range ends, then the appended ones in the outputs' order"
    );
    let ranges: Vec<(Step, u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::PartDone(part) => Some((part.step.clone(), part.number, part.rows)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ranges,
        [
            (Step::Combine, 1, 1_000),
            (Step::Combine, 2, 1_000),
            (Step::Combine, 3, 500)
        ]
    );
    let combined = events
        .iter()
        .filter(|event| matches!(event, RunEvent::CombineDone { .. }))
        .count();
    assert_eq!(combined, 3, "a combine step a range");
}

#[tokio::test]
async fn a_range_that_fails_leaves_the_finished_ranges_files_and_no_appended_one() {
    let folder = tempfile::tempdir().unwrap();
    let plan = plan(
        &BY_RANGE
            .replace("{report}", EVERY_THOUSAND)
            .replace(
                "{outputs}",
                "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"\n\n\
                 [[output]]\nformat = \"csv\"\nfile = \"events-{part_from}.csv\"\n\n\
                 [[output]]\nformat = \"parquet\"\nfile = \"events.parquet\"",
            )
            .replace(
                "AND id < :part_to\"",
                "AND id < :part_to\"\nmax_rows = 1500",
            ),
    )
    .unwrap();
    // Each range holds 1,000 rows, under the source's caps; a range's tables are its own.
    let (outcome, _) = run_values(&plan, &RANGE_KEYS, &Ranged::default(), folder.path()).await;
    outcome.unwrap();
    let opener = Ranged {
        extra: Some(("2000".to_owned(), 1_200)),
        ..Ranged::default()
    };
    let failed = tempfile::tempdir().unwrap();
    let (outcome, events) = run_values(&plan, &RANGE_KEYS, &opener, failed.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("max_rows".to_owned())
    );
    assert_eq!(
        files(failed.path()),
        ["events-0.csv", "events-1000.csv"],
        "ranges 1 and 2's own files, and nothing of the files every range appends to"
    );
    for name in ["events-0.csv", "events-1000.csv"] {
        assert_eq!(
            std::fs::read(failed.path().join(name)).unwrap(),
            std::fs::read(folder.path().join(name)).unwrap(),
            "{name} as the run that finished wrote it"
        );
    }
    let written = events
        .iter()
        .filter(|event| matches!(event, RunEvent::OutputWritten(_)))
        .count();
    assert_eq!(written, 2, "each file put in place is said as it goes");
}

#[tokio::test]
async fn writes_the_preview_and_the_charts_of_a_partitioned_run_from_every_range() {
    let outputs = "[[output]]\nformat = \"csv\"\nfile = \"events-{part_from}.csv\"\n\n\
                   [[chart]]\nfile = \"events.svg\"\nkind = \"lines\"\nx = \"id\"\ny = \"doubled\"";
    let folder = tempfile::tempdir().unwrap();
    let plan = by_range(EVERY_THOUSAND, outputs).unwrap();
    let (outcome, events) = run_values(&plan, &RANGE_KEYS, &Ranged::default(), folder.path()).await;
    let summary = outcome.unwrap();
    let preview = events
        .iter()
        .find_map(|event| match event {
            RunEvent::Preview(preview) => Some(preview.clone()),
            _ => None,
        })
        .unwrap();
    let ids: Vec<String> = preview
        .rows
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    let first: Vec<String> = (0..crate::PREVIEW_ROWS).map(|id| id.to_string()).collect();
    assert_eq!(ids, first, "the first rows of the first range");
    assert!(preview.more);
    assert_eq!(
        files(folder.path()),
        [
            "events-0.csv",
            "events-1000.csv",
            "events-2000.csv",
            "events.svg"
        ]
    );
    let drawn = summary
        .files
        .iter()
        .find(|file| file.path.ends_with("events.svg"))
        .unwrap();
    assert_eq!(drawn.rows, 2_500, "the chart drew every range's rows");
}

#[tokio::test]
async fn writes_a_parquet_file_a_range_and_reads_a_lone_source_a_range_at_a_time() {
    let folder = tempfile::tempdir().unwrap();
    let parquet = by_range(
        EVERY_THOUSAND,
        "[[output]]\nformat = \"parquet\"\nfile = \"events-{part_from}.parquet\"\n\n\
         [[output]]\nformat = \"csv\"\nfile = \"events.csv\"",
    )
    .unwrap();
    let (outcome, events) =
        run_values(&parquet, &RANGE_KEYS, &Ranged::default(), folder.path()).await;
    assert_eq!(outcome.unwrap().rows, 2_500);
    assert_eq!(
        files(folder.path()),
        [
            "events-0.parquet",
            "events-1000.parquet",
            "events-2000.parquet",
            "events.csv"
        ]
    );
    let parquet_rows: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::OutputWritten(file) if file.output == Some(1) => Some(file.rows),
            _ => None,
        })
        .collect();
    assert_eq!(parquet_rows, [1_000, 1_000, 500]);

    let lone = tempfile::tempdir().unwrap();
    let direct = plan(
        &BY_RANGE
            .replace("{report}", EVERY_THOUSAND)
            .replace(
                "[combine]\nquery = \"SELECT id, amount * 2 AS doubled FROM events ORDER BY id\"",
                "",
            )
            .replace(
                "{outputs}",
                "[[output]]\nformat = \"csv\"\nfile = \"events.csv\"",
            ),
    )
    .unwrap();
    let opener = Ranged::default();
    let (outcome, events) = run_values(&direct, &RANGE_KEYS, &opener, lone.path()).await;
    assert_eq!(outcome.unwrap().rows, 2_500);
    assert_eq!(opener.sent.borrow().len(), 3);
    let csv = std::fs::read_to_string(lone.path().join("events.csv")).unwrap();
    assert_eq!(csv.lines().count(), 2_501);
    let sources_done = events
        .iter()
        .filter(|event| matches!(event, RunEvent::SourceDone { .. }))
        .count();
    assert_eq!(sources_done, 3, "the source is read once a range");
}

/// The rows of a Parquet file in the order it holds them, as DuckDB writes each value as text.
pub(crate) async fn parquet_rows(file: &Path) -> Vec<Row> {
    use dbrover_core::{Connection, DuckDbConfig, DuckDbDatabase};
    let mut connection =
        dbrover_driver_duckdb::connect(&DuckDbConfig::new(DuckDbDatabase::InMemory))
            .await
            .unwrap();
    let sql = format!("SELECT * FROM read_parquet('{}')", file.display());
    let mut cursor = connection.execute(&sql).await.unwrap();
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(1_000).await.unwrap());
    }
    cursor.finish().await.unwrap();
    Box::new(connection).close().await.unwrap();
    rows
}

#[tokio::test]
async fn appends_every_ranges_rows_to_one_parquet_file_in_order() {
    let folder = tempfile::tempdir().unwrap();
    let plan = by_range(
        EVERY_THOUSAND,
        "[[output]]\nformat = \"parquet\"\nfile = \"events.parquet\"\n\n\
         [[output]]\nformat = \"csv\"\nfile = \"events.csv\"\n\n\
         [[output]]\nformat = \"parquet\"\nfile = \"events-{part_from}.parquet\"",
    )
    .unwrap();
    let (outcome, events) = run_values(&plan, &RANGE_KEYS, &Ranged::default(), folder.path()).await;
    assert_eq!(outcome.unwrap().rows, 2_500);
    assert_eq!(
        files(folder.path()),
        [
            "events-0.parquet",
            "events-1000.parquet",
            "events-2000.parquet",
            "events.csv",
            "events.parquet"
        ],
        "no range's part is left"
    );
    let rows = parquet_rows(&folder.path().join("events.parquet")).await;
    let ids: Vec<u64> = rows
        .iter()
        .filter_map(|row| row.first()?.as_deref()?.parse().ok())
        .collect();
    let in_order: Vec<u64> = (0..2_500).collect();
    assert_eq!(ids, in_order, "every range's rows, in the ranges' order");
    let csv = std::fs::read_to_string(folder.path().join("events.csv")).unwrap();
    assert_eq!(csv.lines().count(), 2_501);
    let written: Vec<(Option<usize>, u64)> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::OutputWritten(file) => Some((file.output, file.rows)),
            _ => None,
        })
        .collect();
    assert_eq!(
        written,
        [
            (Some(3), 1_000),
            (Some(3), 1_000),
            (Some(3), 500),
            (Some(1), 2_500),
            (Some(2), 2_500)
        ]
    );
}

#[tokio::test]
async fn holds_an_appended_parquet_file_to_its_max_size() {
    let folder = tempfile::tempdir().unwrap();
    let plan = by_range(
        EVERY_THOUSAND,
        "[[output]]\nformat = \"parquet\"\nfile = \"events.parquet\"\nmax_size = \"4 KB\"",
    )
    .unwrap();
    let (outcome, _) = run_values(&plan, &RANGE_KEYS, &Ranged::default(), folder.path()).await;
    let failure = outcome.unwrap_err();
    assert_eq!(
        failure.limit.map(|limit| limit.key),
        Some("max_size".to_owned())
    );
    assert!(
        files(folder.path()).is_empty(),
        "{:?}",
        files(folder.path())
    );
}
