use std::path::Path;

use dbrover_core::{Connection, DriverError, describe_error};

use crate::{DuckDbType, LoadColumn, Workspace, WorkspaceLimit, WorkspaceLimits, WorkspaceStore};

const MIB: u64 = 1 << 20;

fn limits(folder: &Path, memory_limit: u64, max_temp_size: u64) -> WorkspaceLimits {
    WorkspaceLimits {
        memory_limit,
        threads: 2,
        temp_directory: folder.join("spill"),
        max_temp_size,
    }
}

/// A run's database, on a file in `folder`.
fn on_file(folder: &Path) -> WorkspaceStore {
    WorkspaceStore::File(folder.join("workspace.duckdb"))
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

/// Columns whose values choose their types.
fn text(names: &[&str]) -> Vec<LoadColumn> {
    names.iter().map(|name| LoadColumn::text(*name)).collect()
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
    let mut workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    let decimal = DuckDbType::Decimal {
        precision: 38,
        scale: 18,
    };
    // Two columns of a type known before their first value, cast as they are appended, and
    // two typed by their values once loaded.
    let columns = [
        LoadColumn::text("id"),
        LoadColumn {
            name: "quantity".to_owned(),
            known: Some(decimal),
        },
        LoadColumn::text("booked_on"),
        LoadColumn {
            name: "settled".to_owned(),
            known: Some(DuckDbType::Boolean),
        },
    ];
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
            decimal,
            DuckDbType::Date,
            DuckDbType::Boolean,
        ])
        .await
        .unwrap();
    workspace.lock(None).await.unwrap();

    let types = query(
        &mut workspace,
        "SELECT typeof(id), typeof(booked_on), typeof(settled) FROM tx LIMIT 1",
    )
    .await
    .unwrap();
    assert_eq!(types, [cells(&["BIGINT", "DATE", "BOOLEAN"])]);
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
    // The table was typed in place: no copy of it, as text or otherwise, is left.
    let tables = query(&mut workspace, "SELECT table_name FROM duckdb_tables()")
        .await
        .unwrap();
    assert_eq!(tables, [cells(&["tx"])]);
    workspace.close().await;
}

