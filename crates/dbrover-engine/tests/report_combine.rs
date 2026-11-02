//! Reports with a combine step on real data sources (`docs/reporting.md`, RP2d): a SQLite
//! source loaded beside an attached DuckDB file, and, on the PostgreSQL test server, the
//! valuation of 18-decimal quantities at hourly prices, written to the last digit.
// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, postgres_test_server, sqlite_data_source};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, Route,
    SafetyLevel,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{Plan, RunEvent, RunFolders, read_report, run};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

async fn execute(connection: &mut dyn Connection, statements: &[&str]) {
    for statement in statements {
        connection
            .execute(statement)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"))
            .finish()
            .await
            .unwrap();
    }
}

/// The data source `prices`: a DuckDB file of hourly prices, read-only, as a report's
/// attached file would be set up.
async fn prices(folder: &Path) -> DataSource {
    let path = folder.join("prices.duckdb");
    dbrover_driver_duckdb::create_database(&path).await.unwrap();
    let mut file = dbrover_driver_duckdb::connect(&DuckDbConfig {
        database: DuckDbDatabase::File {
            path: path.clone(),
            read_only: false,
        },
    })
    .await
    .unwrap();
    execute(
        &mut file,
        &[
            "CREATE TABLE hourly_prices (asset VARCHAR, hour TIMESTAMP, price_usd DECIMAL(38,8))",
            "INSERT INTO hourly_prices VALUES \
             ('BTC', '2026-10-01 10:00', 61234.12345678), \
             ('BTC', '2026-10-01 11:00', 61300.5), \
             ('ETH', '2026-10-01 10:00', 2345.6789)",
        ],
    )
    .await;
    Box::new(file).close().await.unwrap();
    DataSource {
        id: DataSourceId::generate(),
        name: "prices".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::File {
                path,
                read_only: true,
            },
        }),
    }
}

/// Values each transaction at its asset's price in the hour it ran. `{transactions}` names
/// the transactions' table.
const VALUATION: &str = r##"
[report]
name = "Valuation"
params = { since = "timestamp", until = "timestamp" }

[[source]]
id = "tx"
data_source = "ledger"
query = """
SELECT id, asset, quantity, executed_at FROM {transactions}
WHERE executed_at >= :since AND executed_at < :until
"""

[[source]]
id = "prices"
data_source = "prices"
attach = true

[combine]
query = """
SELECT tx.id, tx.asset, tx.quantity, p.price_usd, tx.quantity * p.price_usd AS value_usd
FROM tx
ASOF JOIN prices.hourly_prices p ON p.asset = tx.asset AND p.hour <= tx.executed_at
ORDER BY tx.id
"""
threads = 2

[[output]]
format = "csv"
file = "valuation.csv"
columns = [
  { name = "Id",          from = "id" },
  { name = "Asset",       from = "asset" },
  { name = "Quantity",    from = "quantity",  type = "decimal" },
  { name = "Price (USD)", from = "price_usd", type = "decimal(38,8)" },
  { name = "Value (USD)", from = "value_usd", type = "decimal" },
]

[[output]]
format = "xlsx"
file = "valuation.xlsx"
columns = [
  { name = "Id",          from = "id",        type = "integer" },
  { name = "Value (USD)", from = "value_usd", type = "decimal(38,2)", format = "#,##0.00" },
]
"##;

const EXPECTED: &str = "Id,Asset,Quantity,Price (USD),Value (USD)\r\n\
    1,BTC,1.234567890123456789,61234.12345678,75597.68259959615905340363907942\r\n\
    2,BTC,0.500000000000000000,61300.50000000,30650.25000000000000000000000000\r\n\
    3,ETH,250.500000000000000000,2345.67890000,587592.56445000000000000000000000\r\n";

