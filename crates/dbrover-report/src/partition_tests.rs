//! Partitioned sources (`docs/reports-at-scale.md`, RS4b), on sources in memory: what the file
//! may say, the ranges a run's values make, the statement each range sends and what the cost
//! guard judges, the table the ranges make together, and `max_rows_per_part`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

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

fn plan(text: &str) -> Result<Plan, Vec<String>> {
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

fn typed(values: &[(&str, &str)]) -> TypedValues {
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
struct Sent {
    from: Option<String>,
    to: Option<String>,
    cost: CostCheck,
    deadline: std::time::Instant,
}

/// Answers each range's statement with one event a key in the range, each of amount 1, in pages
/// of 400, the range `extra` names getting that many more rows; keeps what it was sent.
#[derive(Default)]
struct Ranged {
    sent: RefCell<Vec<Sent>>,
    extra: Option<(String, usize)>,
}

impl SourceOpener for Ranged {
    type Reader = MemoryReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<MemoryReader, SourceError> {
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
        Ok(MemoryReader {
            columns: vec![column("id"), column("amount")],
            pages: rows
                .chunks(400)
                .map(<[Row]>::to_vec)
                .collect::<VecDeque<_>>(),
            then: None,
            scanned: None,
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
        failure
            .message
            .starts_with("range 2 of 3 returned more than 1,200 rows, its max_rows_per_part"),
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
