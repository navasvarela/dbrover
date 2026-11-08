//! The driver against the BigQuery emulator, where `DBROVER_TEST_BIGQUERY_URL` names it as
//! `http://host:port/project`: the nightly job's run (`docs/cloud-data.md`, C4, Testing without
//! an account). The emulator answers BigQuery's catalog and paging as Google documents them, so
//! those are tried here, and **Open data**'s `tabledata.list`; what it does differently (dry runs, the byte limit, running jobs,
//! row counts, errors) is the replayed exchanges' to try.

use dbrover_core::{
    BigQueryConfig, Connection, GoogleCloudSignIn, NodeKind, conformance, describe_error,
};

use super::super::{BigQueryConnection, connect};

/// The emulator's data source, or `None` when the variable is unset.
fn emulator() -> Option<BigQueryConfig> {
    let url = std::env::var("DBROVER_TEST_BIGQUERY_URL").ok()?;
    let (endpoint, project) = url.rsplit_once('/')?;
    Some(BigQueryConfig {
        sign_in: GoogleCloudSignIn::Anonymous,
        project: project.to_owned(),
        dataset: "shop".to_owned(),
        endpoint: endpoint.to_owned(),
        ..BigQueryConfig::default()
    })
}

async fn connected(config: &BigQueryConfig) -> BigQueryConnection {
    connect(config)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

#[tokio::test]
async fn the_emulator_connects_walks_and_pages() {
    let Some(config) = emulator() else {
        eprintln!("DBROVER_TEST_BIGQUERY_URL is unset; skipping");
        return;
    };
    conformance::check_lifecycle(Box::new(connected(&config).await)).await;

    let mut connection = connected(&config).await;
    let nodes = conformance::walk_catalog(&mut connection, 4).await;
    let orders: Vec<(&str, Option<&str>)> = nodes
        .iter()
        .filter(|node| {
            node.path.name_of(NodeKind::Table) == Some("orders")
                && node
                    .path
                    .last()
                    .is_some_and(|last| last.kind == NodeKind::Column)
        })
        .map(|node| (node.label.as_str(), node.detail.as_deref()))
        .collect();
    assert_eq!(
        orders,
        [
            ("id", Some("INT64")),
            ("customer", Some("INT64")),
            ("total", Some("NUMERIC")),
            ("placed", Some("TIMESTAMP")),
        ]
    );
    conformance::check_invalid_path(&mut connection).await;

    let open_data = format!(
        "SELECT * FROM `{}.shop.customers` LIMIT 1000",
        config.project
    );
    let mut cursor = connection
        .execute(&open_data)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let customers = cursor.next_page(500).await.unwrap();
    assert_eq!(customers.len(), 2, "{customers:?}");
    cursor.finish().await.unwrap();

    let mut cursor = connection
        .execute("SELECT x FROM UNNEST(GENERATE_ARRAY(1, 1200)) AS x ORDER BY x")
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let mut read = Vec::new();
    while !cursor.is_exhausted() {
        read.extend(cursor.next_page(400).await.unwrap());
    }
    assert_eq!(read.len(), 1200);
    assert_eq!(read.last(), Some(&vec![Some("1200".to_owned())]));
    cursor.finish().await.unwrap();

    let mut cursor = connection
        .execute(
            "SELECT id, total, placed, [1, 2] AS xs, STRUCT('Paris' AS city) AS address \
             FROM orders WHERE id = 1",
        )
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let rows = cursor.next_page(10).await.unwrap();
    let text = |value: &str| Some(value.to_owned());
    assert_eq!(
        rows,
        [vec![
            text("1"),
            text("12.5"),
            text("2026-01-02 03:04:05.000000 UTC"),
            text("[1,2]"),
            text(r#"{"city":"Paris"}"#),
        ]]
    );
    cursor.finish().await.unwrap();
}
