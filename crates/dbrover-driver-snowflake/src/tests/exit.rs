//! C5's exit (`docs/cloud-data.md`, C5): one key-pair data source taken through every layer of
//! C5 against one replayed file, `exchanges/exit.json`, in CI and with no Snowflake account.

use super::replay::Replay;
use super::*;

#[tokio::test]
async fn c5_exit_a_key_pair_data_source_through_the_whole_of_c5() {
    let replay = Replay::start(include_str!("exchanges/exit.json")).await;
    let config = SnowflakeConfig {
        warehouse: "ANALYTICS_WH".to_owned(),
        role: "ANALYST".to_owned(),
        cost_guard: dbrover_core::CostGuard {
            stop_after: dbrover_core::TimeLimit::from_seconds(600),
            ..dbrover_core::CostGuard::default()
        },
        ..config(&replay)
    };

    // C5c, C5d, C5f: it connects with a JWT and names its warehouse without resuming it.
    let mut console = connected(&config).await;
    assert_eq!(
        console.server_info().version,
        "account myorg-analytics as jane, warehouse ANALYTICS_WH (X-Small, suspended)"
    );

    // C5e: the explorer reaches a column with nothing but SHOW (every statement in the file
    // up to here is one).
    let mut path = CatalogPath::root();
    for (kind, name) in [
        (NodeKind::Database, "SHOP"),
        (NodeKind::Schema, "PUBLIC"),
        (NodeKind::Folder(FolderKind::Tables), "tables"),
        (NodeKind::Table, "ORDERS"),
    ] {
        let children = console.children(&path).await.expect("a level");
        assert!(
            children.iter().any(|node| node.label == name),
            "{name} under {path}"
        );
        path = path.child(kind, name);
    }
    let columns = console.children(&path).await.expect("columns");
    let placed = columns.iter().find(|node| node.label == "PLACED");
    assert_eq!(
        placed.and_then(|node| node.detail.as_deref()),
        Some("TIMESTAMP_NTZ(3)")
    );

    // C5d: a query is polled, then read over three partitions, two of them gzipped, its values
    // as Snowflake's console shows them.
    let (_, rows) = run(&mut console, "SELECT id, placed FROM orders ORDER BY id").await;
    let placed: Vec<Option<String>> = rows.iter().map(|row| row[1].clone()).collect();
    assert_eq!(
        placed,
        [
            text("2026-01-02 03:04:05.123"),
            None,
            text("1970-01-01 00:00:00.000"),
            text("1969-12-31 23:59:58.500"),
            text("2026-01-02 03:04:05.000"),
        ]
    );

    // C5d: USE WAREHOUSE moves this console. C5f: its next statement runs past the data
    // source's limit, and Snowflake stops it.
    run(&mut console, "USE WAREHOUSE big_wh").await;
    let Err(stopped) = console.execute("SELECT COUNT(*) FROM big.events").await else {
        panic!("ran past the limit");
    };
    assert!(
        describe_error(&stopped).contains("Snowflake stopped it after 10 min"),
        "{}",
        describe_error(&stopped)
    );

    // C5d: cancel stops a statement by its handle.
    let cancel = console.cancel_handle();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel.cancel().await
    });
    let Err(cancelled) = console
        .execute("SELECT * FROM big.events ORDER BY ts")
        .await
    else {
        panic!("the statement wasn't cancelled");
    };
    stopper.await.expect("joined").expect("cancelled");
    assert!(describe_error(&cancelled).contains("SQL execution canceled"));

    // Another console of the same data source starts on the data source's warehouse.
    let mut other = connected(&config).await;
    run(&mut other, "SELECT 1").await;
    replay.finish();
}
