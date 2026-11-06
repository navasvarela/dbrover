//! The example reports in `examples/reports/`, the first iteration's four reports
//! (`docs/reporting.md`, RP3f), run on the data their scripts make: the SQLite shop of
//! `shop.sql` and, on the test servers, the MongoDB customers of `shop-customers.js`, the
//! PostgreSQL energy flows of `energy-postgres.sql`, the DynamoDB batteries of
//! `energy-batteries.partiql` and the DuckDB prices of `energy-prices.sql`. Their CSV files, and
//! the sales-by-category chart's SVG, are compared with the ones checked in beside them,
//! so the manual's examples keep working. Set `DBROVER_UPDATE_EXPECTED=1` to write the
//! checked-in files from this run instead, after a change that is meant to change them.

// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use calamine::Reader;

use common::{
    MemorySecrets, dynamodb_test_config, mongo_test_server, postgres_test_server,
    sqlite_data_source,
};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    DynamoDbConfig, Route, SafetyLevel,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_language::QueryLanguage;
use dbrover_report::{LimitDefaults, Plan, RunFolders, read_report, run};
use dbrover_sql::Dialect;
use dbrover_store::{Secret, SecretStore, TrustedCommands};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
}

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

/// The data source `shop`, a SQLite file in `folder` made by `shop.sql`.
fn shop(folder: &Path) -> DataSource {
    let path = folder.join("shop.db");
    let script = std::fs::read_to_string(examples().join("shop.sql")).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    DataSource {
        name: "shop".to_owned(),
        ..sqlite_data_source(path)
    }
}

/// Runs every command of the example script `name` on `connection`, split as `language`
/// splits a console's script.
async fn run_script(connection: &mut dyn Connection, name: &str, language: QueryLanguage) {
    let script = std::fs::read_to_string(examples().join(name)).unwrap();
    for range in language.split(&script) {
        let command = script[range].trim();
        if command.is_empty() {
            continue;
        }
        connection
            .execute(command)
            .await
            .unwrap_or_else(|error| panic!("{command}: {error}"))
            .finish()
            .await
            .unwrap();
    }
}