/// Runs the valuation for October 2026 with `ledger` and `prices`, into `out`, and returns
/// the run's events.
async fn run_valuation(
    transactions: &str,
    data_sources: &[DataSource],
    secrets: Arc<dyn SecretStore>,
    out: &Path,
) -> Vec<RunEvent> {
    let text = VALUATION.replace("{transactions}", transactions);
    let plan = Plan::new(read_report(&text).unwrap(), data_sources, &[]).unwrap();
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
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
    let mut events = Vec::new();
    run(&plan, &values, &access, folders, &mut |event| {
        events.push(event)
    })
    .await
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert!(!staging.exists());
    events
}

#[tokio::test]
async fn combines_a_sqlite_source_with_an_attached_duckdb_file() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("ledger.db");
    // `quantity` has no declared type, so SQLite keeps each value as the text it was given
    // rather than as a float, and the report types it from its values.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE transactions (id INTEGER, asset TEXT, quantity, executed_at TEXT);
             INSERT INTO transactions VALUES
               (1, 'BTC', '1.234567890123456789', '2026-10-01 10:15:00'),
               (2, 'BTC', '0.500000000000000000', '2026-10-01 11:30:00'),
               (3, 'ETH', '250.500000000000000000', '2026-10-02 09:00:00'),
               (4, 'ETH', '1.000000000000000000', '2026-11-02 09:00:00');",
        )
        .unwrap();
    let ledger = DataSource {
        name: "ledger".to_owned(),
        ..sqlite_data_source(path)
    };
    let prices = prices(folder.path()).await;
    let out = folder.path().join("out");
    let events = run_valuation(
        "transactions",
        &[ledger, prices.clone()],
        Arc::new(MemorySecrets::default()),
        &out,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(out.join("valuation.csv")).unwrap(),
        EXPECTED
    );
    assert!(out.join("valuation.xlsx").is_file());
    let ConnectionConfig::DuckDb(DuckDbConfig {
        database: DuckDbDatabase::File { path, .. },
    }) = prices.config
    else {
        unreachable!("prices is a DuckDB file");
    };
    assert!(events.contains(&RunEvent::SourceAttached {
        id: "prices".to_owned(),
        path,
    }));
}

#[tokio::test]
async fn values_postgres_transactions_at_duckdb_prices() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let mut admin =
        dbrover_driver_postgres::connect(&server.config, Some(&server.password), Route::Direct)
            .await
            .unwrap();
    let schema = format!(
        "dbrover_valuation_{}",
        DataSourceId::generate().to_string().replace('-', "_")
    );
    execute(
        &mut admin,
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!(
                "CREATE TABLE {schema}.transactions (id int, asset text, \
                 quantity numeric(38,18), executed_at timestamp)"
            ),
            &format!(
                "INSERT INTO {schema}.transactions VALUES \
                 (1, 'BTC', 1.234567890123456789, '2026-10-01 10:15:00'), \
                 (2, 'BTC', 0.5, '2026-10-01 11:30:00'), \
                 (3, 'ETH', 250.5, '2026-10-02 09:00:00'), \
                 (4, 'ETH', 1, '2026-11-02 09:00:00')"
            ),
        ],
    )
    .await;
    let ledger = DataSource {
        id: DataSourceId::generate(),
        name: "ledger".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config.clone()),
    };
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set(ledger.id, Secret::Database, &server.password)
        .unwrap();
    let prices = prices(folder.path()).await;
    let out: PathBuf = folder.path().join("out");
    let events = run_valuation(
        &format!("{schema}.transactions"),
        &[ledger, prices],
        secrets,
        &out,
    )
    .await;
    execute(&mut admin, &[&format!("DROP SCHEMA {schema} CASCADE")]).await;

    assert_eq!(
        std::fs::read_to_string(out.join("valuation.csv")).unwrap(),
        EXPECTED
    );
    let quantity = events.iter().find_map(|event| match event {
        RunEvent::SourceDone { table, .. } => table
            .iter()
            .find(|column| column.name == "quantity")
            .map(|column| column.type_name.clone()),
        _ => None,
    });
    // PostgreSQL's text protocol says nothing of `numeric(38,18)`, and every value has 18
    // places: the values make it a decimal of just the digits they need.
    assert_eq!(quantity.as_deref(), Some("DECIMAL(21,18)"));
}
