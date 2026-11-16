//! RS7f (`docs/reports-at-scale.md`): a report's Athena source hands its result over through a
//! bucket, and the run reads the files with DuckDB instead of paging a thousand rows at a time.
//!
//! Athena is the replay server of its own driver's tests (`dbrover_driver_athena::replay`),
//! which answers the SDK with exchanges from a file; the bucket is MinIO from
//! `docker-compose.yml`. The replay writes nothing by itself, so the test plays Athena's own
//! side effect: the result file of a plain query is written before the run, and an `UNLOAD`'s
//! Parquet is written by a hook while the query "runs", as Athena writes it before it reports
//! `SUCCEEDED`.
//!
//! Runs where `DBROVER_TEST_S3_URL` is set (see `test-servers.env`), which is CI's Linux
//! check, and skips otherwise.

// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::MemorySecrets;
use dbrover_core::{
    AthenaConfig, AwsSignInKind, Connection, ConnectionConfig, CostGuard, DataSource, DataSourceId,
    DuckDbConfig, DuckDbDatabase, ObjectStorage, Password, S3Bucket, S3SignIn, SafetyLevel,
};
use dbrover_driver_athena::replay::Replay;
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels, open_data_source};
use dbrover_report::{LimitDefaults, Plan, RunEvent, RunFolders, read_report, run};
use dbrover_store::{Secret, SecretStore, TrustedCommands};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

/// Where this test writes in MinIO's bucket: the Athena results it stands in for, and the
/// handoff prefixes the reports name. Each test takes a name of its own under them, because
/// they share one bucket and cargo runs them at the same time.
const RESULTS: &str = "report-handoff";
const HANDOFF_PREFIX: &str = "report-handoff-tmp";

/// The rows the source returns, as both its result file and its text rows.
const ROWS: &str = "SELECT 'bolt' AS item, 10.50 AS amount UNION ALL \
                    SELECT 'nut', 2.25 UNION ALL SELECT 'bolt', 4.00";

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

/// MinIO's bucket as a DuckDB data source with no database of its own, and the access key's
/// secret; `None` when MinIO isn't there.
fn bucket_data_source() -> Option<(DataSource, Password)> {
    let Ok(url) = std::env::var("DBROVER_TEST_S3_URL") else {
        eprintln!("skipping: DBROVER_TEST_S3_URL is not set");
        return None;
    };
    let (scheme, rest) = url.split_once("://")?;
    let (credentials, rest) = rest.split_once('@')?;
    let (key_id, secret) = credentials.split_once(':')?;
    let (authority, bucket) = rest.split_once('/')?;
    let config = DuckDbConfig {
        database: DuckDbDatabase::InMemory,
        storage: Some(ObjectStorage::S3(S3Bucket {
            bucket: bucket.to_owned(),
            region: String::new(),
            endpoint: format!("{scheme}://{authority}"),
            sign_in: S3SignIn::AccessKey {
                key_id: key_id.to_owned(),
            },
        })),
        cost_guard: CostGuard::OFF,
    };
    Some((
        data_source("lake", ConnectionConfig::DuckDb(config)),
        Password::new(secret),
    ))
}

fn bucket_name(data_source: &DataSource) -> String {
    match &data_source.config {
        ConnectionConfig::DuckDb(config) => config.storage.as_ref().unwrap().bucket().to_owned(),
        _ => unreachable!("the bucket's data source is DuckDB"),
    }
}

async fn open(
    data_source: &DataSource,
    secrets: &Arc<dyn SecretStore>,
    folder: &Path,
) -> Box<dyn Connection> {
    open_data_source(
        data_source,
        secrets,
        &Tunnels::new(folder.join("known_hosts")),
        &PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner)),
    )
    .await
    .unwrap()
}

async fn run_statement(connection: &mut dyn Connection, sql: &str) {
    connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .finish()
        .await
        .unwrap();
}

async fn rows_of(connection: &mut dyn Connection, sql: &str) -> Vec<Vec<Option<String>>> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(100).await.unwrap());
    }
    cursor.finish().await.unwrap();
    rows
}

