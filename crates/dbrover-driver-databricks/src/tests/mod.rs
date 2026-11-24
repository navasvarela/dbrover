//! The driver against Databricks's exchanges, replayed by a server on this machine (`replay`),
//! and against Databricks only when the user asks for it by hand (`live_databricks`).

mod replay;

use dbrover_core::{
    CatalogPath, CatalogSearch, ChildrenQuery, ColumnInfo, Connection, DatabricksConfig,
    DatabricksSignIn, DriverError, FolderKind, NodeKind, Password, conformance, describe_error,
};

use self::replay::Replay;
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
        format!("{}, warehouse Shared", replay.endpoint)
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
