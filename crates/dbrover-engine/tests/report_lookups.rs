//! Reports whose sources read by an earlier source's keys and declare their columns, on real
//! data sources (`docs/reporting.md`, RP3d): DynamoDB Local read by its partition key under
//! the cost guard, and MongoDB's documents taken into declared columns. Each needs its test
//! server, `DBROVER_TEST_DYNAMODB_URL` or `DBROVER_TEST_MONGODB_URL`, and is skipped without.
// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{
    MemorySecrets, create_dynamodb_table, dynamodb_test_config, mongo_test_server,
    sqlite_data_source,
};
use dbrover_core::{Connection, ConnectionConfig, DataSource, DataSourceId, Route, SafetyLevel};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{
    LimitDefaults, Plan, RunFailure, RunFolders, RunSummary, Step, read_report, run,
};
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

/// A SQLite file in `folder` with the keys `keys` in the table `keys`, as the data source
/// `ledger`.
fn ledger(folder: &Path, keys: &[&str]) -> DataSource {
    let path = folder.join("ledger.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE keys (key TEXT NOT NULL)")
        .unwrap();
    for key in keys {
        connection
            .execute("INSERT INTO keys (key) VALUES (?1)", [key])
            .unwrap();
    }
    DataSource {
        name: "ledger".to_owned(),
        ..sqlite_data_source(path)
    }
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

async fn run_report(
    text: &str,
    data_sources: &[DataSource],
    secrets: Arc<dyn SecretStore>,
    out: &Path,
) -> Result<RunSummary, RunFailure> {
    let plan = Plan::new(
        read_report(text).unwrap(),
        data_sources,
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan.values(&Default::default()).unwrap();
    let tunnels = Tunnels::new(out.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging: PathBuf = out.join(".run");
    let work = staging.with_extension("work");
    run(
        &plan,
        &values,
        &access,
        RunFolders {
            folder: out,
            staging: &staging,
            work: &work,
        },
        &mut |_| {},
    )
    .await
}

/// Assets read from DynamoDB by the keys a SQLite source found. `{table}` names the table,
/// `{key}` the attribute the lookup reads by.
const ASSETS: &str = r#"
[report]
name = "Assets"

[[source]]
id = "keys"
data_source = "ledger"
query = "SELECT key FROM keys"

[[source]]
id = "assets"
data_source = "assets-dynamodb"
lookup = { from = "keys", column = "key", as = "keys", batch_size = 2 }
query = 'SELECT pk, "symbol", unit, metadata FROM "{table}" WHERE {key} IN :keys'
columns = [
  { name = "pk" },
  { name = "symbol" },
  { name = "unit",  type = "decimal(38,18)" },
  { name = "chain", from = "metadata.chain", default = "unknown" },
]

[combine]
query = "SELECT * FROM assets ORDER BY pk"

[[output]]
format = "csv"
file = "assets.csv"
"#;

/// A lookup on the partition key reads by key in batches, which the cost guard lets through;
/// one on another attribute is a scan, which it refuses, since a run can't ask.
#[tokio::test]
async fn reads_dynamodb_by_its_partition_key_under_the_cost_guard() {
    let Some(config) = dynamodb_test_config() else {
        return;
    };
    let table = tokio::task::spawn_blocking({
        let config = config.clone();
        move || create_dynamodb_table(&config, "report-assets")
    })
    .await
    .unwrap();
    let mut dynamo = dbrover_driver_dynamodb::connect(&config).await.unwrap();
    let inserts: Vec<String> = [
        "{'pk': 'btc', 'symbol': 'BTC', 'unit': 0.00000001, 'metadata': {'chain': 'bitcoin'}}",
        "{'pk': 'eth', 'symbol': 'ETH', 'unit': 0.000000000000000001, 'metadata': {'chain': 'ethereum'}}",
        "{'pk': 'sol', 'symbol': 'SOL', 'unit': 0.000000001}",
        "{'pk': 'doge', 'symbol': 'DOGE', 'unit': 0.00000001}",
    ]
    .iter()
    .map(|item| format!("INSERT INTO \"{table}\" VALUE {item}"))
    .collect();
    let inserts: Vec<&str> = inserts.iter().map(String::as_str).collect();
    execute(&mut dynamo, &inserts).await;

    let folder = tempfile::tempdir().unwrap();
    let data_sources = [
        ledger(folder.path(), &["eth", "btc", "sol", "btc", "xrp"]),
        data_source("assets-dynamodb", ConnectionConfig::DynamoDb(config)),
    ];
    let by_key = ASSETS.replace("{table}", &table).replace("{key}", "pk");
    let summary = run_report(
        &by_key,
        &data_sources,
        Arc::new(MemorySecrets::default()),
        folder.path(),
    )
    .await
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(summary.rows, 3);
    assert_eq!(
        std::fs::read_to_string(folder.path().join("assets.csv")).unwrap(),
        "pk,symbol,unit,chain\r\n\
         btc,BTC,0.000000010000000000,bitcoin\r\n\
         eth,ETH,0.000000000000000001,ethereum\r\n\
         sol,SOL,0.000000001000000000,unknown\r\n"
    );

    let by_symbol = ASSETS
        .replace("{table}", &table)
        .replace("{key}", "\"symbol\"");
    let failure = run_report(
        &by_symbol,
        &data_sources,
        Arc::new(MemorySecrets::default()),
        folder.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        failure.step,
        Step::Source {
            id: "assets".to_owned()
        }
    );
    assert!(
        failure.message.contains("a report's run can't be asked"),
        "{}",
        failure.message
    );
}

/// Counterparties read from MongoDB by the ids a SQLite source found, with nested fields,
/// defaults, `NumberLong`s and dates in declared columns.
#[tokio::test]
async fn takes_declared_columns_from_mongodb_documents() {
    let Some((config, password)) = mongo_test_server("dbrover_report_lookups") else {
        return;
    };
    let mut mongo = dbrover_driver_mongodb::connect(&config, Some(&password), Route::Direct)
        .await
        .unwrap();
    execute(
        &mut mongo,
        &[
            "db.counterparties.drop()",
            "db.counterparties.insertMany([\
             {_id: 'cp-1', name: 'Acme', address: {country: 'FR'}, orders: NumberLong(7), \
              since: ISODate('2026-10-01T12:00:00Z'), credit: NumberDecimal('1000.555')}, \
             {_id: 'cp-2', name: 'Bolt', address: {}, orders: 3, \
              since: ISODate('2026-10-02T06:30:00Z')}, \
             {_id: 'cp-3', name: 'Crate', orders: 1, since: ISODate('2026-10-03T00:00:00Z')}])",
        ],
    )
    .await;

    let text = r#"
[report]
name = "Counterparties"

[[source]]
id = "keys"
data_source = "ledger"
query = "SELECT key FROM keys"

[[source]]
id = "counterparties"
data_source = "crm"
lookup = { from = "keys", column = "key", as = "ids" }
query = "db.counterparties.find({_id: {$in: :ids}})"
columns = [
  { name = "id",      from = "_id" },
  { name = "name" },
  { name = "country", from = "address.country", default = "unknown" },
  { name = "orders",  type = "integer" },
  { name = "since",   type = "timestamp" },
  { name = "credit",  type = "decimal(18,2)" },
]

[combine]
query = "SELECT * FROM counterparties ORDER BY id"

[[output]]
format = "csv"
file = "counterparties.csv"
"#;
    let folder = tempfile::tempdir().unwrap();
    let crm = data_source("crm", ConnectionConfig::Mongo(config));
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set(crm.id, Secret::Database, &password).unwrap();
    let data_sources = [ledger(folder.path(), &["cp-2", "cp-1", "cp-9"]), crm];
    run_report(text, &data_sources, secrets, folder.path())
        .await
        .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(
        std::fs::read_to_string(folder.path().join("counterparties.csv")).unwrap(),
        "id,name,country,orders,since,credit\r\n\
         cp-1,Acme,FR,7,2026-10-01 12:00:00,1000.56\r\n\
         cp-2,Bolt,unknown,3,2026-10-02 06:30:00,\r\n"
    );
}
