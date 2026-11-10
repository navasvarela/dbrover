//! RS1g's benchmark of today's report paths (`docs/reports-at-scale.md`, "Benchmark"): one
//! source of generated rows per engine, loaded into the combine step's DuckDB and added up,
//! with its columns typed from their values (the text path) and declared in the file (the
//! typed path). Each run's profile says where its time went. These are the numbers RS3c's
//! typed loading and RS6's columnar sources are judged by; RS3f ran it again on RS3's typed
//! loading, and both runs are in the plan's table.
//!
//! Slow, so ignored; run it by hand, with the servers of `docker compose up -d --wait postgres
//! mysql` for those engines:
//!
//! ```text
//! set -a; . ./test-servers.env; set +a
//! DBROVER_BENCH_ROWS=1000000 cargo test --release -p dbrover-engine --test report_benchmark \
//!   -- --ignored --nocapture
//! ```
//!
//! Each engine makes its rows in the source's own query (DuckDB's `range()`, PostgreSQL's
//! `generate_series`, a cross join of digits in SQLite and MySQL), so nothing is loaded
//! first and the run measures the engine answering, the transfer, and the load.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{MemorySecrets, mysql_test_servers, postgres_test_server};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, Password,
    SafetyLevel, SqliteConfig,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{LimitDefaults, Plan, RunEvent, RunFolders, SourcePhases, read_report, run};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

/// Rows a run reads unless `DBROVER_BENCH_ROWS` says otherwise.
const DEFAULT_ROWS: u64 = 1_000_000;

/// The report: one source, its rows added up in DuckDB, one line written.
const REPORT: &str = r##"
[report]
name = "Benchmark"
timeout = "2h"

[[source]]
id = "generated"
data_source = "bench"
query = """
{query}
"""
max_rows = 1000000000
max_bytes = "100 GiB"
timeout = "1h"
{columns}

[combine]
query = "SELECT count(*) AS n, sum(amount) AS total, max(ts) AS last, count(DISTINCT label) AS labels FROM generated"
timeout = "1h"

[[output]]
format = "csv"
file = "total.csv"
"##;

/// The typed path's declaration of the four columns every engine's query makes.
const DECLARED: &str = r#"columns = [
  { name = "id", type = "integer" },
  { name = "amount", type = "decimal(18,2)" },
  { name = "ts", type = "timestamp" },
  { name = "label", type = "text" },
]"#;

