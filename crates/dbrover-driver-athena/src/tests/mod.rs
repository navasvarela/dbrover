//! The driver against Athena's exchanges, replayed by a server on this machine (`replay`), and
//! against real Athena only when the user asks for it by hand (`live_athena`).

mod replay;

use dbrover_core::{
    AthenaConfig, AwsSignInKind, ByteCount, CatalogPath, ChildrenQuery, Connection, DriverError,
    JobProgress, NodeKind, PageToken, conformance, describe_error,
};

use self::replay::Replay;
use super::*;

/// A data source on the replay server, signed in with made-up keys, which reads no AWS file of
/// the machine's.
fn config(replay: &Replay) -> AthenaConfig {
    AthenaConfig {
        sign_in: AwsSignInKind::LocalEmulator,
        region: "us-east-1".to_owned(),
        endpoint: replay.endpoint.clone(),
        ..AthenaConfig::default()
    }
}

async fn connected(config: &AthenaConfig) -> AthenaConnection {
    connect(config)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

#[tokio::test]
async fn opens_pings_and_closes_reading_the_workgroup() {
    let replay = Replay::start(include_str!("exchanges/workgroup.json")).await;
    let connection = connected(&config(&replay)).await;
    assert_eq!(connection.server_info().product, "Athena");
    assert_eq!(
        connection.server_info().version,
        "Athena engine version 3, us-east-1"
    );
    conformance::check_lifecycle(Box::new(connection)).await;
    replay.finish();
}

#[tokio::test]
async fn a_select_is_polled_then_read_page_by_page_without_its_header() {
    let replay = Replay::start(include_str!("exchanges/select.json")).await;
    let config = AthenaConfig {
        database: "sales".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let progress = connection
        .progress_handle()
        .expect("Athena reports progress");

    let mut cursor = connection
        .execute("SELECT id, name, total FROM orders")
        .await
        .unwrap();
    let columns: Vec<(&str, Option<&str>)> = cursor
        .columns()
        .iter()
        .map(|column| (column.name.as_str(), column.type_name.as_deref()))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", Some("bigint")),
            ("name", Some("varchar")),
            ("total", Some("decimal(10,2)"))
        ]
    );
    // What the last poll said it had scanned, while the result is read.
    assert_eq!(
        progress.progress().await.unwrap(),
        Some(JobProgress {
            rows_read: None,
            bytes_read: Some(ByteCount(2_500_000)),
        })
    );
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.unwrap());
    }
    let text = |value: &str| Some(value.to_owned());
    assert_eq!(
        rows,
        [
            vec![text("1"), text("Ada"), text("10.50")],
            vec![text("2"), None, text("0.00")],
            // A later page's first row is a row, even one that looks like the names.
            vec![text("3"), text(""), text("7.25")],
            vec![text("4"), text("id"), None],
            vec![text("5"), text("Grace"), text("99.99")],
        ]
    );
    assert_eq!(cursor.finish().await.unwrap(), None);
    replay.finish();
}

