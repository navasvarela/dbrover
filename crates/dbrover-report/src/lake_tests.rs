//! A lake in a report (RS7): what a `[[source]]` with `lake` may say, and a run that joins a
//! loaded source with one.
//!
//! The runs go against MinIO from `docker-compose.yml` when `DBROVER_TEST_S3_URL` is set (see
//! `test-servers.env`), over the fixtures in `docker/minio/dbrover`, and skip otherwise.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use dbrover_core::{
    ByteCount, ConnectionConfig, CostGuard, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    ObjectStorage, Password, S3Bucket, S3SignIn, SafetyLevel, SqliteConfig,
};

use super::*;
use crate::run_tests::{MemoryOpener, MemoryReader, column};

/// A report over a loaded source and a lake, joined in the combine step.
const SALES: &str = r##"
[report]
name = "Sales by region"

[[source]]
id = "regions"
data_source = "ledger"
query = "SELECT * FROM regions"

[[source]]
id = "sales"
data_source = "lake"
lake = "{lake}"
{lake_keys}

[combine]
query = """
SELECT regions.region, sum(sales.amount) AS total
FROM sales JOIN regions USING (item)
GROUP BY ALL ORDER BY regions.region
"""

[[output]]
format = "csv"
file = "sales.csv"
"##;

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
            path: std::path::PathBuf::from("/tmp/ledger.db"),
            read_only: false,
        }),
    )
}

/// MinIO's bucket from `DBROVER_TEST_S3_URL`, as a DuckDB data source with no database of its
/// own: a lake is read from the bucket alone.
fn lake_data_source() -> Option<(DataSource, Password)> {
    let url = std::env::var("DBROVER_TEST_S3_URL").ok().or_else(|| {
        eprintln!("skipping: DBROVER_TEST_S3_URL is not set");
        None
    })?;
    let (scheme, rest) = url.split_once("://")?;
    let (credentials, rest) = rest.split_once('@')?;
    let (key_id, secret) = credentials.split_once(':')?;
    let (authority, bucket) = rest.split_once('/')?;
    let storage = ObjectStorage::S3(S3Bucket {
        bucket: bucket.to_owned(),
        region: String::new(),
        endpoint: format!("{scheme}://{authority}"),
        sign_in: S3SignIn::AccessKey {
            key_id: key_id.to_owned(),
        },
    });
    let config = DuckDbConfig {
        storage: Some(storage),
        ..DuckDbConfig::new(DuckDbDatabase::InMemory)
    };
    Some((
        data_source("lake", ConnectionConfig::DuckDb(config)),
        Password::new(secret),
    ))
}

/// The engine's opener resolves a data source's password from the keychain; this one is given
/// MinIO's, which is the secret of the bucket's access key.
struct LakeOpener {
    sources: MemoryOpener,
    secret: Password,
}

impl SourceOpener for LakeOpener {
    type Reader = MemoryReader;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<MemoryReader, SourceError> {
        self.sources.open(statement).await
    }

    async fn password(&self, _data_source: &DataSource) -> Result<Option<Password>, SourceError> {
        Ok(Some(self.secret.clone()))
    }
}

/// Two items of the MinIO fixtures' `sales` lake, each in a region, as a loaded source: the
/// fixtures hold `item 1` to `item 15`, whose amounts are 2.50 each item (see
/// `docker/minio/README.md`).
fn regions() -> MemoryReader {
    MemoryReader {
        columns: vec![column("item"), column("region")],
        pages: VecDeque::from([vec![
            vec![Some("item 1".to_owned()), Some("eu".to_owned())],
            vec![Some("item 11".to_owned()), Some("us".to_owned())],
        ]]),
        then: None,
        scanned: None,
    }
}

/// The report read and planned, so a problem the file's own reading finds counts as one too.
fn plan_of(lake: &str, lake_keys: &str, sources: &[DataSource]) -> Result<Plan, Vec<String>> {
    let text = SALES
        .replace("{lake}", lake)
        .replace("{lake_keys}", lake_keys);
    Plan::new(read_report(&text)?, sources, &[], &LimitDefaults::default())
}

