//! The driver against Databricks's exchanges, replayed by a server on this machine (`replay`),
//! and against Databricks only when the user asks for it by hand (`live_databricks`).

mod exit;
mod replay;

use dbrover_core::{
    CatalogPath, CatalogSearch, ChildrenQuery, ColumnInfo, Connection, DatabricksConfig,
    DatabricksSignIn, DriverError, FolderKind, NodeKind, Password, conformance, describe_error,
};

use self::replay::{Replay, Unordered};
use super::*;

/// The token every replayed exchange expects, made up for the tests.
const TOKEN: &str = "dapi-test-token";

/// A data source on the replay server, signing in with a personal access token.
fn config(replay: &Replay) -> DatabricksConfig {
    DatabricksConfig {
        host: replay.endpoint.clone(),
        sign_in: DatabricksSignIn::PersonalAccessToken,
        warehouse: "abc123def456".to_owned(),
        ..DatabricksConfig::default()
    }
}

async fn connected(config: &DatabricksConfig) -> DatabricksConnection {
    connect(config, Some(&Password::new(TOKEN)))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

async fn run(connection: &mut DatabricksConnection, sql: &str) -> (Vec<ColumnInfo>, Vec<Row>) {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let columns = cursor.columns().to_vec();
    let mut rows = Vec::new();
    loop {
        let page = cursor.next_page(2).await.expect("a page");
        if page.is_empty() {
            break;
        }
        rows.extend(page);
    }
    (columns, rows)
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

#[tokio::test]
async fn connects_by_reading_its_warehouse_and_pings_the_same_way() {
    let replay = Replay::start(include_str!("exchanges/connect.json")).await;
    let connection = connected(&config(&replay)).await;
    assert_eq!(connection.server_info().product, "Databricks");
    assert_eq!(
        connection.server_info().version,
        format!(
            "{}, warehouse Shared (Small, serverless, stopped)",
            replay.endpoint
        )
    );
    conformance::check_lifecycle(Box::new(connection)).await;
    assert!(replay.header("/api/", "user-agent").iter().all(|agent| {
        agent
            .as_deref()
            .is_some_and(|agent| agent.starts_with("DbRover/"))
    }));
    replay.finish();
}

#[tokio::test]
async fn a_refused_token_fails_connecting_with_databricks_words() {
    let replay = Replay::start(include_str!("exchanges/connect-failures.json")).await;
    let Err(error) = connect(&config(&replay), Some(&Password::new(TOKEN))).await else {
        panic!("connected with a refused token");
    };
    assert!(matches!(error, DriverError::Connect(_)), "{error:?}");
    assert!(
        describe_error(&error).contains("Invalid access token."),
        "{}",
        describe_error(&error)
    );
    replay.finish();
}

#[tokio::test]
async fn what_a_data_source_lacks_is_named_before_any_request() {
    let cases = [
        (
            DatabricksConfig {
                warehouse: "w".to_owned(),
                ..DatabricksConfig::default()
            },
            Some(TOKEN),
            "Set the data source's host",
        ),
        (
            DatabricksConfig {
                host: "dbc-1.cloud.databricks.com".to_owned(),
                ..DatabricksConfig::default()
            },
            Some(TOKEN),
            "Set the data source's SQL warehouse",
        ),
        (
            DatabricksConfig {
                host: "dbc-1.cloud.databricks.com".to_owned(),
                warehouse: "w".to_owned(),
                ..DatabricksConfig::default()
            },
            None,
            "Give the data source's personal access token",
        ),
        (
            DatabricksConfig {
                host: "dbc-1.cloud.databricks.com".to_owned(),
                warehouse: "w".to_owned(),
                sign_in: DatabricksSignIn::ServicePrincipal {
                    client_id: " ".to_owned(),
                },
                ..DatabricksConfig::default()
            },
            Some(TOKEN),
            "Set the service principal's client ID",
        ),
    ];
    for (config, secret, expected) in cases {
        let secret = secret.map(Password::new);
        let Err(error) = connect(&config, secret.as_ref()).await else {
            panic!("connected without what {expected} names");
        };
        assert!(
            describe_error(&error).contains(expected),
            "{}",
            describe_error(&error)
        );
    }
}

#[tokio::test]
async fn a_query_is_polled_and_read_over_chunks_through_their_links() {
    let replay = Replay::start(include_str!("exchanges/select.json")).await;
    let config = DatabricksConfig {
        catalog: "main".to_owned(),
        schema: "sales".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let (columns, rows) = run(
        &mut connection,
        "SELECT id, name, placed, total, tags, paid FROM orders ORDER BY id",
    )
    .await;
    let named: Vec<(&str, Option<&str>)> = columns
        .iter()
        .map(|column| (column.name.as_str(), column.type_name.as_deref()))
        .collect();
    assert_eq!(
        named,
        [
            ("id", Some("BIGINT")),
            ("name", Some("STRING")),
            ("placed", Some("TIMESTAMP")),
            ("total", Some("DECIMAL(10,2)")),
            ("tags", Some("ARRAY<STRING>")),
            ("paid", Some("BOOLEAN")),
        ]
    );
    assert_eq!(rows.len(), 5, "every chunk read");
    assert_eq!(
        rows[0],
        [
            text("1"),
            text("Ada"),
            text("2026-01-02T03:04:05.123Z"),
            text("1.50"),
            text("[\"new\",\"vip\"]"),
            text("true"),
        ]
    );
    // `NULL` and empty text stay apart.
    assert_eq!(rows[1][1], text(""));
    assert_eq!(rows[1][2], None);
    assert_eq!(rows[3][0], text("99999999999999999"));
    // A link is the storage's, signed for the statement: the workspace's token never goes
    // with it.
    let links = replay.header("/links/", "authorization");
    assert_eq!(links.len(), 3);
    assert!(links.iter().all(Option::is_none), "{links:?}");
    replay.finish();
}

#[tokio::test]
async fn an_expired_link_is_renewed_by_asking_for_its_chunk_again() {
    let replay = Replay::start(include_str!("exchanges/expired.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let (_, rows) = run(&mut connection, "SELECT 1 AS one").await;
    assert_eq!(rows, [vec![text("1")]]);
    replay.finish();
}

#[tokio::test]
async fn a_failure_points_at_its_line_and_position() {
    let replay = Replay::start(include_str!("exchanges/failure.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let Err(error) = connection.execute("SELECT idd\nFROM orders").await else {
        panic!("an unknown column ran");
    };
    let DriverError::Statement {
        position, source, ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*position, Some(7));
    assert!(source.to_string().contains("UNRESOLVED_COLUMN"), "{source}");
    replay.finish();
}

#[tokio::test]
async fn cancel_posts_to_the_statement() {
    let replay = Replay::start(include_str!("exchanges/cancel.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let cancel = connection.cancel_handle();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel.cancel().await
    });
    let Err(error) = connection.execute("SELECT COUNT(*) FROM big.events").await else {
        panic!("the statement wasn't stopped");
    };
    stopper.await.expect("joined").expect("cancelled");
    assert!(
        describe_error(&error).contains("cancelled"),
        "{}",
        describe_error(&error)
    );
    connection.ping().await.expect("still answers");
    replay.finish();
}

#[tokio::test]
async fn a_throttled_submission_is_sent_again() {
    let replay = Replay::start(include_str!("exchanges/throttled.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let (columns, rows) = run(&mut connection, "CREATE TABLE t (id INT)").await;
    assert!(columns.is_empty() && rows.is_empty());
    replay.finish();
}

#[tokio::test]
async fn use_changes_what_the_next_statement_is_sent_with_and_set_is_refused() {
    let replay = Replay::start(include_str!("exchanges/use.json")).await;
    let config = DatabricksConfig {
        catalog: "main".to_owned(),
        schema: "sales".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let (_, said) = run(&mut connection, "USE CATALOG dev").await;
    assert_eq!(
        said,
        [vec![text("Later statements here run in catalog dev.")]]
    );
    run(&mut connection, "USE SCHEMA eu").await;
    let Err(error) = connection.execute("SET ansi_mode = false").await else {
        panic!("a SET was sent");
    };
    assert!(
        describe_error(&error).contains("keeps no session"),
        "{}",
        describe_error(&error)
    );
    let (_, rows) = run(&mut connection, "SELECT 1").await;
    assert_eq!(rows, [vec![text("1")]]);
    replay.finish();
}

#[tokio::test]
async fn a_statement_past_the_time_limit_is_cancelled_and_says_so() {
    let replay = Replay::start(include_str!("exchanges/limit.json")).await;
    let config = DatabricksConfig {
        warehouse: "/sql/1.0/warehouses/abc123def456".to_owned(),
        cost_guard: dbrover_core::CostGuard {
            stop_after: dbrover_core::TimeLimit::from_seconds(1),
            ..dbrover_core::CostGuard::default()
        },
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let Err(error) = connection.execute("SELECT COUNT(*) FROM big.events").await else {
        panic!("the statement ran past its limit");
    };
    let said = describe_error(&error);
    assert!(
        said.contains("after 1 s, this data source's limit"),
        "{said}"
    );
    assert!(said.contains("warehouse abc123def456"), "{said}");
    replay.finish();
}

/// Chunk `index` of the Arrow test's result: its rows as Databricks's Arrow writes them, with
/// a `TIMESTAMP` in a zone of another name than DbRover's and an `ARRAY` as Arrow's list.
fn arrow_chunk(index: i64, rows: usize) -> Vec<u8> {
    use arrow_array::builder::{ListBuilder, StringBuilder};
    use arrow_array::{
        ArrayRef, Date32Array, Decimal128Array, Int64Array, RecordBatch, TimestampMicrosecondArray,
    };
    use std::sync::Arc;
    let ids: Vec<i64> = (0..rows).map(|row| index * 10 + row as i64).collect();
    let mut tags = ListBuilder::new(StringBuilder::new());
    for _ in 0..rows {
        tags.values().append_value("new");
        tags.values().append_value("vip");
        tags.append(true);
    }
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(ids.clone())),
        Arc::new(
            Decimal128Array::from(
                ids.iter()
                    .map(|id| i128::from(*id) * 100 + 50)
                    .collect::<Vec<_>>(),
            )
            .with_precision_and_scale(38, 2)
            .expect("a decimal"),
        ),
        Arc::new(Date32Array::from(vec![20_455; rows])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![1_767_323_045_000_000; rows])
                .with_timezone("Etc/UTC"),
        ),
        Arc::new(tags.finish()),
    ];
    let batch = RecordBatch::try_from_iter(
        ["id", "total", "day", "placed", "tags"]
            .into_iter()
            .zip(columns),
    )
    .expect("a batch");
    let mut written = Vec::new();
    let mut writer =
        arrow_ipc::writer::StreamWriter::try_new(&mut written, &batch.schema()).expect("a writer");
    writer.write(&batch).expect("written");
    writer.finish().expect("finished");
    drop(writer);
    written
}

#[tokio::test]
async fn a_report_source_reads_arrow_chunks_through_their_links() {
    use arrow_array::Array;
    use arrow_schema::{DataType, TimeUnit};

    let statement = "01f0c3a2-6d2f-1b9e-9a3c-4f5e6a7b8c08";
    let chunk = |index: u64| Unordered {
        path: format!("/api/2.0/sql/statements/{statement}/result/chunks/{index}"),
        body: format!(
            "{{\"chunk_index\": {index}, \"external_links\": [{{\"chunk_index\": {index}, \
             \"external_link\": \"{{endpoint}}/links/arrow-{index}\"}}]}}"
        )
        .into_bytes(),
        content_type: "application/json",
    };
    let link = |index: i64, rows: usize| Unordered {
        path: format!("/links/arrow-{index}"),
        body: arrow_chunk(index, rows),
        content_type: "application/vnd.apache.arrow.stream",
    };
    let replay = Replay::start_with(
        include_str!("exchanges/arrow.json"),
        vec![chunk(1), chunk(2), link(0, 2), link(1, 2), link(2, 1)],
    )
    .await;
    let mut connection = connected(&config(&replay)).await;
    let mut cursor = connection
        .execute_for_batches(
            "SELECT id, total, day, placed, tags FROM main.sales.orders",
            &[],
        )
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let batches = cursor.batches().expect("Databricks answers in Arrow");
    let types: Vec<DataType> = batches
        .schema()
        .fields()
        .iter()
        .map(|field| field.data_type().clone())
        .collect();
    assert_eq!(
        types,
        [
            DataType::Int64,
            DataType::Decimal128(38, 2),
            DataType::Date32,
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            DataType::Utf8,
        ]
    );
    let mut ids = Vec::new();
    let mut tags = Vec::new();
    while let Some(batch) = batches.next_batch(1000).await.expect("a batch") {
        let id = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .expect("ids");
        ids.extend(id.values().iter().copied());
        let tag = batch
            .column(4)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .expect("tags as text");
        tags.extend((0..tag.len()).map(|row| tag.value(row).to_owned()));
    }
    assert_eq!(ids, [0, 1, 10, 11, 20], "every chunk, in order");
    assert_eq!(tags[0], "[new, vip]");
    let links = replay.header("/links/", "authorization");
    assert_eq!(links.len(), 3);
    assert!(links.iter().all(Option::is_none), "{links:?}");
    replay.finish();
}

/// The schema `main.sales`, as the explorer reaches it.
fn sales() -> CatalogPath {
    CatalogPath::root()
        .child(NodeKind::Database, "main")
        .child(NodeKind::Schema, "sales")
}

fn folder(kind: FolderKind) -> CatalogPath {
    sales().child(NodeKind::Folder(kind), kind.label())
}

#[tokio::test]
async fn the_explorer_walks_unity_catalog_without_a_statement() {
    let replay = Replay::start(include_str!("exchanges/catalog.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let visited = conformance::walk_catalog(&mut connection, 5).await;
    let shown: Vec<(String, Option<String>, bool)> = visited
        .iter()
        .map(|node| (node.path.to_string(), node.detail.clone(), node.system))
        .collect();
    let catalogs: Vec<&(String, Option<String>, bool)> = shown[..3].iter().collect();
    assert_eq!(
        catalogs
            .iter()
            .map(|(_, detail, system)| (detail.as_deref(), *system))
            .collect::<Vec<_>>(),
        [
            (None, false),
            (Some("shared"), false),
            (Some("system"), true)
        ]
    );
    let details = |kind: NodeKind| -> Vec<(String, Option<String>)> {
        visited
            .iter()
            .filter(|node| node.path.last().map(|segment| segment.kind) == Some(kind))
            .map(|node| (node.label.clone(), node.detail.clone()))
            .collect()
    };
    assert_eq!(
        details(NodeKind::Table),
        [
            ("orders".to_owned(), Some("Delta".to_owned())),
            ("events".to_owned(), Some("Iceberg, external".to_owned())),
        ]
    );
    assert_eq!(details(NodeKind::View), [("v_orders".to_owned(), None)]);
    assert_eq!(
        details(NodeKind::MaterializedView),
        [("daily".to_owned(), None)]
    );
    assert_eq!(
        details(NodeKind::Volume),
        [
            ("landing".to_owned(), Some("managed".to_owned())),
            ("raw".to_owned(), Some("s3://lake/raw".to_owned())),
        ]
    );
    let order_columns: Vec<(String, Option<String>)> = visited
        .iter()
        .filter(|node| {
            node.path
                .parent()
                .and_then(|parent| parent.last().cloned())
                .is_some_and(|parent| parent.name == "orders")
        })
        .map(|node| (node.label.clone(), node.detail.clone()))
        .collect();
    assert_eq!(
        order_columns,
        [
            ("id".to_owned(), Some("BIGINT".to_owned())),
            ("total".to_owned(), Some("DECIMAL(10,2)".to_owned())),
            ("placed".to_owned(), Some("TIMESTAMP".to_owned())),
            ("day".to_owned(), Some("DATE".to_owned())),
        ]
    );
    let orders = folder(FolderKind::Tables).child(NodeKind::Table, "orders");
    assert_eq!(
        connection.definition(&orders).await.expect("a definition"),
        "CREATE TABLE `main`.`sales`.`orders` (\n  `id` BIGINT NOT NULL,\n  `total` \
         DECIMAL(10,2),\n  `placed` TIMESTAMP,\n  `day` DATE\n)\nUSING DELTA\nPARTITIONED BY \
         (`day`)\nCOMMENT 'one row an order';"
    );
    let view = folder(FolderKind::Views).child(NodeKind::View, "v_orders");
    assert_eq!(
        connection.definition(&view).await.expect("a definition"),
        "CREATE VIEW `main`.`sales`.`v_orders` AS\nSELECT id, total FROM main.sales.orders \
         WHERE total > 0;"
    );
    conformance::check_invalid_path(&mut connection).await;
    // Every request went to a REST API; none was a statement, which would start the warehouse.
    assert!(
        replay
            .header("/api/2.0/sql/statements", "authorization")
            .is_empty()
    );
    replay.finish();
}

#[tokio::test]
async fn the_hive_metastore_shows_empty_where_unity_catalog_wont_list_it() {
    let replay = Replay::start(include_str!("exchanges/hive.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let catalogs = connection
        .children(&CatalogPath::root())
        .await
        .expect("the catalogs");
    assert_eq!(catalogs[0].label, "hive_metastore");
    assert_eq!(catalogs[0].detail.as_deref(), Some("legacy Hive metastore"));
    let schemas = connection
        .children(&catalogs[0].path)
        .await
        .expect("an empty level, not an error");
    assert!(schemas.is_empty());
    let Err(error) = connection.children(&catalogs[1].path).await else {
        panic!("another catalog's refusal was hidden");
    };
    assert!(
        describe_error(&error).contains("USE CATALOG"),
        "{}",
        describe_error(&error)
    );
    replay.finish();
}

#[tokio::test]
async fn a_schemas_tables_page_fifty_at_a_time() {
    let replay = Replay::start(include_str!("exchanges/tables-paged.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let tables = connection
        .children(&folder(FolderKind::Tables))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    assert_eq!(tables.len(), 120);
    assert_eq!(tables[119].label, "t119");
    replay.finish();
}

#[tokio::test]
async fn a_filter_goes_to_table_summaries_and_a_search_catalog_by_catalog() {
    let replay = Replay::start(include_str!("exchanges/search.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let filtered = connection
        .children_page(
            &folder(FolderKind::Tables),
            &ChildrenQuery::first_page(Some("ord".to_owned())),
        )
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let labels: Vec<(&str, Option<&str>)> = filtered
        .nodes
        .iter()
        .map(|node| (node.label.as_str(), node.detail.as_deref()))
        .collect();
    assert_eq!(
        labels,
        [("orders", None), ("Order_Lines", Some("streaming"))]
    );
    let found = connection
        .search_catalog(&CatalogSearch::new("ord"))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
        .expect("Databricks searches");
    let found: Vec<(String, Option<String>)> = found
        .nodes
        .iter()
        .map(|node| (node.path.to_string(), node.detail.clone()))
        .collect();
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[2].1.as_deref(), Some("shared_data.retail"));
    replay.finish();
}

/// The checks above, against Databricks itself: run by hand with
/// `DBROVER_TEST_DATABRICKS_LIVE=host,warehouse-id DBROVER_TEST_DATABRICKS_TOKEN=… cargo test
/// -p dbrover-driver-databricks live_databricks -- --ignored`. It reads the `samples` catalog
/// every workspace has, which starts the warehouse named.
#[tokio::test]
#[ignore = "needs a Databricks workspace: set DBROVER_TEST_DATABRICKS_LIVE"]
async fn live_databricks() {
    let Ok(live) = std::env::var("DBROVER_TEST_DATABRICKS_LIVE") else {
        panic!("set DBROVER_TEST_DATABRICKS_LIVE=host,warehouse-id");
    };
    let Some((host, warehouse)) = live.split_once(',') else {
        panic!("DBROVER_TEST_DATABRICKS_LIVE is host,warehouse-id");
    };
    let Ok(token) = std::env::var("DBROVER_TEST_DATABRICKS_TOKEN") else {
        panic!("set DBROVER_TEST_DATABRICKS_TOKEN to a personal access token");
    };
    let config = DatabricksConfig {
        host: host.trim().to_owned(),
        warehouse: warehouse.trim().to_owned(),
        ..DatabricksConfig::default()
    };
    let mut connection = connect(&config, Some(&Password::new(token)))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    eprintln!("{}", connection.server_info().version);
    let (columns, rows) = run(
        &mut connection,
        "SELECT NULL AS nothing, '' AS empty, CAST(1.50 AS DECIMAL(10,2)) AS total, \
         DATE'2026-01-02' AS day, TIMESTAMP'2026-01-02 03:04:05.123' AS placed, \
         TIMESTAMP_NTZ'2026-01-02 03:04:05' AS plain, X'1F' AS bytes, array(1, 2) AS list, \
         map('a', 1) AS pairs, named_struct('a', 1) AS record, parse_json('{\"a\": [1]}') AS doc",
    )
    .await;
    eprintln!("{columns:?}\n{rows:?}");
    assert_eq!(rows[0][0], None);
    assert_eq!(rows[0][1], text(""));
    let (_, many) = run(
        &mut connection,
        "SELECT * FROM samples.tpch.lineitem LIMIT 200000",
    )
    .await;
    assert_eq!(many.len(), 200_000, "every chunk read");
    let Err(error) = connection
        .execute("SELECT nmae FROM samples.tpch.orders")
        .await
    else {
        panic!("a misspelt column ran");
    };
    eprintln!("{}", describe_error(&error));
}
