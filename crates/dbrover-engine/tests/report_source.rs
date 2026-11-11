//! A report's source on real data sources (`docs/reporting.md`, RP1d): a trial balance on a
//! SQLite ledger written to CSV and Excel with its parameters bound, the same ledger read a
//! week at a time (RS4b), a statement that writes refused before it is sent, and, on the
//! PostgreSQL test server, a slow source stopped at its timeout.
// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use common::{MemorySecrets, postgres_test_server, sqlite_data_source};
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, SafetyLevel};
use dbrover_engine::{PasswordCommands, ProcessRunner, ReportSource, SourceAccess, Tunnels};
use dbrover_report::{
    CostCheck, LimitDefaults, Logic, Plan, RunEvent, RunFolders, RunValues, SourceError,
    SourceOpener, SourceStatement, read_report, run,
};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

/// A ledger of two accounts, with entries on either side of October 2026.
fn ledger(folder: &Path) -> DataSource {
    let path = folder.join("ledger.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, code TEXT, name TEXT);
             CREATE TABLE entries (account_id INTEGER, booked_on TEXT, debit NUMERIC, credit NUMERIC);
             INSERT INTO accounts VALUES (1, '1000', 'Cash'), (2, '4000', 'Sales');
             INSERT INTO entries VALUES
               (1, '2026-09-30', 999, 0),
               (1, '2026-10-01', 100.25, 0),
               (2, '2026-10-01', 0, 100.25),
               (1, '2026-10-15', 50.125, 0),
               (2, '2026-10-15', 0, 50.125),
               (1, '2026-11-01', 7, 0);",
        )
        .unwrap();
    DataSource {
        name: "ledger".to_owned(),
        ..sqlite_data_source(path)
    }
}

const TRIAL_BALANCE: &str = r##"
[report]
name = "Trial balance"
params = { since = "date", until = "date" }

[[source]]
id = "balances"
data_source = "ledger"
query = """
SELECT a.code AS account, a.name AS account_name,
       SUM(e.debit) AS debit, SUM(e.credit) AS credit,
       SUM(e.debit) - SUM(e.credit) AS balance
FROM entries e JOIN accounts a ON a.id = e.account_id
WHERE e.booked_on >= :since AND e.booked_on < :until
GROUP BY a.code, a.name
ORDER BY a.code
"""

[[output]]
format = "xlsx"
file = "trial-balance-{since:%Y-%m}.xlsx"
columns = [
  { name = "Account", from = "account", type = "text" },
  { name = "Name",    from = "account_name" },
  { name = "Debit",   from = "debit",   type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Credit",  from = "credit",  type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
"##;

fn plan(text: &str, data_source: &DataSource) -> Plan {
    Plan::new(
        read_report(text).unwrap(),
        std::slice::from_ref(data_source),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap()
}

fn october(plan: &Plan) -> RunValues {
    plan.values(
        &[("since", "2026-10-01"), ("until", "2026-11-01")]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
    )
    .unwrap()
}

struct Access {
    secrets: Arc<dyn SecretStore>,
    tunnels: Tunnels,
    commands: PasswordCommands,
}

impl Access {
    fn new(secrets: Arc<dyn SecretStore>, folder: &Path) -> Self {
        Self {
            secrets,
            tunnels: Tunnels::new(folder.join("known_hosts")),
            commands: PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner)),
        }
    }

    fn get(&self) -> SourceAccess<'_> {
        SourceAccess {
            secrets: &self.secrets,
            tunnels: &self.tunnels,
            commands: &self.commands,
        }
    }
}

async fn open(
    plan: &Plan,
    values: &RunValues,
    access: &Access,
) -> Result<ReportSource, SourceError> {
    let Logic::Direct(source) = &plan.logic else {
        panic!("{:?} has a combine step", plan.name);
    };
    let statement = SourceStatement {
        source,
        query: source.query.clone(),
        parameters: values.parameters.clone(),
        deadline: Instant::now() + source.limits.timeout,
        cost: CostCheck::Statement,
    };
    access.get().open(&statement).await
}

#[tokio::test]
async fn writes_a_trial_balance_from_sqlite_with_its_dates_bound() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    let plan = plan(TRIAL_BALANCE, &ledger);
    let values = october(&plan);
    let access = Access::new(Arc::new(MemorySecrets::default()), folder.path());
    let out = folder.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let staging = out.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder: &out,
        staging: &staging,
        work: &work,
    };
    let summary = run(&plan, &values, &access.get(), folders, &mut |_| {})
        .await
        .unwrap();
    assert!(!staging.exists(), "the run removes its staging folder");

    assert_eq!(summary.rows, 2);
    assert_eq!(
        std::fs::read_to_string(out.join("trial-balance-2026-10.csv")).unwrap(),
        "Account,Name,Debit,Credit,Balance\r\n\
         1000,Cash,150.38,0.00,150.38\r\n\
         4000,Sales,0.00,150.38,-150.38\r\n"
    );
    assert!(out.join("trial-balance-2026-10.xlsx").is_file());
}

