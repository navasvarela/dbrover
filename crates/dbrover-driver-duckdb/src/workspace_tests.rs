use std::path::Path;

use dbrover_core::{Connection, DriverError, describe_error};

use crate::{DuckDbType, Workspace, WorkspaceLimit, WorkspaceLimits};

const MIB: u64 = 1 << 20;

fn limits(folder: &Path, memory_limit: u64, max_temp_size: u64) -> WorkspaceLimits {
    WorkspaceLimits {
        memory_limit,
        threads: 2,
        temp_directory: folder.join("spill"),
        max_temp_size,
    }
}

fn roomy(folder: &Path) -> WorkspaceLimits {
    limits(folder, 256 * MIB, 256 * MIB)
}

async fn query(
    workspace: &mut Workspace,
    sql: &str,
) -> Result<Vec<Vec<Option<String>>>, DriverError> {
    let connection = workspace.connection();
    let mut cursor = connection.execute(sql).await?;
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(1000).await?);
    }
    cursor.finish().await?;
    Ok(rows)
}

fn cells(values: &[&str]) -> Vec<Option<String>> {
    values
        .iter()
        .map(|value| Some((*value).to_owned()))
        .collect()
}

#[tokio::test]
async fn loads_text_into_typed_columns_with_every_digit() {
    let folder = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::open(&roomy(folder.path())).await.unwrap();
    let columns = ["id", "quantity", "booked_on", "settled"].map(str::to_owned);
    let mut loader = workspace.loader("tx", &columns).await.unwrap();
    loader
        .append(vec![
            cells(&[
                "1",
                "12345678901234567890.123456789012345678",
                "2026-10-01",
                "t",
            ]),
            vec![Some("2".to_owned()), None, None, Some("f".to_owned())],
        ])
        .await
        .unwrap();
    loader
        .append(vec![cells(&[
            "3",
            "-0.000000000000000001",
            "2026-10-31",
            "true",
        ])])
        .await
        .unwrap();
    loader
        .finish(&[
            DuckDbType::BigInt,
            DuckDbType::Decimal {
                precision: 38,
                scale: 18,
            },
            DuckDbType::Date,
            DuckDbType::Boolean,
        ])
        .await
        .unwrap();
    workspace.lock().await.unwrap();

    let rows = query(
        &mut workspace,
        "SELECT typeof(quantity), quantity * 2, booked_on, settled FROM tx ORDER BY id",
    )
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            cells(&[
                "DECIMAL(38,18)",
                "24691357802469135780.246913578024691356",
                "2026-10-01",
                "true",
            ]),
            vec![
                Some("DECIMAL(38,18)".to_owned()),
                None,
                None,
                Some("false".to_owned()),
            ],
            cells(&[
                "DECIMAL(38,18)",
                "-0.000000000000000002",
                "2026-10-31",
                "true",
            ]),
        ]
    );
    // The text the rows arrived as is gone once the run's query starts.
    let schemas = query(
        &mut workspace,
        "SELECT schema_name FROM duckdb_schemas() WHERE schema_name = 'dbrover_load'",
    )
    .await
    .unwrap();
    assert!(schemas.is_empty(), "{schemas:?}");
    workspace.close().await;
}

#[tokio::test]
async fn reads_a_columns_distinct_values_in_order() {
    let folder = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&roomy(folder.path())).await.unwrap();
    let columns = ["asset", "amount"].map(str::to_owned);
    let mut loader = workspace.loader("tx", &columns).await.unwrap();
    loader
        .append(vec![
            cells(&["eth", "10"]),
            cells(&["btc", "9"]),
            vec![None, Some("100".to_owned())],
            cells(&["eth", "9"]),
        ])
        .await
        .unwrap();
    loader
        .finish(&[DuckDbType::Varchar, DuckDbType::BigInt])
        .await
        .unwrap();
    assert_eq!(
        workspace.distinct_values("tx", "asset").await.unwrap(),
        ["btc", "eth"]
    );
    assert_eq!(
        workspace.distinct_values("tx", "amount").await.unwrap(),
        ["9", "10", "100"],
        "numbers in their order, not the text's"
    );
    assert!(workspace.distinct_values("tx", "missing").await.is_err());
    workspace.close().await;
}

#[tokio::test]
async fn names_the_value_that_does_not_fit_its_type() {
    let folder = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&roomy(folder.path())).await.unwrap();
    let mut loader = workspace
        .loader("tx", &["amount".to_owned()])
        .await
        .unwrap();
    loader.append(vec![cells(&["twelve"])]).await.unwrap();
    let error = loader.finish(&[DuckDbType::BigInt]).await.unwrap_err();
    assert!(describe_error(&error).contains("twelve"), "{error}");
    let wrong_count = workspace
        .loader("other", &["a".to_owned()])
        .await
        .unwrap()
        .finish(&[])
        .await
        .unwrap_err();
    assert!(describe_error(&wrong_count).contains("0 types for the 1 columns"));
    workspace.close().await;
}

