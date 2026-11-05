//! The conformance suite and the protocol's session against Trino's container: the `trino`
//! service of `docker-compose.yml`, under its `nightly` profile, with the `memory` and `tpch`
//! catalogs. Skipped unless `DBROVER_TEST_TRINO_URL` is set, as
//! `trino://dbrover@localhost:58080/memory/default`.

use dbrover_core::{
    CatalogPath, Connection, DriverError, NodeKind, Port, RejectionKind, Route, TransactionStatus,
    TrinoConfig, conformance, describe_error,
};

use super::config;
use crate::{TrinoConnection, connect};

const TEST_URL_VAR: &str = "DBROVER_TEST_TRINO_URL";

/// Sums six billion rows that the `tpch` catalog makes up as it reads them: minutes of work,
/// which a cancel cuts short, with nothing stored anywhere.
const LONG_RUNNING: &str = "SELECT sum(quantity) FROM tpch.sf1000.lineitem";

/// Reads `trino://user@host:port/catalog/schema`.
fn test_config() -> Option<TrinoConfig> {
    let Ok(url) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    let rest = url.strip_prefix("trino://").expect("trino:// URL");
    let (user, rest) = rest.split_once('@').expect("user@ in URL");
    let (address, place) = rest.split_once('/').expect("/catalog/schema in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    let (catalog, schema) = place.split_once('/').expect("catalog/schema in URL");
    Some(config(
        host,
        port.parse().expect("a numeric port"),
        user,
        catalog,
        schema,
    ))
}

async fn connected(config: &TrinoConfig) -> TrinoConnection {
    connect(config, None, Route::Direct)
        .await
        .unwrap_or_else(|error| panic!("{TEST_URL_VAR}: {}", describe_error(&error)))
}

/// Runs `sql` to its end and returns its rows.
async fn rows(connection: &mut TrinoConnection, sql: &str) -> Vec<Vec<Option<String>>> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {}", describe_error(&error)));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("rows"));
    }
    cursor.finish().await.expect("finish");
    rows
}

#[tokio::test]
async fn opens_pings_and_closes() {
    let Some(config) = test_config() else { return };
    let connection = connected(&config).await;
    assert_eq!(connection.server_info().product, "Trino");
    conformance::check_lifecycle(Box::new(connection)).await;
    let mut nowhere = config;
    nowhere.server.port = Port::Number(1);
    conformance::check_connect_failure(connect(&nowhere, None, Route::Direct).await);
}

#[tokio::test]
async fn runs_statements_pages_and_points_at_errors() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    conformance::check_execution(&mut connection, conformance::Spelling::TRINO).await;
}

#[tokio::test]
async fn cancels_a_running_query() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    conformance::check_cancel(&mut connection, LONG_RUNNING).await;
}

#[tokio::test]
async fn walks_catalogs_schemas_tables_views_and_columns() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    rows(
        &mut connection,
        "CREATE SCHEMA IF NOT EXISTS memory.dbrover_walk",
    )
    .await;
    rows(
        &mut connection,
        "CREATE OR REPLACE VIEW memory.dbrover_walk.big_nations AS \
         SELECT name FROM tpch.tiny.nation WHERE nationkey > 10",
    )
    .await;
    rows(
        &mut connection,
        "CREATE TABLE IF NOT EXISTS memory.dbrover_walk.orders (id bigint, total decimal(10, 2))",
    )
    .await;

    let top = connection.children(&CatalogPath::root()).await.unwrap();
    let catalogs: Vec<(&str, bool)> = top
        .iter()
        .map(|node| (node.label.as_str(), node.system))
        .collect();
    assert!(catalogs.contains(&("memory", false)), "{catalogs:?}");
    assert!(catalogs.contains(&("tpch", false)), "{catalogs:?}");
    assert!(catalogs.contains(&("system", true)), "{catalogs:?}");

    let walk = CatalogPath::root()
        .child(NodeKind::Database, "memory")
        .child(NodeKind::Schema, "dbrover_walk");
    let visited = conformance::walk_catalog_under(&mut connection, &walk, 2).await;
    let shown: Vec<(String, Option<NodeKind>, Option<&str>)> = visited
        .iter()
        .map(|node| (node.label.clone(), node.kind(), node.detail.as_deref()))
        .collect();
    assert_eq!(
        shown,
        [
            ("big_nations".to_owned(), Some(NodeKind::View), None),
            ("orders".to_owned(), Some(NodeKind::Table), None),
            (
                "name".to_owned(),
                Some(NodeKind::Column),
                Some("varchar(25)")
            ),
            ("id".to_owned(), Some(NodeKind::Column), Some("bigint")),
            (
                "total".to_owned(),
                Some(NodeKind::Column),
                Some("decimal(10,2)")
            ),
        ]
    );
    let view = connection.definition(&visited[0].path).await.unwrap();
    assert!(view.contains("CREATE VIEW"), "{view}");

    let tiny = CatalogPath::root()
        .child(NodeKind::Database, "tpch")
        .child(NodeKind::Schema, "tiny");
    let tables: Vec<String> = connection
        .children(&tiny)
        .await
        .unwrap()
        .into_iter()
        .map(|node| node.label)
        .collect();
    assert!(tables.contains(&"lineitem".to_owned()), "{tables:?}");
    conformance::check_invalid_path(&mut connection).await;
    rows(&mut connection, "DROP VIEW memory.dbrover_walk.big_nations").await;
    rows(&mut connection, "DROP TABLE memory.dbrover_walk.orders").await;
}