/// The example `name`'s plan and values from `since` up to `until`, run on `data_sources`
/// into `out`.
async fn run_example(
    name: &str,
    period: (&str, &str),
    data_sources: &[DataSource],
    secrets: Arc<dyn SecretStore>,
    out: &Path,
) {
    let text = std::fs::read_to_string(examples().join(format!("{name}.toml"))).unwrap();
    let plan = Plan::new(
        read_report(&text).unwrap(),
        data_sources,
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan
        .values(
            &[("since", period.0), ("until", period.1)]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .unwrap();
    let tunnels = Tunnels::new(out.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = out.join(".run");
    let folders = RunFolders {
        folder: out,
        staging: &staging,
    };
    run(&plan, &values, &access, folders, &mut |_| {})
        .await
        .unwrap_or_else(|failure| panic!("{name}: {failure}"));
}

const OCTOBER: (&str, &str) = ("2026-10-01", "2026-11-01");
const OCTOBER_TIMES: (&str, &str) = ("2026-10-01 00:00", "2026-11-01 00:00");

/// Whether `file` in `out` is the one checked in under `expected/`.
fn compare(out: &Path, file: &str) {
    let written = std::fs::read_to_string(out.join(file)).unwrap();
    let expected_path = examples().join("expected").join(file);
    if std::env::var_os("DBROVER_UPDATE_EXPECTED").is_some() {
        std::fs::write(&expected_path, &written).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&expected_path).unwrap();
    // Git may have checked the file out with the platform's line endings; the writer's are
    // CSV's own, CRLF.
    let expected = expected.replace("\r\n", "\n").replace('\n', "\r\n");
    assert!(
        written == expected,
        "{file} differs from {}; run with DBROVER_UPDATE_EXPECTED=1 if that is meant",
        expected_path.display()
    );
}

/// Whether the chart `file` in `out` is, byte for byte, the SVG checked in under `expected/`:
/// the same rows make the same picture on every platform (`docs/charts-as-code.md`, CH3).
fn compare_chart(out: &Path, file: &str) {
    let written = std::fs::read_to_string(out.join(file)).unwrap();
    let expected_path = examples().join("expected").join(file);
    if std::env::var_os("DBROVER_UPDATE_EXPECTED").is_some() {
        std::fs::write(&expected_path, &written).unwrap();
        return;
    }
    // Git may have checked the file out with the platform's line endings; the chart's are LF.
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        written == expected,
        "{file} differs from {}; run with DBROVER_UPDATE_EXPECTED=1 if that is meant",
        expected_path.display()
    );
}

#[tokio::test]
async fn the_sales_by_category_example_matches_its_checked_in_file() {
    let folder = tempfile::tempdir().unwrap();
    let shop = shop(folder.path());
    run_example(
        "sales-by-category",
        OCTOBER,
        &[shop],
        Arc::new(MemorySecrets::default()),
        folder.path(),
    )
    .await;
    compare(folder.path(), "sales-by-category-2026-10.csv");
    compare_chart(folder.path(), "sales-by-category-2026-10.svg");
    assert!(
        folder
            .path()
            .join("sales-by-category-2026-10.xlsx")
            .is_file()
    );
}

/// The data source `crm`, on the MongoDB test server, with the customers of
/// `shop-customers.js`, and its password saved in `secrets`.
async fn crm(secrets: &MemorySecrets) -> Option<DataSource> {
    let (config, password) = mongo_test_server("dbrover_report_examples")?;
    let mut mongo = dbrover_driver_mongodb::connect(&config, Some(&password), Route::Direct)
        .await
        .unwrap();
    run_script(&mut mongo, "shop-customers.js", QueryLanguage::Mongo).await;
    let crm = data_source("crm", ConnectionConfig::Mongo(config));
    secrets.set(crm.id, Secret::Database, &password).unwrap();
    Some(crm)
}

/// Needs the MongoDB test server, where it replaces the collection `customers`.
#[tokio::test]
async fn the_order_history_example_matches_its_checked_in_file() {
    let secrets = Arc::new(MemorySecrets::default());
    let Some(crm) = crm(&secrets).await else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let shop = shop(folder.path());
    run_example(
        "order-history",
        OCTOBER,
        &[shop, crm],
        secrets,
        folder.path(),
    )
    .await;
    compare(folder.path(), "order-history-2026-10.csv");
    assert!(folder.path().join("order-history-2026-10.xlsx").is_file());
}

/// The data source `prices-duckdb`: a DuckDB file in `folder` made by `energy-prices.sql`,
/// read-only, as the example says to set it.
async fn prices(folder: &Path) -> DataSource {
    let path = folder.join("prices.duckdb");
    dbrover_driver_duckdb::create_database(&path).await.unwrap();
    let mut file = dbrover_driver_duckdb::connect(&DuckDbConfig::new(DuckDbDatabase::File {
        path: path.clone(),
        read_only: false,
    }))
    .await
    .unwrap();
    run_script(
        &mut file,
        "energy-prices.sql",
        QueryLanguage::Sql(Dialect::DuckDb),
    )
    .await;
    Box::new(file).close().await.unwrap();
    data_source(
        "prices-duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path,
            read_only: true,
        })),
    )
}

