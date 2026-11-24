//! C6's exit: one Databricks data source through every layer of C6, against exchanges replayed
//! on this machine (`docs/cloud-data.md`, C6, the milestone's exit).

use arrow_array::{Array, Decimal128Array, Int64Array, RecordBatch};
use dbrover_core::{
    CatalogPath, Connection, CostGuard, DatabricksConfig, DatabricksSignIn, FolderKind, NodeKind,
    Password, TimeLimit, describe_error,
};

use super::replay::{Replay, Unordered};
use super::{connect, run, text};

/// The one Arrow chunk of the report's source: two orders' ids and totals.
fn orders_chunk() -> Vec<u8> {
    use std::sync::Arc;
    let batch = RecordBatch::try_from_iter([
        (
            "id",
            Arc::new(Int64Array::from(vec![1, 2])) as Arc<dyn Array>,
        ),
        (
            "total",
            Arc::new(
                Decimal128Array::from(vec![150_i128, 2_000])
                    .with_precision_and_scale(18, 2)
                    .expect("a decimal"),
            ),
        ),
    ])
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
async fn c6_exit_a_service_principals_data_source_through_the_whole_of_c6() {
    let replay = Replay::start_with(
        include_str!("exchanges/exit.json"),
        vec![Unordered {
            path: "/links/e5".to_owned(),
            body: orders_chunk(),
            content_type: "application/vnd.apache.arrow.stream",
        }],
    )
    .await;
    let config = DatabricksConfig {
        host: replay.endpoint.clone(),
        sign_in: DatabricksSignIn::ServicePrincipal {
            client_id: "a1b2-client".to_owned(),
        },
        warehouse: "/sql/1.0/warehouses/abc123def456".to_owned(),
        catalog: "main".to_owned(),
        cost_guard: CostGuard {
            stop_after: TimeLimit::from_seconds(1),
            ..CostGuard::default()
        },
        ..DatabricksConfig::default()
    };
    let secret = Password::new("dose-secret");
    let mut console = connect(&config, Some(&secret))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));

    // Test connection names the warehouse, and that it is stopped.
    assert!(
        console
            .server_info()
            .version
            .ends_with("warehouse Shared (Small, serverless, stopped)"),
        "{}",
        console.server_info().version
    );

    // The explorer reaches a column through Unity Catalog's API alone.
    let tables = CatalogPath::root()
        .child(NodeKind::Database, "main")
        .child(NodeKind::Schema, "sales")
        .child(NodeKind::Folder(FolderKind::Tables), "tables");
    for path in [
        CatalogPath::root(),
        CatalogPath::root().child(NodeKind::Database, "main"),
    ] {
        console.children(&path).await.expect("a level");
    }
    let orders = console.children(&tables).await.expect("the tables");
    assert_eq!(orders[0].detail.as_deref(), Some("Delta"));
    let columns = console.children(&orders[0].path).await.expect("columns");
    assert_eq!(columns[1].label, "placed");
    assert_eq!(columns[1].detail.as_deref(), Some("TIMESTAMP"));

    // A query is read over two chunks through their links.
    let (_, rows) = run(
        &mut console,
        "SELECT id, placed FROM main.sales.orders ORDER BY id",
    )
    .await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1][1], None);

    // USE CATALOG moves this console, and not a second one.
    run(&mut console, "USE CATALOG dev").await;
    let (_, here) = run(&mut console, "SELECT current_catalog()").await;
    assert_eq!(here, [vec![text("dev")]]);
    let mut other = connect(&config, Some(&secret))
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let (_, there) = run(&mut other, "SELECT current_catalog()").await;
    assert_eq!(there, [vec![text("main")]]);

    // A statement past the time limit is cancelled, and says so.
    let Err(error) = console.execute("SELECT COUNT(*) FROM big.events").await else {
        panic!("the statement ran past its limit");
    };
    assert!(
        describe_error(&error).contains("after 1 s, this data source's limit"),
        "{}",
        describe_error(&error)
    );

    // A report's source reads Arrow, its decimal kept.
    let mut source = console
        .execute_for_batches("SELECT id, total FROM main.sales.orders", &[])
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let batches = source.batches().expect("Arrow");
    let batch = batches
        .next_batch(100)
        .await
        .expect("a batch")
        .expect("rows");
    let totals = batch
        .column(1)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .expect("a decimal column");
    assert_eq!(totals.value_as_string(0), "1.50");
    replay.finish();
}