/// October's entries read a week at a time, each week a statement with its dates bound by the
/// SQLite driver, and added up in the combine step.
const BY_WEEK: &str = r##"
[report]
name = "October by week"
params = { since = "date", until = "date" }

[[source]]
id = "entries"
data_source = "ledger"
query = """
SELECT account_id, booked_on, debit, credit FROM entries
WHERE booked_on >= :part_from AND booked_on < :part_to
"""
partition = { from = ":since", to = ":until", every = "1 week" }

[combine]
query = "SELECT count(*) AS entries, sum(debit) AS debit, sum(credit) AS credit FROM entries"

[[output]]
format = "csv"
file = "october.csv"
"##;

#[tokio::test]
async fn reads_a_sqlite_ledger_a_week_at_a_time_with_each_range_bound() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    let plan = plan(BY_WEEK, &ledger);
    let values = october(&plan);
    let access = Access::new(Arc::new(MemorySecrets::default()), folder.path());
    let out = folder.path().join("out");
    let staging = out.join(".run");
    let work = out.join(".work");
    let folders = RunFolders {
        folder: &out,
        staging: &staging,
        work: &work,
    };
    let mut parts = None;
    run(&plan, &values, &access.get(), folders, &mut |event| {
        if let RunEvent::SourceDone { parts: done, .. } = event {
            parts = done;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("october.csv")).unwrap(),
        "entries,debit,credit\r\n4,150.375,150.375\r\n",
        "October's four entries, none of September's or November's"
    );
    assert_eq!(
        parts,
        Some(5),
        "five weeks from October 1st, the last cut at November 1st"
    );
}

#[tokio::test]
async fn refuses_a_statement_that_writes_before_sending_it() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    let text = TRIAL_BALANCE.replace(
        "query = \"\"\"\nSELECT",
        "query = \"\"\"\nDELETE FROM entries WHERE booked_on >= :since AND booked_on < :until;\nSELECT",
    );
    let only_delete = text.replace(
        "\nSELECT a.code AS account, a.name AS account_name,\n       SUM(e.debit) AS debit, SUM(e.credit) AS credit,\n       SUM(e.debit) - SUM(e.credit) AS balance\nFROM entries e JOIN accounts a ON a.id = e.account_id\nWHERE e.booked_on >= :since AND e.booked_on < :until\nGROUP BY a.code, a.name\nORDER BY a.code\n",
        "\n",
    );
    let plan = plan(&only_delete, &ledger);
    let values = october(&plan);
    let access = Access::new(Arc::new(MemorySecrets::default()), folder.path());
    let Err(refused) = open(&plan, &values, &access).await else {
        panic!("a DELETE opened as a report's source");
    };
    assert!(
        refused
            .message
            .ends_with("a report's source only reads, so it isn't sent"),
        "{refused:?}"
    );
    let ConnectionConfig::Sqlite(config) = &ledger.config else {
        unreachable!("the ledger is a SQLite file");
    };
    let entries: i64 = rusqlite::Connection::open(&config.path)
        .unwrap()
        .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entries, 6);
}

#[tokio::test]
async fn stops_a_slow_postgres_source_at_its_timeout() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "warehouse".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config),
    };
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set(data_source.id, Secret::Database, &server.password)
        .unwrap();
    let text = r#"
        [report]
        name = "Slow"
        params = { pause = "decimal" }
        [[source]]
        id = "slow"
        data_source = "warehouse"
        query = "SELECT pg_sleep(:pause::float8), 1 AS one"
        timeout = "1s"
        [[output]]
        format = "csv"
        file = "slow.csv"
        "#;
    let plan = plan(text, &data_source);
    let values = plan
        .values(
            &[("pause".to_owned(), "30".to_owned())]
                .into_iter()
                .collect(),
        )
        .unwrap();
    let access = Access::new(secrets, folder.path());
    let started = Instant::now();
    let staging = folder.path().join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder: folder.path(),
        staging: &staging,
        work: &work,
    };
    let outcome = run(&plan, &values, &access.get(), folders, &mut |_| {})
        .await
        .map(drop)
        .map_err(|failure| failure.limit.map(|limit| limit.key));
    assert_eq!(outcome, Err(Some("timeout".to_owned())));
    assert!(
        started.elapsed().as_secs() < 15,
        "stopped after {:?}",
        started.elapsed()
    );
    assert!(!folder.path().join("slow.csv").exists());
}