async fn run_lake(
    lake: &str,
    lake_keys: &str,
    folder: &Path,
) -> Option<(Result<RunSummary, RunFailure>, Vec<RunEvent>)> {
    let (lake_source, secret) = lake_data_source()?;
    let plan = plan_of(lake, lake_keys, &[ledger(), lake_source]).unwrap();
    let opener = LakeOpener {
        sources: MemoryOpener::of(vec![("regions", regions())]),
        secret,
    };
    let staging = folder.join(".run");
    let work = staging.with_extension("work");
    let mut events = Vec::new();
    let outcome = crate::run(
        &plan,
        &plan.values(&BTreeMap::new()).unwrap(),
        &opener,
        RunFolders {
            folder,
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await;
    Some((outcome, events))
}

#[tokio::test]
async fn joins_a_loaded_source_with_a_parquet_lake_read_in_place() {
    let folder = tempfile::tempdir().unwrap();
    let Some((outcome, events)) = run_lake(
        "sales/year=*/*.parquet",
        "hive_partitioning = true",
        folder.path(),
    )
    .await
    else {
        return;
    };

    outcome.unwrap();

    let csv = std::fs::read_to_string(folder.path().join("sales.csv")).unwrap();
    // `item 1` is in 2025's file and `item 11` in 2026's, so the join reads both years.
    assert_eq!(csv, "region,total\r\neu,2.50\r\nus,27.50\r\n");
    let read = events.iter().find_map(|event| match event {
        RunEvent::LakeRead { id, url, format } => {
            Some((id.as_str(), url.as_str(), format.as_str()))
        }
        _ => None,
    });
    assert_eq!(
        read,
        Some((
            "sales",
            "read_parquet('s3://dbrover/sales/year=*/*.parquet', hive_partitioning = true)",
            "parquet"
        )),
        "the run says what it read the lake through"
    );
    // The lake loads no rows of its own: it is read where it is.
    let loaded: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::SourceDone { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(loaded, ["regions"]);
}

#[tokio::test]
async fn reads_an_iceberg_lake_from_its_newest_metadata_file() {
    let folder = tempfile::tempdir().unwrap();
    let Some((lake_source, secret)) = lake_data_source() else {
        return;
    };
    let text = SALES
        .replace("{lake}", "warehouse/shop/orders")
        .replace("{lake_keys}", "format = \"iceberg\"")
        .replace(
            "SELECT regions.region, sum(sales.amount) AS total\nFROM sales JOIN regions USING (item)\nGROUP BY ALL ORDER BY regions.region",
            "SELECT count(*) AS orders, sum(total) AS total FROM sales",
        );
    let plan = Plan::new(
        read_report(&text).unwrap(),
        &[ledger(), lake_source],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let opener = LakeOpener {
        sources: MemoryOpener::of(vec![("regions", regions())]),
        secret,
    };
    let staging = folder.path().join(".run");
    let work = staging.with_extension("work");
    let mut events = Vec::new();

    crate::run(
        &plan,
        &plan.values(&BTreeMap::new()).unwrap(),
        &opener,
        RunFolders {
            folder: folder.path(),
            staging: &staging,
            work: &work,
        },
        &mut |event| events.push(event),
    )
    .await
    .unwrap();

    let csv = std::fs::read_to_string(folder.path().join("sales.csv")).unwrap();
    assert_eq!(csv, "orders,total\r\n4,54.50\r\n");
    let url = events.iter().find_map(|event| match event {
        RunEvent::LakeRead { url, .. } => Some(url.clone()),
        _ => None,
    });
    let url = url.expect("the lake was read");
    assert!(
        url.starts_with("iceberg_scan('s3://dbrover/warehouse/shop/orders/metadata/")
            && url.ends_with(".metadata.json')"),
        "{url}"
    );
}

/// RS7d: the data source's cost guard judges the lake before the run reads a row.
#[tokio::test]
async fn the_cost_guard_refuses_a_lake_that_holds_more_than_its_limit() {
    let folder = tempfile::tempdir().unwrap();
    let Some((lake_source, secret)) = lake_data_source() else {
        return;
    };
    let with_limit = |bytes: u64| {
        let ConnectionConfig::DuckDb(config) = &lake_source.config else {
            unreachable!("the lake's data source is DuckDB")
        };
        DataSource {
            config: ConnectionConfig::DuckDb(DuckDbConfig {
                cost_guard: CostGuard {
                    ask_above_bytes: Some(ByteCount(bytes)),
                    ..CostGuard::OFF
                },
                ..config.clone()
            }),
            ..lake_source.clone()
        }
    };
    let run = async |named: DataSource, folder: &Path| {
        let plan = plan_of(
            "sales/year=*/*.parquet",
            "hive_partitioning = true",
            &[ledger(), named],
        )
        .unwrap();
        let opener = LakeOpener {
            sources: MemoryOpener::of(vec![("regions", regions())]),
            secret: secret.clone(),
        };
        let staging = folder.join(".run");
        let work = staging.with_extension("work");
        crate::run(
            &plan,
            &plan.values(&BTreeMap::new()).unwrap(),
            &opener,
            RunFolders {
                folder,
                staging: &staging,
                work: &work,
            },
            &mut |_| {},
        )
        .await
    };

    // The fixtures hold a few hundred bytes, so one byte is over the limit and a megabyte is
    // not: the numbers the guard compares are the files' own, which another test pins.
    let refused = run(with_limit(1), folder.path()).await.unwrap_err();
    let allowed = run(with_limit(1_000_000), folder.path()).await;

    assert!(
        refused.message.contains("the lake \"sales\" holds")
            && refused.message.contains("refuses a lake above 1 bytes"),
        "{}",
        refused.message
    );
    allowed.expect("a lake under the limit runs");
}

#[tokio::test]
async fn a_lake_outside_the_combines_reach_fails_the_run_naming_the_source() {
    let folder = tempfile::tempdir().unwrap();
    let Some((outcome, _)) = run_lake("sales/nothing-here/*.parquet", "", folder.path()).await
    else {
        return;
    };

    let failure = outcome.unwrap_err();

    assert!(
        failure
            .message
            .contains("could not read the lake \"sales\""),
        "{}",
        failure.message
    );
}

#[test]
fn names_every_problem_with_a_lake() {
    let duckdb_file = data_source(
        "plain-duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path: std::path::PathBuf::from("/tmp/prices.duckdb"),
            read_only: true,
        })),
    );
    let bucket = ObjectStorage::S3(S3Bucket {
        bucket: "dbrover".to_owned(),
        sign_in: S3SignIn::Anonymous,
        ..S3Bucket::default()
    });
    let lake = data_source(
        "lake",
        ConnectionConfig::DuckDb(DuckDbConfig {
            storage: Some(bucket),
            ..DuckDbConfig::new(DuckDbDatabase::InMemory)
        }),
    );
    let sources = vec![ledger(), lake, duckdb_file];
    let cases = [
        (
            "sales/*",
            "",
            "source \"sales\": \"sales/*\" doesn't end in a name DuckDB chooses a reader \
             by, so the source needs one: format = \"parquet\", \"csv\", \"json\", \"iceberg\" \
             or \"delta\"",
        ),
        (
            "sales/*.parquet",
            "format = \"orc\"",
            "source \"sales\": \"orc\" isn't a format a lake is read as: parquet, csv, \
             json, iceberg or delta",
        ),
        (
            "s3://elsewhere/sales/*.parquet",
            "",
            "source \"sales\": lake is \"s3://elsewhere/sales/*.parquet\", which is not \
             in the data source's bucket s3://dbrover; write a path in the bucket, such as \
             \"sales/*.parquet\"",
        ),
        (
            "../etc/passwd",
            "format = \"csv\"",
            "source \"sales\": lake is \"../etc/passwd\", and a lake's path can't hold \
             \"..\"",
        ),
        (
            "warehouse/*",
            "format = \"delta\"",
            "source \"sales\": \"warehouse/*\" holds a wildcard, and a delta table is \
             read from its own folder; name the table's folder",
        ),
        (
            "sales/*.parquet",
            "max_rows = 10",
            "source \"sales\": max_rows bounds what a source's query copies, and a lake copies \
             nothing; [combine] sets the limits of what reads it, and the data source's cost \
             guard what the lake itself may hold",
        ),
        (
            "sales/*.parquet",
            "query = \"SELECT 1\"",
            "source \"sales\": a lake has no query: the [combine] query reads it in place",
        ),
        (
            "sales/*.parquet",
            "columnar = false",
            "source \"sales\": columnar is for a source's query, and a lake has none; [combine] \
             reads it as it is",
        ),
        (
            "sales/*.parquet",
            "attach = true",
            "source \"sales\": attach = true reads a DuckDB file in place and lake reads a \
             bucket's files; a source does one or the other",
        ),
    ];
    for (lake, keys, expected) in cases {
        let problems = plan_of(lake, keys, &sources)
            .err()
            .unwrap_or_else(|| panic!("{lake:?} {keys:?} was planned"));
        let messages: Vec<&str> = problems.iter().map(String::as_str).collect();
        assert!(
            messages.contains(&expected),
            "{lake:?} {keys:?}: {messages:#?}"
        );
    }
}

#[test]
fn a_lake_needs_a_data_source_with_a_bucket() {
    let plain = data_source(
        "plain-duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path: std::path::PathBuf::from("/tmp/prices.duckdb"),
            read_only: true,
        })),
    );
    let cases = [
        (
            plain.clone(),
            "source \"sales\": \"plain-duckdb\" is a DuckDB data source without a bucket, \
             and a lake is read from one; add object storage to the data source's settings, or \
             give the source a query",
        ),
        (
            ledger(),
            "source \"sales\": lake reads files in a bucket, and \"ledger\" is a SQLite \
             data source; give the source a query instead, or read the bucket through a DuckDB \
             data source",
        ),
    ];
    for (named, expected) in cases {
        let text = SALES
            .replace("{lake}", "sales/*.parquet")
            .replace("{lake_keys}", "")
            .replace(
                "data_source = \"lake\"",
                &format!("data_source = {:?}", named.name),
            )
            .replace(
                "data_source = \"ledger\"",
                &format!("data_source = {:?}", named.name),
            );
        // `named` is the source both the loaded source and the lake point at, so the list
        // holds it once: two data sources of one name are a problem of their own.
        let problems = Plan::new(
            read_report(&text).unwrap(),
            &[named],
            &[],
            &LimitDefaults::default(),
        )
        .expect_err("the lake was planned");
        let messages: Vec<&str> = problems.iter().map(String::as_str).collect();
        assert!(messages.contains(&expected), "{messages:#?}");
    }
}