/// A CTE `s` of the numbers `i` from 0 to `rows - 1`, from ten digits joined as many times
/// as `rows` has digits, for engines without a series function.
fn numbers(rows: u64) -> String {
    let places = rows.saturating_sub(1).max(1).to_string().len();
    let digits = (0..10)
        .map(|digit| format!("SELECT {digit} AS n"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let sum = (0..places)
        .map(|place| format!("{} * d{place}.n", 10u64.pow(u32::try_from(place).unwrap())))
        .collect::<Vec<_>>()
        .join(" + ");
    let joins = (0..places)
        .map(|place| format!("d AS d{place}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("WITH d AS ({digits}), s AS (SELECT {sum} AS i FROM {joins})")
}

/// An engine, its data source, and the query that makes `rows` rows of the four columns.
struct Engine {
    name: &'static str,
    data_source: DataSource,
    password: Option<Password>,
    query: String,
}

fn data_source(config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "bench".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config,
    }
}

fn engines(rows: u64, folder: &std::path::Path) -> Vec<Engine> {
    let mut engines = vec![
        Engine {
            name: "DuckDB",
            data_source: data_source(ConnectionConfig::DuckDb(DuckDbConfig::new(
                DuckDbDatabase::InMemory,
            ))),
            password: None,
            query: format!(
                "SELECT i AS id, CAST((i % 100000) / 100 AS DECIMAL(18,2)) AS amount, \
                 TIMESTAMP '2026-01-01' + to_seconds(i) AS ts, 'label ' || (i % 1000) AS label \
                 FROM range({rows}) t(i)"
            ),
        },
        Engine {
            name: "SQLite",
            data_source: {
                let path = folder.join("bench.db");
                rusqlite::Connection::open(&path).unwrap();
                data_source(ConnectionConfig::Sqlite(SqliteConfig {
                    path,
                    read_only: true,
                }))
            },
            password: None,
            query: format!(
                "{} SELECT i AS id, printf('%.2f', (i % 100000) / 100.0) AS amount, \
                 datetime('2026-01-01', '+' || i || ' seconds') AS ts, 'label ' || (i % 1000) \
                 AS label FROM s WHERE i < {rows}",
                numbers(rows)
            ),
        },
    ];
    if let Some(server) = postgres_test_server() {
        engines.push(Engine {
            name: "PostgreSQL",
            data_source: data_source(ConnectionConfig::Postgres(server.config)),
            password: Some(server.password),
            query: format!(
                "SELECT i AS id, ((i % 100000) / 100.0)::numeric(18,2) AS amount, \
                 timestamp '2026-01-01' + i * interval '1 second' AS ts, \
                 'label ' || (i % 1000) AS label FROM generate_series(0, {rows} - 1) AS i"
            ),
        });
    }
    if let Some((config, password)) = mysql_test_servers().into_iter().next() {
        engines.push(Engine {
            name: "MySQL",
            data_source: data_source(ConnectionConfig::Mysql(config)),
            password: Some(password),
            query: format!(
                "{} SELECT i AS id, CAST((i % 100000) / 100 AS DECIMAL(18,2)) AS amount, \
                 TIMESTAMP '2026-01-01 00:00:00' + INTERVAL i SECOND AS ts, \
                 CONCAT('label ', i % 1000) AS label FROM s WHERE i < {rows}",
                numbers(rows)
            ),
        });
    }
    engines
}

/// What a run measured.
struct Measured {
    total: Duration,
    source_ms: u64,
    rows: u64,
    phases: SourcePhases,
    combine_ms: u64,
    peak_memory: Option<u64>,
}

async fn run_once(engine: &Engine, declared: bool, folder: &std::path::Path) -> Measured {
    let text = REPORT
        .replace("{query}", &engine.query)
        .replace("{columns}", if declared { DECLARED } else { "" });
    let plan = Plan::new(
        read_report(&text).unwrap(),
        std::slice::from_ref(&engine.data_source),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan.values(&Default::default()).unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    if let Some(password) = &engine.password {
        secrets
            .set(engine.data_source.id, Secret::Database, password)
            .unwrap();
    }
    let tunnels = Tunnels::new(folder.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let out = folder.join("out");
    let staging = out.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder: &out,
        staging: &staging,
        work: &work,
    };
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    run(&plan, &values, &access, folders, &mut |event| {
        events.push(event)
    })
    .await
    .unwrap_or_else(|failure| panic!("{}: {failure}", engine.name));
    let total = started.elapsed();
    let (source_ms, rows, phases) = events
        .iter()
        .find_map(|event| match event {
            RunEvent::SourceDone {
                elapsed_ms,
                rows,
                phases,
                ..
            } => Some((*elapsed_ms, *rows, phases.unwrap_or_default())),
            _ => None,
        })
        .unwrap();
    let (combine_ms, peak_memory) = events
        .iter()
        .find_map(|event| match event {
            RunEvent::CombineDone {
                elapsed_ms,
                peak_memory,
                ..
            } => Some((*elapsed_ms, *peak_memory)),
            _ => None,
        })
        .unwrap();
    Measured {
        total,
        source_ms,
        rows,
        phases,
        combine_ms,
        peak_memory,
    }
}

fn seconds(ms: u64) -> String {
    format!("{:.2}", ms as f64 / 1000.0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "slow: generates millions of rows; run by hand"]
async fn benchmark_todays_report_paths() {
    let rows = std::env::var("DBROVER_BENCH_ROWS")
        .ok()
        .and_then(|rows| rows.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let folder = tempfile::tempdir().unwrap();
    println!(
        "| Engine | Columns | Rows | Run (s) | Source (s) | Until first row | Receiving | \
         Loading | Typing | Combine (s) | Rows a second | DuckDB peak |"
    );
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for engine in engines(rows, folder.path()) {
        for declared in [false, true] {
            let measured = run_once(&engine, declared, folder.path()).await;
            assert_eq!(measured.rows, rows, "{}", engine.name);
            let phases = measured.phases;
            let total_ms = u64::try_from(measured.total.as_millis()).unwrap();
            let per_second = measured.rows * 1000 / measured.source_ms.max(1);
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                engine.name,
                if declared { "declared" } else { "inferred" },
                rows,
                seconds(total_ms),
                seconds(measured.source_ms),
                seconds(phases.connect_ms + phases.server_ms),
                seconds(phases.transfer_ms),
                seconds(phases.load_ms),
                seconds(phases.type_ms),
                seconds(measured.combine_ms),
                per_second,
                measured.peak_memory.map_or_else(
                    || "—".to_owned(),
                    |bytes| format!("{} MB", bytes / 1_000_000)
                ),
            );
        }
    }
}