#[tokio::test]
async fn reads_a_columns_distinct_values_in_order() {
    let folder = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    let mut loader = workspace
        .loader("tx", &text(&["asset", "amount"]))
        .await
        .unwrap();
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
    let workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    let mut loader = workspace.loader("tx", &text(&["amount"])).await.unwrap();
    loader.append(vec![cells(&["twelve"])]).await.unwrap();
    let error = loader.finish(&[DuckDbType::BigInt]).await.unwrap_err();
    assert!(describe_error(&error).contains("twelve"), "{error}");
    let known = LoadColumn {
        name: "amount".to_owned(),
        known: Some(DuckDbType::Decimal {
            precision: 18,
            scale: 2,
        }),
    };
    let mut typed = workspace.loader("typed", &[known]).await.unwrap();
    let error = typed
        .append(vec![cells(&["1.50"]), cells(&["eleven"])])
        .await
        .unwrap_err();
    let message = describe_error(&error);
    assert!(
        message.contains("eleven") && message.contains("DECIMAL(18,2)"),
        "a value that doesn't fit fails as it is appended: {message}"
    );
    let short_row = workspace
        .loader("short", &text(&["a", "b"]))
        .await
        .unwrap()
        .append(vec![cells(&["1"])])
        .await
        .unwrap_err();
    assert!(describe_error(&short_row).contains("a row of 1 values"));
    let wrong_count = workspace
        .loader("other", &text(&["a"]))
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
    let mut workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    workspace.attach("prices", &path).await.unwrap();
    workspace.lock(None).await.unwrap();

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
    let mut workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    workspace.lock(None).await.unwrap();

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
    let mut spilling = Workspace::open(
        &on_file(folder.path()),
        &limits(folder.path(), 64 * MIB, 1024 * MIB),
    )
    .await
    .unwrap();
    let rows = query(&mut spilling, SORT_PAST_MEMORY).await.unwrap();
    assert_eq!(rows.len(), 1_000_000);
    let peaks = spilling.profile().await.unwrap().peaks;
    assert!(
        peaks.temp_size.is_some_and(|spilled| spilled > 0),
        "{peaks:?}"
    );
    spilling.close().await;

    let folder = tempfile::tempdir().unwrap();
    let mut full = Workspace::open(
        &on_file(folder.path()),
        &limits(folder.path(), 64 * MIB, 2 * MIB),
    )
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

#[tokio::test]
async fn reports_its_peaks_once_locked_down() {
    let folder = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    workspace.lock(None).await.unwrap();
    let rows = query(&mut workspace, "SELECT sum(range) FROM range(100000)")
        .await
        .unwrap();
    assert_eq!(rows, [cells(&["4999950000"])]);
    let peaks = workspace.profile().await.unwrap().peaks;
    assert!(peaks.memory.is_some_and(|memory| memory > 0), "{peaks:?}");
    assert_eq!(peaks.temp_size, Some(0), "nothing spills with room");
    workspace.close().await;
}

/// Two tables loaded as a run loads its sources, for a query that filters, aggregates and joins.
async fn orders_and_regions(folder: &Path) -> Workspace {
    let workspace = Workspace::open(&on_file(folder), &roomy(folder))
        .await
        .unwrap();
    let mut orders = workspace
        .loader("orders", &text(&["region", "amount", "note"]))
        .await
        .unwrap();
    let rows = (0..1000)
        .map(|index| {
            let region = if index % 2 == 0 { "EU" } else { "US" };
            cells(&[region, &index.to_string(), "n"])
        })
        .collect();
    orders.append(rows).await.unwrap();
    orders
        .finish(&[DuckDbType::Varchar, DuckDbType::BigInt, DuckDbType::Varchar])
        .await
        .unwrap();
    let mut regions = workspace
        .loader("regions", &text(&["code", "name"]))
        .await
        .unwrap();
    regions
        .append(vec![cells(&["EU", "Europe"]), cells(&["US", "Americas"])])
        .await
        .unwrap();
    regions
        .finish(&[DuckDbType::Varchar, DuckDbType::Varchar])
        .await
        .unwrap();
    workspace.lock(None).await.unwrap();
    workspace
}

const ORDERS_BY_REGION: &str = "SELECT r.name, sum(o.amount) AS total FROM orders o \
     JOIN regions r ON r.code = o.region WHERE o.region = 'EU' GROUP BY r.name";

/// Pins the profiler's metrics' names, and the JSON the workspace writes them as, to what
/// `dbrover-plan` reads (`docs/reports-at-scale.md`, RS2b).
#[tokio::test]
async fn profiles_the_last_querys_operators_as_a_plan() {
    let folder = tempfile::tempdir().unwrap();
    let mut workspace = orders_and_regions(folder.path()).await;
    let rows = query(&mut workspace, ORDERS_BY_REGION).await.unwrap();
    assert_eq!(rows, [cells(&["Europe", "249500"])]);
    let profile = workspace.profile().await.unwrap();
    let text = profile.plan.unwrap();
    let plan = dbrover_plan::PlanFormat::DuckDbJson.parse(&text).unwrap();
    assert!(plan.is_analysed(), "{text}");
    let scan = plan
        .root
        .find(&|step| step.target.as_deref() == Some("workspace.main.orders"))
        .unwrap_or_else(|| panic!("no scan of orders in {text}"));
    assert_eq!(
        scan.actual.and_then(|actual| actual.rows),
        Some(500.0),
        "{text}"
    );
    let details: Vec<(&str, &str)> = scan
        .details
        .iter()
        .map(|detail| (detail.label.as_str(), detail.value.as_str()))
        .collect();
    assert!(
        details.contains(&("Projections", "region\namount")),
        "{text}"
    );
    assert!(
        details
            .iter()
            .any(|(label, value)| *label == "Filters" && value.contains("region='EU'")),
        "{text}"
    );
    workspace.close().await;
}

#[tokio::test]
async fn explains_a_query_without_running_it() {
    let folder = tempfile::tempdir().unwrap();
    let mut workspace = orders_and_regions(folder.path()).await;
    let text = workspace
        .explain(
            "SELECT * FROM orders WHERE amount > $1",
            &[Some("10".to_owned())],
        )
        .await
        .unwrap();
    let plan = dbrover_plan::PlanFormat::DuckDbJson.parse(&text).unwrap();
    assert!(!plan.is_analysed(), "{text}");
    let scan = plan
        .root
        .find(&|step| step.target.as_deref() == Some("workspace.main.orders"))
        .unwrap_or_else(|| panic!("no scan of orders in {text}"));
    assert!(scan.estimate.rows.is_some(), "{text}");
    let missing = workspace.explain("SELECT nothing FROM orders", &[]).await;
    assert!(missing.is_err());
    workspace.close().await;
}

#[test]
fn reads_back_every_type_it_names() {
    let types = [
        DuckDbType::BigInt,
        DuckDbType::HugeInt,
        DuckDbType::Decimal {
            precision: 18,
            scale: 2,
        },
        DuckDbType::Double,
        DuckDbType::Boolean,
        DuckDbType::Date,
        DuckDbType::Timestamp,
        DuckDbType::TimestampTz,
        DuckDbType::Varchar,
    ];
    for column_type in types {
        assert_eq!(column_type.to_string().parse(), Ok(column_type));
    }
    for unknown in ["INTEGER", "DECIMAL(39,2)", "DECIMAL(2,3)", "DECIMAL(x)"] {
        assert!(unknown.parse::<DuckDbType>().is_err(), "{unknown}");
    }
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

/// More rows than `memory_limit` holds, loaded as a run loads a source: on a file, DuckDB
/// writes the table's pages out to it, and typing a column rewrites that column alone, so
/// little is spilled.
#[tokio::test]
async fn keeps_tables_larger_than_its_memory_on_its_file() {
    let folder = tempfile::tempdir().unwrap();
    let store = on_file(folder.path());
    let mut workspace = Workspace::open(&store, &limits(folder.path(), 16 * MIB, 4 * MIB))
        .await
        .unwrap();
    let mut loader = workspace
        .loader("wide", &text(&["id", "padding"]))
        .await
        .unwrap();
    for page in 0..200 {
        let rows = (0..1000)
            .map(|row| {
                let id: u64 = page * 1000 + row;
                cells(&[&id.to_string(), &noise(id)])
            })
            .collect();
        loader.append(rows).await.unwrap();
    }
    loader
        .finish(&[DuckDbType::BigInt, DuckDbType::Varchar])
        .await
        .unwrap();
    workspace.lock(None).await.unwrap();
    let rows = query(
        &mut workspace,
        "SELECT count(*), sum(length(padding)) FROM wide",
    )
    .await
    .unwrap();
    assert_eq!(rows, [cells(&["200000", "40000000"])]);
    workspace.close().await;
    let WorkspaceStore::File(file) = store else {
        unreachable!("on_file makes a file store")
    };
    let size = std::fs::metadata(file).unwrap().len();
    assert!(size > 16 * MIB, "the table is on the file: {size} bytes");
}

/// 200 hexadecimal digits that follow from `seed` but look like noise, so DuckDB can't
/// compress a column of them to little (xorshift64).
fn noise(seed: u64) -> String {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..13)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            format!("{state:016x}")
        })
        .collect::<String>()[..200]
        .to_owned()
}

