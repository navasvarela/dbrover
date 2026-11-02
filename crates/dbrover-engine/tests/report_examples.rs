//! The example reports in `examples/reports/`, run on the data their scripts make: the
//! SQLite ledger of `ledger.sql`, and for the valuation, the PostgreSQL test server's
//! transactions of `valuation-postgres.sql` and the DuckDB prices of `valuation-prices.sql`.
//! Their CSV files are compared with the ones checked in beside them, so the manual's examples
//! keep working. Set `DBROVER_UPDATE_EXPECTED=1` to write the checked-in files from this run
//! instead, after a change that is meant to change them.

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
use dbrover_report::{Plan, RunFolders, read_report, run};
use dbrover_sql::Dialect;
use dbrover_store::{Secret, SecretStore, TrustedCommands};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
}

/// The data source `ledger`, a SQLite file in `folder` made by `ledger.sql`.
fn ledger(folder: &Path) -> DataSource {
    let path = folder.join("ledger.db");
    let script = std::fs::read_to_string(examples().join("ledger.sql")).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    DataSource {
        name: "ledger".to_owned(),
        ..sqlite_data_source(path)
    }
}

/// Runs every statement of the example script `name` on `connection`.
async fn run_script(connection: &mut dyn Connection, name: &str, dialect: Dialect) {
    let script = std::fs::read_to_string(examples().join(name)).unwrap();
    for range in dbrover_sql::split_statements(&script, dialect) {
        let statement = &script[range];
        connection
            .execute(statement)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"))
            .finish()
            .await
            .unwrap();
    }
}

/// Runs the example `name` for October 2026 on `data_sources` into `out`.
async fn run_example(name: &str, data_sources: &[DataSource], out: &Path) {
    run_example_with(name, data_sources, Arc::new(MemorySecrets::default()), out).await;
}

async fn run_example_with(
    name: &str,
    data_sources: &[DataSource],
    secrets: Arc<dyn SecretStore>,
    out: &Path,
) {
    let text = std::fs::read_to_string(examples().join(format!("{name}.toml"))).unwrap();
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
    run(&plan, &values, &access, folders, &mut |_| {})
        .await
        .unwrap_or_else(|failure| panic!("{failure}"));
}

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

#[tokio::test]
async fn the_trial_balance_example_matches_its_checked_in_file() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    run_example("trial-balance", &[ledger], folder.path()).await;
    compare(folder.path(), "trial-balance-2026-10.csv");
    assert!(folder.path().join("trial-balance-2026-10.xlsx").is_file());
}

#[tokio::test]
async fn the_general_ledger_example_matches_its_checked_in_file() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    run_example("general-ledger", &[ledger], folder.path()).await;
    compare(folder.path(), "general-ledger-2026-10.csv");
    assert!(folder.path().join("general-ledger-2026-10.xlsx").is_file());
}

/// The data source `prices-duckdb`: a DuckDB file in `folder` made by `valuation-prices.sql`,
/// read-only, as the example says to set it.
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
    run_script(&mut file, "valuation-prices.sql", Dialect::DuckDb).await;
    Box::new(file).close().await.unwrap();
    DataSource {
        id: DataSourceId::generate(),
        name: "prices-duckdb".to_owned(),
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

/// Needs the PostgreSQL test server, where it replaces the schema `valuation`, as the
/// example's script does.
#[tokio::test]
async fn the_valuation_example_matches_its_checked_in_file() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let mut admin =
        dbrover_driver_postgres::connect(&server.config, Some(&server.password), Route::Direct)
            .await
            .unwrap();
    run_script(&mut admin, "valuation-postgres.sql", Dialect::Postgres).await;
    let ledger = DataSource {
        id: DataSourceId::generate(),
        name: "ledger-postgres".to_owned(),
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
    run_example_with("valuation", &[ledger, prices], secrets, folder.path()).await;
    compare(folder.path(), "valuation-2026-10.csv");
    assert!(folder.path().join("valuation-2026-10.xlsx").is_file());
}