/// The table `energy_batteries` on DynamoDB Local, made afresh as `energy-batteries.partiql`
/// says, with its items.
async fn batteries(config: DynamoDbConfig) -> DataSource {
    use aws_sdk_dynamodb::types::{
        AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
    };
    let sign_in = dbrover_cloud::aws::AwsSignIn {
        credentials: dbrover_cloud::aws::AwsCredentials::LocalEmulator,
        region: None,
        endpoint: Some(config.endpoint.clone()),
    };
    let client = aws_sdk_dynamodb::Client::new(&sign_in.sdk_config().await.unwrap());
    // Left by an earlier run, or absent: either way it is made again below.
    let _ = client
        .delete_table()
        .table_name("energy_batteries")
        .send()
        .await;
    client
        .create_table()
        .table_name("energy_batteries")
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("battery_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("battery_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    let mut dynamo = dbrover_driver_dynamodb::connect(&config).await.unwrap();
    run_script(
        &mut dynamo,
        "energy-batteries.partiql",
        QueryLanguage::PartiQl,
    )
    .await;
    data_source("batteries-dynamodb", ConnectionConfig::DynamoDb(config))
}

/// Needs the PostgreSQL test server, where it replaces the schema `energy`, and DynamoDB
/// Local, where it replaces the table `energy_batteries`, as the examples' scripts do. The
/// battery flows and cycles share them, so they run in one test, one after the other.
#[tokio::test]
async fn the_battery_flows_and_cycles_examples_match_their_checked_in_files() {
    let (Some(server), Some(dynamodb)) = (postgres_test_server(), dynamodb_test_config()) else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let mut admin =
        dbrover_driver_postgres::connect(&server.config, Some(&server.password), Route::Direct)
            .await
            .unwrap();
    run_script(
        &mut admin,
        "energy-postgres.sql",
        QueryLanguage::Sql(Dialect::Postgres),
    )
    .await;
    let energy = data_source(
        "energy-postgres",
        ConnectionConfig::Postgres(server.config.clone()),
    );
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set(energy.id, Secret::Database, &server.password)
        .unwrap();
    let data_sources = [
        energy,
        batteries(dynamodb).await,
        prices(folder.path()).await,
    ];

    run_example(
        "battery-flows",
        OCTOBER_TIMES,
        &data_sources,
        secrets.clone(),
        folder.path(),
    )
    .await;
    compare(folder.path(), "battery-flows-2026-10.csv");
    assert!(folder.path().join("battery-flows-2026-10.xlsx").is_file());

    run_example(
        "battery-cycles",
        OCTOBER_TIMES,
        &data_sources,
        secrets.clone(),
        folder.path(),
    )
    .await;
    compare(folder.path(), "battery-cycles-2026-10.csv");
    assert!(folder.path().join("battery-cycles-2026-10.xlsx").is_file());

    // Every flow priced, from the first: what FIFO is worked out from, by hand.
    let history = folder.path().join("history");
    std::fs::create_dir(&history).unwrap();
    run_example(
        "battery-flows",
        ("2026-09-01 00:00", "2026-12-01 00:00"),
        &data_sources,
        secrets,
        &history,
    )
    .await;
    let flows = std::fs::read_to_string(history.join("battery-flows-2026-09.csv")).unwrap();
    let report = std::fs::read_to_string(folder.path().join("battery-cycles-2026-10.csv")).unwrap();
    assert_eq!(
        cycle_rows(&report),
        fifo(&flows, OCTOBER_TIMES),
        "the battery cycles report differs from FIFO worked out row by row"
    );
}

/// Places an energy is kept to, a price, and their product.
const QUANTITY_SCALE: u32 = 18;
const PRICE_SCALE: u32 = 8;
const PRODUCT_SCALE: u32 = QUANTITY_SCALE + PRICE_SCALE;

/// A decimal's digits as a whole number of `10^-scale`, exactly: `"-1.5"` at scale 2 is
/// `-150`. Panics on more places than `scale`, which no column here has.
fn fixed(text: &str, scale: u32) -> i128 {
    let (negative, digits) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    assert!(fraction.len() <= scale as usize, "{text} at scale {scale}");
    let padded = format!("{whole}{fraction:0<width$}", width = scale as usize);
    let value: i128 = padded.parse().unwrap();
    if negative { -value } else { value }
}

/// A discharge's draw on one charge: when, which battery, the charge's time, the energy and both
/// prices in their scales, and the charge cost, discharge value and difference at the product's.
type Match = (String, String, String, i128, i128, i128, i128, i128, i128);

/// The battery cycles CSV's rows, as numbers in their scales.
fn cycle_rows(csv: &str) -> Vec<Match> {
    csv.lines()
        .skip(1)
        .map(|line| {
            let cells: Vec<&str> = line.split(',').collect();
            (
                cells[0].to_owned(),
                cells[1].to_owned(),
                cells[2].to_owned(),
                fixed(cells[3], QUANTITY_SCALE),
                fixed(cells[4], PRICE_SCALE),
                fixed(cells[5], PRICE_SCALE),
                fixed(cells[6], PRODUCT_SCALE),
                fixed(cells[7], PRODUCT_SCALE),
                fixed(cells[8], PRODUCT_SCALE),
            )
        })
        .collect()
}

/// FIFO worked out one flow at a time from the battery flows' rows (time, battery, site, energy,
/// price, value), keeping the matches of discharges in `period`: each battery's charges in a
/// queue, and each discharge drawing from the front until it is used up.
fn fifo(flows: &str, period: (&str, &str)) -> Vec<Match> {
    struct Charge {
        charged_at: String,
        left: i128,
        price: i128,
    }
    let mut charges: BTreeMap<String, VecDeque<Charge>> = BTreeMap::new();
    let mut matches = Vec::new();
    for line in flows.lines().skip(1) {
        let cells: Vec<&str> = line.split(',').collect();
        let (at, battery) = (cells[0].to_owned(), cells[1].to_owned());
        let energy = fixed(cells[3], QUANTITY_SCALE);
        let price = fixed(cells[4], PRICE_SCALE);
        let queue = charges.entry(battery.clone()).or_default();
        if energy > 0 {
            queue.push_back(Charge {
                charged_at: at,
                left: energy,
                price,
            });
            continue;
        }
        let mut to_draw = -energy;
        while to_draw > 0 {
            let charge = queue
                .front_mut()
                .expect("a discharge never exceeds the battery's charge");
            let drawn = charge.left.min(to_draw);
            charge.left -= drawn;
            to_draw -= drawn;
            let in_period = at.as_str() >= period.0 && at.as_str() < period.1;
            if in_period {
                let (cost, value) = (drawn * charge.price, drawn * price);
                matches.push((
                    at.clone(),
                    battery.clone(),
                    charge.charged_at.clone(),
                    drawn,
                    charge.price,
                    price,
                    cost,
                    value,
                    value - cost,
                ));
            }
            if charge.left == 0 {
                queue.pop_front();
            }
        }
    }
    matches
}

/// The order history past an Excel sheet: 1,100,000 order lines in October, every one written
/// to CSV, the workbook going on to a second sheet, and each customer's last running total
/// their sum. Needs the MongoDB test server for the customers. Slow: a debug build takes
/// minutes, so it is run by hand before a release.
#[tokio::test]
#[ignore = "slow: writes 1,100,000 rows to CSV and Excel; run with --ignored"]
async fn the_order_history_writes_past_a_million_rows() {
    const LINES: i64 = 1_100_000;
    let secrets = Arc::new(MemorySecrets::default());
    let Some(crm) = crm(&secrets).await else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let shop = shop(folder.path());
    let ConnectionConfig::Sqlite(config) = &shop.config else {
        unreachable!("shop() makes a SQLite data source");
    };
    let database = rusqlite::Connection::open(&config.path).unwrap();
    let october_lines = || -> i64 {
        database
            .query_row(
                "SELECT count(*) FROM order_lines WHERE ordered_on >= '2026-10-01' \
                 AND ordered_on < '2026-11-01'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    // Lines on the October days, each in whole cents, so SQLite sums them exactly.
    database
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {})
             INSERT INTO order_lines (order_no, product_id, ordered_on, customer_id, quantity,
                                      amount)
             SELECT 100000 + i / 2, i % 7 + 1,
                    date('2026-10-01', '+' || (i % 31) || ' days'), printf('cust-%02d', i % 12 + 1),
                    1, (i % 9973) / 100.0
             FROM n",
            LINES - october_lines()
        ))
        .unwrap();
    let in_october = u64::try_from(october_lines()).unwrap();
    assert!(in_october > 1_048_576, "{in_october}");
    let mut sums = database
        .prepare(
            "SELECT customer_id, SUM(CAST(round(amount * 100) AS INTEGER)) \
             FROM order_lines \
             WHERE ordered_on >= '2026-10-01' AND ordered_on < '2026-11-01' \
             GROUP BY customer_id",
        )
        .unwrap();
    let expected: BTreeMap<String, i128> = sums
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)?)))
        .unwrap()
        .map(|row| {
            let (customer, cents) = row.unwrap();
            (customer, i128::from(cents))
        })
        .collect();

    run_example(
        "order-history",
        OCTOBER,
        &[shop, crm],
        secrets,
        folder.path(),
    )
    .await;
    let csv = std::fs::read_to_string(folder.path().join("order-history-2026-10.csv")).unwrap();
    let mut last: BTreeMap<String, i128> = BTreeMap::new();
    let mut rows = 0_u64;
    for line in csv.lines().skip(1) {
        rows += 1;
        let cells: Vec<&str> = line.split(',').collect();
        let total = cells.last().unwrap();
        last.insert(cells[2].to_owned(), fixed(total, 2));
    }
    assert_eq!(rows, in_october);
    assert_eq!(last, expected);
    let book: calamine::Xlsx<_> =
        calamine::open_workbook(folder.path().join("order-history-2026-10.xlsx")).unwrap();
    assert_eq!(book.sheet_names(), ["Order history", "Order history 2"]);
}