#[tokio::test]
async fn an_insert_reports_the_rows_it_wrote_and_sends_the_output_location() {
    let replay = Replay::start(include_str!("exchanges/insert.json")).await;
    let config = AthenaConfig {
        workgroup: "reports".to_owned(),
        output_location: "s3://my-results/dbrover/".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let cursor = connection
        .execute("INSERT INTO archive SELECT * FROM orders WHERE year = 2025")
        .await
        .unwrap();
    assert!(cursor.columns().is_empty());
    assert_eq!(cursor.finish().await.unwrap(), Some(3));
    replay.finish();
}

#[tokio::test]
async fn a_failed_query_points_at_its_mistake() {
    let replay = Replay::start(include_str!("exchanges/failure.json")).await;
    let mut connection = connected(&config(&replay)).await;

    // Read when it ran: a reason with Trino's line and column.
    let sql = "SELECT *\nFORM orders";
    let Err(error) = connection.execute(sql).await else {
        panic!("{sql} ran");
    };
    assert!(
        describe_error(&error).contains("mismatched input 'FORM'"),
        "{error:?}"
    );
    assert!(
        matches!(
            error,
            DriverError::Statement {
                position: Some(9),
                ..
            }
        ),
        "{error:?}"
    );

    // Refused before it ran.
    let sql = "SELECT * FROM t; SELECT 1";
    let Err(error) = connection.execute(sql).await else {
        panic!("{sql} ran");
    };
    assert!(
        describe_error(&error).contains("InvalidRequestException"),
        "{error:?}"
    );
    assert!(
        matches!(
            error,
            DriverError::Statement {
                position: Some(15),
                ..
            }
        ),
        "{error:?}"
    );
    replay.finish();
}

#[tokio::test]
async fn cancel_stops_the_query_by_its_id() {
    let replay = Replay::start(include_str!("exchanges/cancel.json")).await;
    let mut connection = connected(&config(&replay)).await;
    conformance::check_cancel(&mut connection, "SELECT count(*) FROM huge").await;
    replay.finish();
}

#[tokio::test]
async fn connecting_says_what_is_missing() {
    let replay = Replay::start(include_str!("exchanges/connect-failures.json")).await;
    let nowhere = AthenaConfig {
        workgroup: "nowhere".to_owned(),
        ..config(&replay)
    };
    let result = connect(&nowhere).await;
    let message = result
        .as_ref()
        .err()
        .map(|error| describe_error(error))
        .unwrap_or_default();
    assert!(
        message.contains("WorkGroup nowhere is not found"),
        "{message}"
    );
    conformance::check_connect_failure(result);

    let bare = AthenaConfig {
        workgroup: "bare".to_owned(),
        ..config(&replay)
    };
    let Err(error) = connect(&bare).await else {
        panic!("connected without a place for results");
    };
    assert!(
        describe_error(&error).contains("set an output location"),
        "{error:?}"
    );
    replay.finish();
}

#[tokio::test]
async fn the_explorer_walks_the_glue_catalog_from_athenas_catalog_api() {
    let replay = Replay::start(include_str!("exchanges/catalog.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let visited = conformance::walk_catalog(&mut connection, 4).await;
    let shown: Vec<(String, Option<&str>, bool)> = visited
        .iter()
        .map(|node| (node.path.to_string(), node.detail.as_deref(), node.system))
        .collect();
    let lines: Vec<String> = shown
        .iter()
        .map(|(path, detail, system)| format!("{path} {detail:?} {system}"))
        .collect();
    let expected = [
        "/AwsDataCatalog Some(\"Glue\") false",
        "/AwsDataCatalog/information_schema None true",
        "/AwsDataCatalog/sales None false",
        "/AwsDataCatalog/sales/orders Some(\"parquet\") false",
        "/AwsDataCatalog/sales/monthly None false",
        "/AwsDataCatalog/sales/orders/id Some(\"bigint\") false",
        "/AwsDataCatalog/sales/orders/total Some(\"decimal(10,2)\") false",
        "/AwsDataCatalog/sales/orders/dt Some(\"string, partition key\") false",
        "/AwsDataCatalog/sales/monthly/month Some(\"varchar\") false",
        "/AwsDataCatalog/sales/monthly/total Some(\"decimal(38,2)\") false",
    ];
    assert_eq!(lines, expected);
    assert_eq!(visited[4].kind(), Some(NodeKind::View));
    conformance::check_invalid_path(&mut connection).await;
    replay.finish();
}

#[tokio::test]
async fn a_level_of_tables_pages_and_filters_on_athena() {
    let replay = Replay::start(include_str!("exchanges/tables-paged.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let sales = CatalogPath::root()
        .child(NodeKind::Database, "AwsDataCatalog")
        .child(NodeKind::Schema, "sales");
    let labels = |page: &dbrover_core::ChildrenPage| -> Vec<String> {
        page.nodes.iter().map(|node| node.label.clone()).collect()
    };

    // The explorer's first page: every request of fifty until Athena has no more.
    let whole = connection
        .children_page(&sales, &ChildrenQuery::default())
        .await
        .unwrap();
    assert_eq!(
        labels(&whole),
        ["customers", "events", "orders", "order_lines", "returns"]
    );
    assert_eq!(whole.next, None);
    assert_eq!(whole.nodes[1].detail.as_deref(), Some("Iceberg"));

    // Three at a time: the second request asks only for what is still wanted.
    let three = ChildrenQuery {
        limit: Some(3),
        ..ChildrenQuery::default()
    };
    let page = connection.children_page(&sales, &three).await.unwrap();
    assert_eq!(labels(&page), ["customers", "events", "orders"]);
    assert_eq!(page.next, Some(PageToken::new("t2b")));

    // The filter goes to Athena as an expression.
    let filtered = connection
        .children_page(&sales, &ChildrenQuery::first_page(Some("ORDER".to_owned())))
        .await
        .unwrap();
    assert_eq!(labels(&filtered), ["orders", "order_lines"]);
    replay.finish();
}

/// The same requests against real Athena, run by hand only: `DBROVER_TEST_ATHENA_LIVE` names a
/// profile, a region and a workgroup, and optionally an output location, as
/// `profile,region,workgroup[,s3://bucket/prefix/]`. CI never sets it. The queries scan no
/// table, so each is billed Athena's 10 MB minimum.
#[tokio::test]
#[ignore = "needs an AWS account; set DBROVER_TEST_ATHENA_LIVE and run with --ignored"]
async fn live_athena() {
    let Ok(live) = std::env::var("DBROVER_TEST_ATHENA_LIVE") else {
        eprintln!("DBROVER_TEST_ATHENA_LIVE is unset; nothing to try");
        return;
    };
    let parts: Vec<&str> = live.split(',').map(str::trim).collect();
    let [profile, region, workgroup, rest @ ..] = parts.as_slice() else {
        panic!("DBROVER_TEST_ATHENA_LIVE is profile,region,workgroup[,output location]");
    };
    let config = AthenaConfig {
        profile: (*profile).to_owned(),
        region: (*region).to_owned(),
        workgroup: (*workgroup).to_owned(),
        output_location: rest.first().map_or_else(String::new, |at| (*at).to_owned()),
        ..AthenaConfig::default()
    };
    let mut connection = connect(&config)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    eprintln!("connected: {}", connection.server_info());

    let mut cursor = connection
        .execute("SELECT 1 AS n, NULL AS no_value, '' AS empty_text, 'n' AS named_like_a_column")
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let rows = cursor.next_page(500).await.unwrap();
    eprintln!("columns: {:?}\nrows: {rows:?}", cursor.columns());
    assert_eq!(
        rows,
        [vec![
            Some("1".to_owned()),
            None,
            Some(String::new()),
            Some("n".to_owned())
        ]]
    );
    cursor.finish().await.unwrap();

    let sql = "SELECT *\nFORM t";
    let error = match connection.execute(sql).await {
        Ok(_) => panic!("{sql} ran"),
        Err(error) => error,
    };
    eprintln!("failure: {error:?}");
    assert!(matches!(
        error,
        DriverError::Statement {
            position: Some(9),
            ..
        }
    ));

    conformance::check_cancel(
        &mut connection,
        // A trillion rows made from nothing: `sequence` stops at 10,000 entries.
        "SELECT count(*) FROM UNNEST(sequence(1, 10000)) a(x) CROSS JOIN UNNEST(sequence(1, \
         10000)) b(y) CROSS JOIN UNNEST(sequence(1, 10000)) c(z)",
    )
    .await;
}