/// Every key under `prefix` in the bucket, which is how the test sees what a run left.
async fn keys_under(connection: &mut dyn Connection, bucket: &str, prefix: &str) -> Vec<String> {
    let sql = format!("SELECT file FROM glob('s3://{bucket}/{prefix}**') ORDER BY file");
    rows_of(connection, &sql)
        .await
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

/// The report: an Athena source, with a handoff when `handoff` says, summed per item in the
/// combine step.
///
/// The sum casts, so the rows a handoff's Parquet or CSV brought and the rows the text path
/// brought add up the same way: a handoff's columns are typed by the files, which is what the
/// manual tells an author to cast for.
fn report(name: &str, handoff: &str) -> String {
    format!(
        r##"
[report]
name = "{name}"

[[source]]
id = "orders"
data_source = "orders-athena"
query = "SELECT item, amount FROM orders"
{handoff}

[combine]
query = """
SELECT item, sum(CAST(amount AS DECIMAL(10,2))) AS total
FROM orders GROUP BY item ORDER BY item
"""

[[output]]
format = "csv"
file = "orders.csv"
"##
    )
}

/// Runs `text` with the sources it names, and gives the rows of its CSV output and the run's
/// events.
async fn run_report(
    text: &str,
    sources: &[DataSource],
    secrets: &Arc<dyn SecretStore>,
    folder: &Path,
) -> (String, Vec<RunEvent>) {
    let plan = Plan::new(
        read_report(text).unwrap(),
        sources,
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_or_else(|problems| panic!("{problems:#?}"));
    let tunnels = Tunnels::new(folder.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = folder.join(".run");
    let work = staging.with_extension("work");
    let mut events = Vec::new();
    run(
        &plan,
        &plan.values(&BTreeMap::new()).unwrap(),
        &access,
        RunFolders {
            folder,
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await
    .unwrap_or_else(|failure| panic!("{failure:#?}"));
    let written = std::fs::read_to_string(folder.join("orders.csv")).unwrap();
    (written, events)
}

fn athena(replay: &Replay) -> DataSource {
    data_source(
        "orders-athena",
        ConnectionConfig::Athena(AthenaConfig {
            sign_in: AwsSignInKind::LocalEmulator,
            region: "us-east-1".to_owned(),
            database: "sales".to_owned(),
            endpoint: replay.endpoint.clone(),
            ..AthenaConfig::default()
        }),
    )
}

/// What a source's `SourceDone` event says it handed over.
fn handed_over(events: &[RunEvent]) -> Option<(u64, u64)> {
    events.iter().find_map(|event| match event {
        RunEvent::SourceDone { handed_over, .. } => {
            handed_over.map(|files| (files.files, files.bytes))
        }
        _ => None,
    })
}

/// The bytes the source's engine says it scanned, which is what it bills for the statement.
fn scanned(events: &[RunEvent]) -> Option<u64> {
    events.iter().find_map(|event| match event {
        RunEvent::SourceDone { scanned_bytes, .. } => *scanned_bytes,
        _ => None,
    })
}

/// The replay for an `UNLOAD`, with Athena's own side effect played by `writer`: the Parquet
/// shards go into the folder the statement names, before the query reports `SUCCEEDED`, and
/// every statement sent is kept for the test to check.
async fn unloading_replay(
    bucket: &str,
    writer: Box<dyn Connection>,
    shards: &'static [&'static str],
) -> (Replay, Arc<Mutex<Vec<String>>>) {
    let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let writing = Arc::new(AsyncMutex::new(writer));
    {
        let mut connection = writing.lock().await;
        run_statement(connection.as_mut(), "INSTALL httpfs; LOAD httpfs").await;
    }
    let statements = Arc::clone(&sent);
    let replay = Replay::start_with(
        &include_str!("exchanges/athena-unload.json").replace("{{bucket}}", bucket),
        Some(Arc::new(move |operation: String, body: Value| {
            let statements = Arc::clone(&statements);
            let writing = Arc::clone(&writing);
            Box::pin(async move {
                if operation != "StartQueryExecution" {
                    return;
                }
                let sql = body["QueryString"].as_str().unwrap_or_default().to_owned();
                statements.lock().unwrap().push(sql.clone());
                let Some(to) = unload_destination(&sql) else {
                    return;
                };
                let mut connection = writing.lock().await;
                for (part, rows) in shards.iter().enumerate() {
                    run_statement(
                        connection.as_mut(),
                        &format!(
                            "COPY ({rows}) TO '{to}part-{part}.parquet' \
                             (FORMAT parquet, OVERWRITE_OR_IGNORE true)"
                        ),
                    )
                    .await;
                }
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        })),
    )
    .await;
    (replay, sent)
}

/// RS7f's exit: the same source loads the same table through its handoff as through its text
/// rows, and the files it read are gone when the run ends.
#[tokio::test(flavor = "multi_thread")]
async fn athenas_result_file_loads_the_same_table_as_its_text_rows_and_is_then_deleted() {
    let Some((bucket_source, secret)) = bucket_data_source() else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(bucket_source.id, Secret::Database, &secret)
        .unwrap();
    let bucket = bucket_name(&bucket_source);
    let mut writing = open(&bucket_source, &secrets, folder.path()).await;
    run_statement(writing.as_mut(), "INSTALL httpfs; LOAD httpfs").await;

    // Athena's own result file, as it writes one for every query: CSV, every value quoted.
    let result = format!("s3://{bucket}/{RESULTS}/own-result/q-handoff.csv");
    run_statement(
        writing.as_mut(),
        &format!(
            "COPY ({ROWS}) TO '{result}' (FORMAT csv, HEADER true, FORCE_QUOTE *, \
             OVERWRITE_OR_IGNORE true)"
        ),
    )
    .await;

    // The same source, read as C3 reads one: a page of text rows.
    let rows_replay =
        Replay::start(&include_str!("exchanges/athena-rows.json").replace("{{bucket}}", &bucket))
            .await;
    let paged = athena(&rows_replay);
    let (by_rows, _) = run_report(
        &report("Orders by item", ""),
        &[paged, bucket_source.clone()],
        &secrets,
        folder.path(),
    )
    .await;
    rows_replay.finish();

    // And handed over through the bucket, which reads the file Athena already wrote.
    let handoff_replay = Replay::start(
        &include_str!("exchanges/athena-handoff.json")
            .replace("{{bucket}}", &bucket)
            .replace("{{result}}", &result),
    )
    .await;
    let handed = athena(&handoff_replay);
    let (by_files, events) = run_report(
        &report("Orders by item", "handoff = { data_source = \"lake\" }"),
        &[handed, bucket_source.clone()],
        &secrets,
        folder.path(),
    )
    .await;
    handoff_replay.finish();

    assert_eq!(by_files, by_rows, "the handoff loaded a different table");
    assert_eq!(
        by_rows.lines().collect::<Vec<&str>>(),
        ["item,total", "bolt,14.50", "nut,2.25"]
    );
    let (files, _) = handed_over(&events).expect("the source says it handed its result over");
    assert_eq!(files, 1, "Athena writes one result file");
    // A handed-over statement still says what it scanned, which is what Athena bills.
    assert_eq!(scanned(&events), Some(4_096));
    // The run deletes what it read: the result file and the metadata file beside it.
    let left = keys_under(writing.as_mut(), &bucket, &format!("{RESULTS}/own-result/")).await;
    assert!(
        left.is_empty(),
        "the run left its handed-over files behind: {left:?}"
    );
    writing.close().await.unwrap();
}

/// A handoff with a prefix of its own sends `UNLOAD`, into a folder the run owns and nothing
/// else writes in, and reads the Parquet shards it finds there.
#[tokio::test(flavor = "multi_thread")]
async fn a_handoff_with_a_prefix_unloads_parquet_into_the_folder_the_run_owns() {
    let Some((bucket_source, secret)) = bucket_data_source() else {
        return;
    };
    let report_name = "Unloaded orders";
    let prefix = format!("{HANDOFF_PREFIX}/unload/");
    let folder = tempfile::tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(bucket_source.id, Secret::Database, &secret)
        .unwrap();
    let bucket = bucket_name(&bucket_source);
    let mut reading = open(&bucket_source, &secrets, folder.path()).await;
    run_statement(reading.as_mut(), "INSTALL httpfs; LOAD httpfs").await;
    let (replay, sent) = unloading_replay(
        &bucket,
        open(&bucket_source, &secrets, folder.path()).await,
        &[
            "SELECT 'bolt' AS item, 10.50 AS amount",
            "SELECT 'nut' AS item, 2.25 AS amount UNION ALL SELECT 'bolt', 4.00",
        ],
    )
    .await;

    let handed = athena(&replay);
    let (written, events) = run_report(
        &report(
            report_name,
            &format!("handoff = {{ data_source = \"lake\", prefix = \"{prefix}\" }}"),
        ),
        &[handed, bucket_source.clone()],
        &secrets,
        folder.path(),
    )
    .await;
    replay.finish();

    assert_eq!(
        written.lines().collect::<Vec<&str>>(),
        ["item,total", "bolt,14.50", "nut,2.25"]
    );
    let sent = sent.lock().unwrap().clone();
    let unload = sent
        .iter()
        .find(|statement| statement.starts_with("UNLOAD"))
        .expect("the source sent an UNLOAD");
    let destination = unload_destination(unload).expect("the UNLOAD names a folder");
    assert_eq!(
        *unload,
        format!(
            "UNLOAD (SELECT item, amount FROM orders) TO '{destination}' \
             WITH (format = 'parquet')"
        )
    );
    assert!(
        destination.starts_with(&format!("s3://{bucket}/{prefix}unloaded-orders/run-")),
        "the UNLOAD went to {destination}, not to a folder of this run's under the report's \
         prefix"
    );
    assert!(
        destination.ends_with("/orders/"),
        "the UNLOAD went to {destination}, not to the source's own folder"
    );
    let (files, bytes) = handed_over(&events).expect("the source says it handed its result over");
    assert_eq!(files, 2, "it read both shards");
    assert!(bytes > 0, "the files' bytes come from the bucket's listing");
    // Everything the run wrote under the report's prefix is gone, the run's folder included.
    let left = keys_under(reading.as_mut(), &bucket, &prefix).await;
    assert!(left.is_empty(), "the run left files behind: {left:?}");
    reading.close().await.unwrap();
}

/// A run killed before its delete leaves its files; the next run of the same report removes
/// them, and leaves alone what isn't a run's folder.
#[tokio::test(flavor = "multi_thread")]
async fn the_next_run_removes_what_a_killed_run_left_and_keeps_what_isnt_a_runs() {
    let Some((bucket_source, secret)) = bucket_data_source() else {
        return;
    };
    let report_name = "Kept orders";
    let prefix = format!("{HANDOFF_PREFIX}/leftovers/");
    let folder = tempfile::tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(bucket_source.id, Secret::Database, &secret)
        .unwrap();
    let bucket = bucket_name(&bucket_source);
    let mut reading = open(&bucket_source, &secrets, folder.path()).await;
    run_statement(reading.as_mut(), "INSTALL httpfs; LOAD httpfs").await;

    // What a run killed before its delete left: a folder of its own, named for the
    // nanosecond it started, which is long before this test's run.
    let report_prefix = format!("{prefix}kept-orders");
    let left_behind = format!("s3://{bucket}/{report_prefix}/run-1-1/orders/part-0.parquet");
    // And a folder under the same prefix that no run of DbRover's made, which it must leave
    // alone.
    let not_a_runs = format!("s3://{bucket}/{report_prefix}/my-own-files/notes.parquet");
    for address in [&left_behind, &not_a_runs] {
        run_statement(
            reading.as_mut(),
            &format!(
                "COPY (SELECT 1 AS kept) TO '{address}' (FORMAT parquet, \
                 OVERWRITE_OR_IGNORE true)"
            ),
        )
        .await;
    }

    let (replay, _) = unloading_replay(
        &bucket,
        open(&bucket_source, &secrets, folder.path()).await,
        &["SELECT 'bolt' AS item, 14.50 AS amount"],
    )
    .await;
    let handed = athena(&replay);
    run_report(
        &report(
            report_name,
            &format!("handoff = {{ data_source = \"lake\", prefix = \"{prefix}\" }}"),
        ),
        &[handed, bucket_source.clone()],
        &secrets,
        folder.path(),
    )
    .await;
    replay.finish();

    let left = keys_under(reading.as_mut(), &bucket, &format!("{report_prefix}/")).await;
    assert_eq!(
        left,
        [not_a_runs],
        "the run removed the wrong keys under its report's prefix"
    );
    reading.close().await.unwrap();
}

/// The folder an `UNLOAD` writes into, as its statement names it.
fn unload_destination(statement: &str) -> Option<String> {
    let (_, rest) = statement.split_once(" TO '")?;
    let (address, _) = rest.split_once('\'')?;
    Some(address.to_owned())
}
