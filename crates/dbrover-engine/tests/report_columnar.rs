//! Columnar sources (`docs/reports-at-scale.md`, RS6): a report's source on an engine that
//! answers in Arrow batches, run on the columnar path and on the text path (`columnar =
//! false`), writes the same file. DuckDB always; ClickHouse when the test server is set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::MemorySecrets;
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, Password, SafetyLevel,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{LimitDefaults, Plan, RunEvent, RunFolders, read_report, run};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

/// One source, whose every column the combine query writes: `{query}` makes the rows.
const REPORT: &str = r##"
[report]
name = "Columnar"

[[source]]
id = "s"
data_source = "engine"
query = """
{query}
"""
{more}

[combine]
query = "SELECT * FROM s ORDER BY id"

[[output]]
format = "csv"
file = "s.csv"
"##;

/// What a run wrote and the types its source's table had.
struct Ran {
    csv: String,
    types: Vec<(String, String)>,
}

fn data_source(config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "engine".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config,
    }
}

async fn run_report(
    data_source: &DataSource,
    password: Option<&Password>,
    query: &str,
    more: &str,
    folder: &Path,
) -> Ran {
    let text = REPORT.replace("{query}", query).replace("{more}", more);
    let plan = Plan::new(
        read_report(&text).unwrap(),
        std::slice::from_ref(data_source),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan.values(&Default::default()).unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    if let Some(password) = password {
        secrets
            .set(data_source.id, Secret::Database, password)
            .unwrap();
    }
    let tunnels = Tunnels::new(folder.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = folder.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder,
        staging: &staging,
        work: &work,
    };
    let mut events = Vec::new();
    run(&plan, &values, &access, folders, &mut |event| {
        events.push(event)
    })
    .await
    .unwrap_or_else(|failure| panic!("{more:?}: {failure}"));
    let types = events
        .iter()
        .find_map(|event| match event {
            RunEvent::SourceDone { table, .. } => Some(
                table
                    .iter()
                    .map(|column| (column.name.clone(), column.type_name.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap();
    Ran {
        csv: std::fs::read_to_string(folder.join("s.csv")).unwrap(),
        types,
    }
}

/// Runs `query` on both paths: what each wrote, and the columnar path's types.
async fn both_paths(
    data_source: &DataSource,
    password: Option<&Password>,
    query: &str,
) -> (Ran, Ran) {
    let columnar = tempfile::tempdir().unwrap();
    let text = tempfile::tempdir().unwrap();
    (
        run_report(data_source, password, query, "", columnar.path()).await,
        run_report(
            data_source,
            password,
            query,
            "columnar = false",
            text.path(),
        )
        .await,
    )
}

fn names_and_types(types: &[(&str, &str)]) -> Vec<(String, String)> {
    types
        .iter()
        .map(|(name, type_name)| ((*name).to_owned(), (*type_name).to_owned()))
        .collect()
}

#[tokio::test]
async fn a_duckdb_source_writes_on_the_columnar_path_what_it_writes_as_text() {
    let duckdb = data_source(ConnectionConfig::DuckDb(DuckDbConfig::new(
        DuckDbDatabase::InMemory,
    )));
    let query = "SELECT i AS id, CAST(i * 1.25 AS DECIMAL(18,2)) AS amount, i / 3 AS ratio, \
                 i % 2 = 0 AS even, DATE '2026-01-01' + CAST(i AS INTEGER) AS day, \
                 TIMESTAMP '2026-01-01' + to_seconds(i) AS at, 'label ' || i AS label, \
                 CAST(i AS VARCHAR) AS code, CAST(NULL AS INTEGER) AS nothing, [i, i + 1] AS pair \
                 FROM range(5000) t(i)";
    let (columnar, text) = both_paths(&duckdb, None, query).await;
    assert_eq!(columnar.csv, text.csv);
    assert!(
        columnar
            .csv
            .contains("\r\n1,1.25,0.3333333333333333,false,2026-01-02,2026-01-01 00:00:01,label 1,1,,\"[1, 2]\"\r\n"),
        "{}",
        &columnar.csv[..300]
    );
    // A column of only NULLs keeps its engine's type on the columnar path; as text, nothing
    // types it. Text the engine sends stays text there, unless it is a list it sent as text.
    assert_eq!(
        columnar.types,
        names_and_types(&[
            ("id", "BIGINT"),
            ("amount", "DECIMAL(18,2)"),
            ("ratio", "DOUBLE"),
            ("even", "BOOLEAN"),
            ("day", "DATE"),
            ("at", "TIMESTAMP"),
            ("label", "VARCHAR"),
            ("code", "VARCHAR"),
            ("nothing", "BIGINT"),
            ("pair", "VARCHAR"),
        ])
    );
    assert_eq!(
        text.types.iter().find(|(name, _)| name == "nothing"),
        Some(&("nothing".to_owned(), "VARCHAR".to_owned()))
    );
}

#[tokio::test]
async fn a_clickhouse_source_writes_on_the_columnar_path_what_it_writes_as_text() {
    let Some((config, password)) = common::clickhouse_test_server() else {
        return;
    };
    let clickhouse = data_source(ConnectionConfig::ClickHouse(config));
    let query = "SELECT number AS id, toDecimal64(number * 1.25, 2) AS amount, \
                 number / 3 AS ratio, toBool(number % 2 = 0) AS even, \
                 toDate32('2026-01-01') + number AS day, \
                 toDateTime('2026-01-01 00:00:00', 'UTC') + number AS at, \
                 concat('label ', toString(number)) AS label, toString(number) AS code, \
                 CAST(NULL AS Nullable(Int64)) AS nothing, toFloat32(0.1) AS small, \
                 CAST('b' AS Enum8('a' = 1, 'b' = 2)) AS letter \
                 FROM numbers(5000)";
    let (columnar, text) = both_paths(&clickhouse, Some(&password), query).await;
    assert_eq!(columnar.csv, text.csv);
    assert!(
        columnar.csv.contains(
            "\r\n1,1.25,0.3333333333333333,false,2026-01-02,2026-01-01 00:00:01,label 1,1,,0.1,b\r\n"
        ),
        "{}",
        &columnar.csv[..300]
    );
    assert_eq!(
        columnar.types,
        names_and_types(&[
            ("id", "HUGEINT"),
            ("amount", "DECIMAL(18,2)"),
            ("ratio", "DOUBLE"),
            ("even", "BOOLEAN"),
            ("day", "DATE"),
            ("at", "TIMESTAMP"),
            ("label", "VARCHAR"),
            ("code", "VARCHAR"),
            ("nothing", "BIGINT"),
            ("small", "DOUBLE"),
            ("letter", "VARCHAR"),
        ])
    );
}