#[tokio::test]
async fn attaches_a_file_read_only() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("prices.duckdb");
    {
        let file = duckdb::Connection::open(&path).unwrap();
        file.execute_batch(
            "CREATE TABLE hourly_prices (asset VARCHAR, price_usd DECIMAL(38,8)); \
             INSERT INTO hourly_prices VALUES ('BTC', 61234.12345678)",
        )
        .unwrap();
    }
    let mut workspace = Workspace::open(&roomy(folder.path())).await.unwrap();
    workspace.attach("prices", &path).await.unwrap();
    workspace.lock().await.unwrap();

    let rows = query(
        &mut workspace,
        "SELECT asset, price_usd FROM prices.hourly_prices",
    )
    .await
    .unwrap();
    assert_eq!(rows, vec![cells(&["BTC", "61234.12345678"])]);
    let write = query(
        &mut workspace,
        "INSERT INTO prices.hourly_prices VALUES ('ETH', 1)",
    )
    .await
    .unwrap_err();
    assert!(describe_error(&write).contains("read-only"), "{write}");

    let missing = workspace
        .attach("other", &folder.path().join("missing.duckdb"))
        .await
        .unwrap_err();
    assert!(describe_error(&missing).contains("doesn't exist"));
    workspace.close().await;
}

#[tokio::test]
async fn once_locked_reads_no_file_and_keeps_its_limits() {
    let folder = tempfile::tempdir().unwrap();
    let outside = folder.path().join("outside.csv");
    std::fs::write(&outside, "a\n1\n").unwrap();
    let mut workspace = Workspace::open(&roomy(folder.path())).await.unwrap();
    workspace.lock().await.unwrap();

    let attempts = [
        format!("SELECT * FROM read_csv('{}')", outside.display()),
        format!(
            "COPY (SELECT 1) TO '{}'",
            folder.path().join("out.csv").display()
        ),
        "SET memory_limit = '64GiB'".to_owned(),
        "LOAD httpfs".to_owned(),
        format!(
            "ATTACH '{}' AS other",
            folder.path().join("new.duckdb").display()
        ),
    ];
    for sql in attempts {
        let refused = query(&mut workspace, &sql).await;
        assert!(refused.is_err(), "{sql} ran");
    }
    assert!(!folder.path().join("out.csv").exists());
    let limit = query(&mut workspace, "SELECT current_setting('memory_limit')")
        .await
        .unwrap();
    assert_eq!(limit, vec![cells(&["256.0 MiB"])]);
    workspace.close().await;
}

/// Sorting this many wide rows needs far more memory than the small limits below.
const SORT_PAST_MEMORY: &str = "SELECT range, repeat('x', 100) AS padding \
                                FROM range(1000000) ORDER BY hash(range)";

#[tokio::test]
async fn spills_past_its_memory_limit_and_stops_at_its_temp_size() {
    let folder = tempfile::tempdir().unwrap();
    let mut spilling = Workspace::open(&limits(folder.path(), 64 * MIB, 1024 * MIB))
        .await
        .unwrap();
    let rows = query(&mut spilling, SORT_PAST_MEMORY).await.unwrap();
    assert_eq!(rows.len(), 1_000_000);
    spilling.close().await;

    let mut full = Workspace::open(&limits(folder.path(), 64 * MIB, 2 * MIB))
        .await
        .unwrap();
    let error = query(&mut full, SORT_PAST_MEMORY).await.unwrap_err();
    assert_eq!(
        WorkspaceLimit::of(&error),
        Some(WorkspaceLimit::TempDirectory),
        "{}",
        describe_error(&error)
    );
    full.close().await;
}

#[test]
fn names_types_as_duckdb_casts_to_them() {
    let names = [
        (DuckDbType::BigInt, "BIGINT"),
        (DuckDbType::HugeInt, "HUGEINT"),
        (
            DuckDbType::Decimal {
                precision: 20,
                scale: 2,
            },
            "DECIMAL(20,2)",
        ),
        (DuckDbType::Double, "DOUBLE"),
        (DuckDbType::Boolean, "BOOLEAN"),
        (DuckDbType::Date, "DATE"),
        (DuckDbType::Timestamp, "TIMESTAMP"),
        (DuckDbType::TimestampTz, "TIMESTAMP WITH TIME ZONE"),
        (DuckDbType::Varchar, "VARCHAR"),
    ];
    for (duckdb_type, name) in names {
        assert_eq!(duckdb_type.to_string(), name);
    }
}

#[test]
fn tells_the_memory_limit_from_the_temp_size() {
    let error = |message: &str| DriverError::Statement {
        source: Box::new(std::io::Error::other(message.to_owned())),
        position: None,
        sqlstate: None,
    };
    let cases = [
        (
            "Out of Memory Error: failed to pin block of size 256.0 KiB (31.9 MiB/32.0 MiB used)",
            Some(WorkspaceLimit::Memory),
        ),
        (
            "Out of Memory Error: failed to offload data block of size 256.0 KiB (2.0 MiB/2.0 \
             MiB used). This limit was set by the 'max_temp_directory_size' setting.",
            Some(WorkspaceLimit::TempDirectory),
        ),
        ("Binder Error: column \"x\" not found", None),
    ];
    for (message, limit) in cases {
        assert_eq!(WorkspaceLimit::of(&error(message)), limit, "{message}");
    }
}