/// `USE`, `SET SESSION`, `PREPARE` and `START TRANSACTION` reach the next statement through
/// the headers the coordinator hands back.
#[tokio::test]
async fn keeps_the_session_between_statements() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    rows(&mut connection, "USE tpch.tiny").await;
    assert_eq!(
        rows(&mut connection, "SELECT count(*) FROM nation").await,
        [vec![Some("25".to_owned())]]
    );
    rows(&mut connection, "SET SESSION query_max_run_time = '1h'").await;
    let shown = rows(&mut connection, "SHOW SESSION LIKE 'query_max_run_time'").await;
    assert_eq!(shown[0][1].as_deref(), Some("1h"), "{shown:?}");
    rows(&mut connection, "PREPARE plus_one FROM SELECT ? + 1").await;
    assert_eq!(
        rows(&mut connection, "EXECUTE plus_one USING 41").await,
        [vec![Some("42".to_owned())]]
    );
    rows(&mut connection, "START TRANSACTION").await;
    assert_eq!(
        connection.transaction_status().await.unwrap(),
        TransactionStatus::InTransaction
    );
    rows(&mut connection, "COMMIT").await;
    assert_eq!(
        connection.transaction_status().await.unwrap(),
        TransactionStatus::Idle
    );
}

#[tokio::test]
async fn values_keep_their_text_and_progress_is_reported() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let values = rows(
        &mut connection,
        "SELECT 9223372036854775807, DECIMAL '1.50', NULL, '', ARRAY[1, 2], DOUBLE 'NaN', \
         TIMESTAMP '2026-11-05 10:00:00.123', true",
    )
    .await;
    let text = |value: &str| Some(value.to_owned());
    assert_eq!(
        values,
        [vec![
            text("9223372036854775807"),
            text("1.50"),
            None,
            text(""),
            text("[1,2]"),
            text("NaN"),
            text("2026-11-05 10:00:00.123"),
            text("true"),
        ]]
    );
    let mut cursor = connection
        .execute("SELECT count(*) FROM tpch.tiny.lineitem")
        .await
        .unwrap();
    while !cursor.is_exhausted() {
        cursor.next_page(500).await.unwrap();
    }
    let progress = cursor.progress().expect("Trino reports what it read");
    assert!(
        progress.rows_read.is_some_and(|rows| rows > 0),
        "{progress:?}"
    );
    cursor.finish().await.unwrap();
}

#[tokio::test]
async fn checks_statements_without_running_them() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    rows(
        &mut connection,
        "CREATE TABLE IF NOT EXISTS dbrover_validate (n integer)",
    )
    .await;
    for accepted in [
        "SELECT n FROM dbrover_validate WHERE n > 1",
        "INSERT INTO dbrover_validate VALUES (1)",
    ] {
        assert_eq!(
            connection.validate(accepted).await.unwrap(),
            None,
            "{accepted}"
        );
    }
    assert_eq!(
        rows(&mut connection, "SELECT count(*) FROM dbrover_validate").await,
        [vec![Some("0".to_owned())]],
        "nothing was inserted"
    );
    for unknown in [
        "SELECT * FROM dbrover_no_such_table",
        "SELECT missing FROM dbrover_validate",
    ] {
        let rejection = connection.validate(unknown).await.unwrap().expect(unknown);
        assert_eq!(rejection.kind, RejectionKind::UnknownName, "{rejection:?}");
    }
    let sql = "SELECT n n n FROM dbrover_validate";
    let rejection = connection.validate(sql).await.unwrap().expect(sql);
    assert_eq!(rejection.kind, RejectionKind::Syntax, "{rejection:?}");
    assert_eq!(
        rejection.position,
        Some("SELECT n n ".len()),
        "{rejection:?}"
    );
    rows(&mut connection, "DROP TABLE dbrover_validate").await;
    let error = connection.execute("SELECT * FROM dbrover_validate").await;
    assert!(matches!(error, Err(DriverError::Statement { .. })));
}
