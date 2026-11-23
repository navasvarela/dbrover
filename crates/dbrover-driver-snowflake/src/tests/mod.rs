//! The driver against Snowflake's exchanges, replayed by a server on this machine (`replay`),
//! and against Snowflake only when the user asks for it by hand (`live_snowflake`).

mod replay;

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use dbrover_core::{
    CatalogNode, CatalogPath, CatalogSearch, ChildrenQuery, ColumnInfo, Connection, DriverError,
    FolderKind, NodeKind, Password, SnowflakeConfig, SnowflakeSignIn, conformance, describe_error,
};

use self::replay::Replay;
use super::*;

/// The key `dbrover-cloud`'s tests sign with, registered with no Snowflake user.
fn test_key() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dbrover-cloud/testdata/snowflake/key.p8")
}

/// A data source on the replay server, signing in with the test key.
fn config(replay: &Replay) -> SnowflakeConfig {
    SnowflakeConfig {
        account: "myorg-analytics".to_owned(),
        user: "jane".to_owned(),
        sign_in: SnowflakeSignIn::KeyPair {
            private_key_file: test_key(),
        },
        endpoint: replay.endpoint.clone(),
        ..SnowflakeConfig::default()
    }
}

async fn connected(config: &SnowflakeConfig) -> SnowflakeConnection {
    connect(config, None)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

async fn run(connection: &mut SnowflakeConnection, sql: &str) -> (Vec<ColumnInfo>, Vec<Row>) {
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
async fn connects_with_a_jwt_and_its_context_and_pings_without_a_warehouse() {
    let replay = Replay::start(include_str!("exchanges/connect.json")).await;
    let config = SnowflakeConfig {
        warehouse: "ANALYTICS_WH".to_owned(),
        role: "ANALYST".to_owned(),
        database: "SHOP".to_owned(),
        schema: "PUBLIC".to_owned(),
        ..config(&replay)
    };
    let connection = connected(&config).await;
    assert_eq!(connection.server_info().product, "Snowflake");
    assert_eq!(
        connection.server_info().version,
        "account myorg-analytics as jane, warehouse ANALYTICS_WH (X-Small, suspended; stops \
         statements after 1 h)"
    );
    conformance::check_lifecycle(Box::new(connection)).await;

    let authorization = replay.header("authorization");
    let jwt = authorization[0]
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
        .expect("a bearer token");
    let claims: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(jwt.split('.').nth(1).expect("claims"))
            .expect("base64"),
    )
    .expect("JSON");
    assert_eq!(claims["sub"], "MYORG-ANALYTICS.JANE");
    // One JWT for the whole connection: it lasts an hour.
    assert!(authorization.iter().all(|sent| sent == &authorization[0]));
    assert!(replay.header("user-agent").iter().all(|agent| {
        agent
            .as_deref()
            .is_some_and(|agent| agent.starts_with("DbRover/"))
    }));
    assert_eq!(
        replay.request_ids().len(),
        4,
        "a requestId for each statement"
    );
    replay.finish();
}

#[tokio::test]
async fn a_programmatic_access_token_is_sent_with_its_kind() {
    let replay = Replay::start(include_str!("exchanges/token.json")).await;
    let config = SnowflakeConfig {
        sign_in: SnowflakeSignIn::ProgrammaticAccessToken,
        ..config(&replay)
    };
    connect(&config, Some(&Password::new("pat-secret")))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    replay.finish();
}

#[tokio::test]
async fn a_token_sign_in_without_its_token_says_so() {
    let config = SnowflakeConfig {
        account: "acct".to_owned(),
        user: "me".to_owned(),
        sign_in: SnowflakeSignIn::OAuth,
        endpoint: "http://127.0.0.1:9".to_owned(),
        ..SnowflakeConfig::default()
    };
    let Err(error) = connect(&config, None).await else {
        panic!("connected without a token");
    };
    assert!(
        describe_error(&error).contains("Give the data source's OAuth token"),
        "{}",
        describe_error(&error)
    );
}

#[tokio::test]
async fn a_refused_jwt_fails_connecting_with_snowflakes_message() {
    let replay = Replay::start(include_str!("exchanges/connect-failures.json")).await;
    let Err(error) = connect(&config(&replay), None).await else {
        panic!("connected with a refused JWT");
    };
    assert!(matches!(error, DriverError::Connect(_)), "{error:?}");
    assert!(
        describe_error(&error).contains("JWT token is invalid"),
        "{}",
        describe_error(&error)
    );
    replay.finish();
}

#[tokio::test]
async fn a_query_is_polled_and_read_over_gzipped_partitions() {
    let replay = Replay::start(include_str!("exchanges/select.json")).await;
    let config = SnowflakeConfig {
        warehouse: "ANALYTICS_WH".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let (columns, rows) = run(
        &mut connection,
        "SELECT id, name, placed, day, at, when_tz, total, payload, photo, paid FROM orders \
         ORDER BY id",
    )
    .await;
    let named: Vec<(&str, Option<&str>)> = columns
        .iter()
        .map(|column| (column.name.as_str(), column.type_name.as_deref()))
        .collect();
    assert_eq!(
        named,
        [
            ("ID", Some("NUMBER(38,0)")),
            ("NAME", Some("VARCHAR")),
            ("PLACED", Some("TIMESTAMP_NTZ(3)")),
            ("DAY", Some("DATE")),
            ("AT", Some("TIME(0)")),
            ("WHEN_TZ", Some("TIMESTAMP_TZ(3)")),
            ("TOTAL", Some("NUMBER(10,2)")),
            ("PAYLOAD", Some("VARIANT")),
            ("PHOTO", Some("BINARY")),
            ("PAID", Some("BOOLEAN")),
        ]
    );
    assert_eq!(
        rows[0],
        [
            text("99999999999999999999"),
            text("Ada"),
            text("2026-01-02 03:04:05.123"),
            text("2026-01-02"),
            text("13:45:30"),
            text("2021-03-19 18:06:59.000 +01:00"),
            text("1.50"),
            text("{\n  \"tags\": [\n    \"new\"\n  ]\n}"),
            text("DBF00D"),
            text("true"),
        ]
    );
    assert!(rows[1][1..].iter().all(Option::is_none), "{:?}", rows[1]);
    assert_eq!(rows[2][1], text(""), "empty text isn't NULL");
    assert_eq!(rows[2][2], text("1969-12-31 23:59:58.500"));
    let ids: Vec<Option<String>> = rows.iter().map(|row| row[0].clone()).collect();
    assert_eq!(
        ids,
        ["99999999999999999999", "2", "3", "4", "5", "6"].map(text)
    );
    replay.finish();
}

#[tokio::test]
async fn an_update_says_how_many_rows_it_changed() {
    let replay = Replay::start(include_str!("exchanges/update.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let cursor = connection
        .execute("UPDATE orders SET total = 0 WHERE id < 4")
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    assert_eq!(cursor.finish().await.expect("finished"), Some(3));
    replay.finish();
}

#[tokio::test]
async fn a_failed_statement_points_at_snowflakes_line_and_position() {
    let replay = Replay::start(include_str!("exchanges/failure.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let statement = "SELECT id,\n  nmae FROM orders";
    let Err(error) = connection.execute(statement).await else {
        panic!("the statement ran");
    };
    let DriverError::Statement {
        position, source, ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(*position, Some(13), "at nmae");
    assert!(source.to_string().contains("invalid identifier 'NMAE'"));
    replay.finish();
}

#[tokio::test]
async fn cancel_posts_to_the_statements_handle() {
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
        describe_error(&error).contains("SQL execution canceled"),
        "{}",
        describe_error(&error)
    );
    connection.ping().await.expect("still answers");
    replay.finish();
}

#[tokio::test]
async fn a_throttled_submission_is_sent_again_with_the_same_request_id() {
    let replay = Replay::start(include_str!("exchanges/throttled.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let (_, rows) = run(&mut connection, "SELECT 1").await;
    assert_eq!(rows, [vec![text("1")]]);
    assert_eq!(
        replay.request_ids().len(),
        2,
        "the probe's, then one for SELECT 1"
    );
    replay.finish();
}

#[tokio::test]
async fn use_moves_only_this_connections_later_statements() {
    let replay = Replay::start(include_str!("exchanges/use.json")).await;
    let config = SnowflakeConfig {
        warehouse: "ANALYTICS_WH".to_owned(),
        role: "ANALYST".to_owned(),
        database: "SHOP".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let (_, said) = run(&mut connection, "use warehouse big_wh;").await;
    assert_eq!(
        said,
        [vec![text("Later statements here run on warehouse BIG_WH.")]]
    );
    run(&mut connection, "SELECT 1").await;
    replay.finish();

    // Another connection of the same data source still starts where the data source says.
    let other = Replay::start(include_str!("exchanges/connect.json")).await;
    let other_config = SnowflakeConfig {
        endpoint: other.endpoint.clone(),
        schema: "PUBLIC".to_owned(),
        ..config.clone()
    };
    let mut second = connected(&other_config).await;
    second.ping().await.expect("first ping");
    second.ping().await.expect("second ping");
    other.finish();
}

#[tokio::test]
async fn the_explorer_walks_two_databases_with_show_commands_only() {
    let replay = Replay::start(include_str!("exchanges/catalog.json")).await;
    let config = SnowflakeConfig {
        warehouse: "ANALYTICS_WH".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let visited = conformance::walk_catalog(&mut connection, 5).await;
    let shown: Vec<(String, Option<String>)> = visited
        .iter()
        .filter(|node| node.path.segments().len() >= 4)
        .filter(|node| node.path.name_of(NodeKind::Database) == Some("SHOP"))
        .filter(|node| node.kind() != Some(NodeKind::Column))
        .map(|node| (node.path.to_string(), node.detail.clone()))
        .collect();
    let detail = |text: &str| Some(text.to_owned());
    assert_eq!(
        shown
            .iter()
            .map(|(_, detail)| detail.clone())
            .collect::<Vec<_>>(),
        [
            None,
            detail("transient"),
            detail("dynamic"),
            None,
            None,
            detail("internal"),
            detail("s3://acme-lake/orders/"),
        ],
        "{shown:#?}"
    );
    let kinds: Vec<Option<NodeKind>> = visited
        .iter()
        .filter(|node| node.path.segments().len() == 4)
        .map(CatalogNode::kind)
        .collect();
    assert!(kinds.contains(&Some(NodeKind::MaterializedView)));
    assert!(kinds.contains(&Some(NodeKind::Stage)));
    let information_schema = visited
        .iter()
        .find(|node| node.label == "INFORMATION_SCHEMA")
        .expect("listed");
    assert!(information_schema.system, "hidden unless System is ticked");
    let orders = CatalogPath::root()
        .child(NodeKind::Database, "SHOP")
        .child(NodeKind::Schema, "PUBLIC")
        .child(NodeKind::Folder(FolderKind::Tables), "tables")
        .child(NodeKind::Table, "ORDERS");
    let columns: Vec<(String, Option<String>)> = visited
        .iter()
        .filter(|node| node.path.parent().as_ref() == Some(&orders))
        .map(|node| (node.label.clone(), node.detail.clone()))
        .collect();
    assert_eq!(
        columns,
        [
            ("ID".to_owned(), detail("NUMBER(38,0)")),
            ("REGION".to_owned(), detail("VARCHAR(100)")),
            ("PLACED".to_owned(), detail("TIMESTAMP_NTZ(9)")),
            ("TOTAL".to_owned(), detail("NUMBER(10,2)")),
        ]
    );
    replay.finish();
}

#[tokio::test]
async fn show_definition_writes_a_tables_create_table_and_a_views_text() {
    let replay = Replay::start(include_str!("exchanges/definitions.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let public = CatalogPath::root()
        .child(NodeKind::Database, "SHOP")
        .child(NodeKind::Schema, "PUBLIC");
    let table = public
        .child(NodeKind::Folder(FolderKind::Tables), "tables")
        .child(NodeKind::Table, "ORDERS");
    assert_eq!(
        connection.definition(&table).await.expect("a definition"),
        "CREATE TABLE \"SHOP\".\"PUBLIC\".\"ORDERS\" (\n  \"ID\" NUMBER(38,0) NOT NULL,\n  \
         \"REGION\" VARCHAR(100) DEFAULT 'EU',\n  \"PLACED\" TIMESTAMP_NTZ(9),\n  \
         \"TOTAL\" NUMBER(10,2)\n)\nCLUSTER BY (REGION, PLACED);"
    );
    let view = public
        .child(NodeKind::Folder(FolderKind::Views), "views")
        .child(NodeKind::View, "ORDER_TOTALS");
    assert!(
        connection
            .definition(&view)
            .await
            .expect("a definition")
            .starts_with("create view ORDER_TOTALS as select region")
    );
    replay.finish();
}

#[tokio::test]
async fn a_level_of_schemas_pages_with_limit_and_from() {
    let replay = Replay::start(include_str!("exchanges/schemas-paged.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let shop = CatalogPath::root().child(NodeKind::Database, "SHOP");
    let first = ChildrenQuery {
        filter: None,
        after: None,
        limit: Some(2),
    };
    let mut labels = Vec::new();
    let mut page = connection
        .children_page(&shop, &first)
        .await
        .expect("a page");
    loop {
        labels.extend(page.nodes.iter().map(|node| node.label.clone()));
        let Some(next) = page.next else { break };
        page = connection
            .children_page(&shop, &first.after(next))
            .await
            .expect("a page");
    }
    assert_eq!(labels, ["A_EU", "B_US", "C_APAC", "D_LATAM", "E_AFRICA"]);
    replay.finish();
}

#[tokio::test]
async fn found_on_the_server_searches_the_whole_account() {
    let replay = Replay::start(include_str!("exchanges/search.json")).await;
    let mut connection = connected(&config(&replay)).await;
    let found = connection
        .search_catalog(&CatalogSearch::new("order"))
        .await
        .expect("a search")
        .expect("Snowflake searches");
    let places: Vec<(String, Option<NodeKind>, Option<String>)> = found
        .nodes
        .iter()
        .map(|node| (node.label.clone(), node.kind(), node.detail.clone()))
        .collect();
    assert_eq!(
        places,
        [
            (
                "ORDERS".to_owned(),
                Some(NodeKind::Table),
                Some("SHOP.PUBLIC".to_owned())
            ),
            (
                "ORDER_TOTALS".to_owned(),
                Some(NodeKind::View),
                Some("SHOP.PUBLIC".to_owned())
            ),
            (
                "ORDERS_EU".to_owned(),
                Some(NodeKind::Table),
                Some("SALES.EU".to_owned())
            ),
        ]
    );
    assert!(!found.more);
    replay.finish();
}

#[tokio::test]
async fn a_statement_past_the_time_limit_is_stopped_by_snowflake_and_says_so() {
    let replay = Replay::start(include_str!("exchanges/limit.json")).await;
    let config = SnowflakeConfig {
        cost_guard: dbrover_core::CostGuard {
            stop_after: dbrover_core::TimeLimit::from_seconds(600),
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
        said.contains("Snowflake stopped it after 10 min, this data source's limit"),
        "{said}"
    );
    assert!(said.contains("Raise Stop statements after"), "{said}");
    replay.finish();
}

/// The checks above, against Snowflake itself: run by hand with
/// `DBROVER_TEST_SNOWFLAKE_LIVE=account,user,key-file,warehouse cargo test -p
/// dbrover-driver-snowflake live_snowflake -- --ignored`. It reads `SNOWFLAKE_SAMPLE_DATA`,
/// shared with every account, on the warehouse named, for a few credit-seconds.
#[tokio::test]
#[ignore = "needs a Snowflake account: set DBROVER_TEST_SNOWFLAKE_LIVE"]
async fn live_snowflake() {
    let Ok(live) = std::env::var("DBROVER_TEST_SNOWFLAKE_LIVE") else {
        panic!("set DBROVER_TEST_SNOWFLAKE_LIVE=account,user,key-file,warehouse");
    };
    let parts: Vec<&str> = live.split(',').map(str::trim).collect();
    let [account, user, key_file, warehouse] = parts.as_slice() else {
        panic!("DBROVER_TEST_SNOWFLAKE_LIVE is account,user,key-file,warehouse");
    };
    let passphrase = std::env::var("DBROVER_TEST_SNOWFLAKE_PASSPHRASE")
        .ok()
        .map(Password::new);
    let config = SnowflakeConfig {
        account: (*account).to_owned(),
        user: (*user).to_owned(),
        sign_in: SnowflakeSignIn::KeyPair {
            private_key_file: PathBuf::from(key_file),
        },
        warehouse: (*warehouse).to_owned(),
        ..SnowflakeConfig::default()
    };
    let mut connection = connect(&config, passphrase.as_ref())
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let (columns, rows) = run(
        &mut connection,
        "SELECT NULL AS nothing, '' AS empty, 1.50::NUMBER(10,2) AS total, \
         '2026-01-02'::DATE AS day, '2026-01-02 03:04:05.123'::TIMESTAMP_NTZ AS placed, \
         '2026-01-02 03:04:05 +01:00'::TIMESTAMP_TZ AS zoned, PARSE_JSON('{\"a\": [1]}') AS doc",
    )
    .await;
    eprintln!("{columns:?}\n{rows:?}");
    assert_eq!(rows[0][0], None);
    assert_eq!(rows[0][1], text(""));
    let (_, many) = run(
        &mut connection,
        "SELECT C_CUSTKEY FROM SNOWFLAKE_SAMPLE_DATA.TPCH_SF1.CUSTOMER LIMIT 200000",
    )
    .await;
    assert_eq!(many.len(), 200_000, "every partition read");
    let Err(error) = connection.execute("SELECT nmae FROM nowhere_at_all").await else {
        panic!("a missing table ran");
    };
    eprintln!("{}", describe_error(&error));
}