#[tokio::test]
async fn refuses_a_file_that_exists_already() {
    let folder = tempfile::tempdir().unwrap();
    let store = on_file(folder.path());
    let first = Workspace::open(&store, &roomy(folder.path()))
        .await
        .unwrap();
    first.close().await;
    assert!(
        Workspace::open(&store, &roomy(folder.path()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn keeps_a_workspace_in_memory_when_asked() {
    let folder = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::open(&WorkspaceStore::Memory, &roomy(folder.path()))
        .await
        .unwrap();
    let rows = query(&mut workspace, "SELECT 1").await.unwrap();
    assert_eq!(rows, [cells(&["1"])]);
    workspace.close().await;
    let files: Vec<_> = std::fs::read_dir(folder.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, ["spill"], "nothing but the spill folder");
}

/// RS3d, DuckDB 1.5.6's `allowed_directories`: once locked, the workspace writes Parquet in
/// the one folder it was given, reads it back, and writes nowhere else, `..` included; locked
/// without one, it writes nowhere.
#[tokio::test]
async fn writes_parquet_only_in_the_folder_it_was_locked_to() {
    let folder = tempfile::tempdir().unwrap();
    let staging = folder.path().join("staging");
    let elsewhere = folder.path().join("elsewhere");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    workspace.lock(Some(&staging)).await.unwrap();
    let file = staging.join("out.parquet.partial");
    let copy = crate::copy_to_parquet(
        "SELECT range AS n, CAST(range / 4 AS DECIMAL(18,2)) AS quarter FROM range(5)\n-- the end;",
        &file,
        crate::ParquetCompression::Zstd,
    );
    let mut cursor = workspace.connection().execute(&copy).await.unwrap();
    while !cursor.is_exhausted() {
        cursor.next_page(10).await.unwrap();
    }
    assert_eq!(
        cursor.finish().await.unwrap(),
        Some(5),
        "COPY says the rows it wrote"
    );
    let rows = query(
        &mut workspace,
        &format!(
            "SELECT typeof(quarter), sum(quarter) FROM ({}) GROUP BY ALL",
            crate::read_parquet(&file)
        ),
    )
    .await
    .unwrap();
    assert_eq!(rows, [cells(&["DECIMAL(18,2)", "2.50"])]);
    for outside in [
        elsewhere.join("out.parquet"),
        staging.join("..").join("elsewhere").join("sneaked.parquet"),
    ] {
        let copy = crate::copy_to_parquet("SELECT 1", &outside, crate::ParquetCompression::Snappy);
        let error = query(&mut workspace, &copy).await.unwrap_err();
        assert!(
            describe_error(&error).contains("disabled by configuration"),
            "{outside:?}: {error}"
        );
    }
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    workspace.close().await;

    let folder = tempfile::tempdir().unwrap();
    let mut locked = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    locked.lock(None).await.unwrap();
    let copy = crate::copy_to_parquet(
        "SELECT 1",
        &folder.path().join("x.parquet"),
        crate::ParquetCompression::Zstd,
    );
    assert!(query(&mut locked, &copy).await.is_err());
    locked.close().await;
}
