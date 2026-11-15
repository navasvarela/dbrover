//! RS7's exit (`docs/reports-at-scale.md`): a report joins a PostgreSQL source with a
//! partitioned Parquet lake on MinIO, and the profile says what the lake's scan read.
//!
//! The lake is written into MinIO by this test, under a prefix of its own that each run
//! overwrites, because the fixtures in `docker/minio/dbrover` are too small to show pruning.
//! It runs where both `DBROVER_TEST_S3_URL` and `DBROVER_TEST_POSTGRES_URL` are set (see
//! `test-servers.env`), which is CI's Linux check, and skips otherwise.

// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, postgres_test_server};
use dbrover_core::{
    Connection, ConnectionConfig, CostGuard, DataSource, DataSourceId, DuckDbConfig,
    DuckDbDatabase, ObjectStorage, Password, S3Bucket, S3SignIn, SafetyLevel,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels, open_data_source};
use dbrover_report::{LimitDefaults, Plan, RunEvent, RunFolders, read_report, run};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

/// Where the lake is written in MinIO's bucket. Each run overwrites it, so a local MinIO
/// doesn't grow a prefix per run.
const LAKE_PREFIX: &str = "report-lake/events";

/// The rows the lake holds, over twelve months: enough that one month's Parquet file is far
/// smaller than the whole lake, and small enough to write in a second or two.
const LAKE_ROWS: u64 = 240_000;

/// The month the report reads, of the twelve the lake holds.
const MONTH: u64 = 7;

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
fn lake_data_source() -> Option<(DataSource, Password)> {
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

async fn one_value(connection: &mut dyn Connection, sql: &str) -> String {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(1).await.unwrap());
    }
    cursor.finish().await.unwrap();
    rows.into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .flatten()
        .unwrap_or_else(|| panic!("{sql} answered nothing"))
}

/// Writes the lake into the bucket, laid out by month as Hive does, and gives the bytes its
/// files hold, from their Parquet footers.
async fn write_lake(connection: &mut dyn Connection, bucket: &str) -> u64 {
    let address = format!("s3://{bucket}/{LAKE_PREFIX}");
    run_statement(connection, "INSTALL httpfs; LOAD httpfs").await;
    let copy = format!(
        "COPY (SELECT (i % 12) + 1 AS month, ((i // 12) % 4) AS item, \
         CAST(i % 997 AS DECIMAL(10,2)) AS amount, \
         'wallet-' || CAST(i % 1000 AS VARCHAR) AS wallet \
         FROM range({LAKE_ROWS}) t(i)) TO '{address}' \
         (FORMAT parquet, PARTITION_BY (month), OVERWRITE_OR_IGNORE true)"
    );
    run_statement(connection, &copy).await;
    one_value(
        connection,
        &format!(
            "SELECT coalesce(sum(total_compressed_size), 0)::VARCHAR \
             FROM parquet_metadata('{address}/**/*.parquet')"
        ),
    )
    .await
    .parse()
    .unwrap()
}

/// The report: the items in PostgreSQL, the events in the lake, joined for one month.
fn report(lake: &str) -> String {
    format!(
        r##"
[report]
name = "Events by item"

[[source]]
id = "items"
data_source = "items-postgres"
query = "SELECT item, name FROM rs7_items ORDER BY item"

[[source]]
id = "events"
data_source = "lake"
lake = "{lake}"
hive_partitioning = true

[combine]
query = """
SELECT items.name, count(*) AS events, sum(events.amount) AS total
FROM events JOIN items USING (item)
WHERE events.month = {MONTH}
GROUP BY ALL ORDER BY items.name
"""

[[output]]
format = "csv"
file = "events.csv"
"##
    )
}

#[tokio::test]
async fn joins_a_postgresql_source_with_a_partitioned_parquet_lake() {
    let (Some((lake_source, lake_secret)), Some(postgres)) =
        (lake_data_source(), postgres_test_server())
    else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(lake_source.id, Secret::Database, &lake_secret)
        .unwrap();
    let items = data_source(
        "items-postgres",
        ConnectionConfig::Postgres(postgres.config),
    );
    secrets
        .set(items.id, Secret::Database, &postgres.password)
        .unwrap();
    let bucket = match &lake_source.config {
        ConnectionConfig::DuckDb(config) => config.storage.as_ref().unwrap().bucket().to_owned(),
        _ => unreachable!("the lake's data source is DuckDB"),
    };
    let mut lake = open(&lake_source, &secrets, folder.path()).await;
    let lake_bytes = write_lake(lake.as_mut(), &bucket).await;
    lake.close().await.unwrap();
    let mut items_connection = open(&items, &secrets, folder.path()).await;
    for sql in [
        "DROP TABLE IF EXISTS rs7_items",
        "CREATE TABLE rs7_items (item INT PRIMARY KEY, name TEXT NOT NULL)",
        "INSERT INTO rs7_items VALUES (0, 'alpha'), (1, 'beta'), (2, 'gamma'), (3, 'delta')",
    ] {
        run_statement(items_connection.as_mut(), sql).await;
    }
    items_connection.close().await.unwrap();
    let text = report(&format!("{LAKE_PREFIX}/month=*/*.parquet"));
    let plan = Plan::new(
        read_report(&text).unwrap(),
        &[items, lake_source],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let tunnels = Tunnels::new(folder.path().join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = folder.path().join(".run");
    let work = staging.with_extension("work");
    let mut events = Vec::new();

    run(
        &plan,
        &plan.values(&BTreeMap::new()).unwrap(),
        &access,
        RunFolders {
            folder: folder.path(),
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await
    .unwrap_or_else(|failure| panic!("{failure}"));

    let csv = std::fs::read_to_string(folder.path().join("events.csv")).unwrap();
    let month_rows = LAKE_ROWS / 12;
    assert_eq!(
        csv.lines().count(),
        5,
        "a header and one row an item: {csv}"
    );
    let counted: u64 = csv
        .lines()
        .skip(1)
        .map(|line| {
            line.split(',')
                .nth(1)
                .and_then(|events| events.trim().parse::<u64>().ok())
                .unwrap_or_else(|| panic!("a count in {line}"))
        })
        .sum();
    assert_eq!(counted, month_rows, "one month's rows, joined: {csv}");
    let scanned = events
        .iter()
        .find_map(|event| match event {
            RunEvent::LakeScanned {
                id,
                requests,
                bytes,
            } if id == "events" => Some((*requests, *bytes)),
            _ => None,
        })
        .expect("the lake's scan was measured");
    let (requests, read) = scanned;
    // The exit: the lake's bytes read are far below what it holds. The query asks for one
    // month of twelve and three of its four columns, and DuckDB skips the other months'
    // files by their Hive partition without opening them.
    println!(
        "RS7 exit: the lake holds {lake_bytes} bytes in {LAKE_ROWS} rows over 12 months; \
         the run read {read} bytes in {requests} requests"
    );
    assert!(read > 0, "the bucket was read");
    assert!(
        read * 4 < lake_bytes,
        "the run read {read} of the lake's {lake_bytes} bytes, which is not far below it"
    );
}
