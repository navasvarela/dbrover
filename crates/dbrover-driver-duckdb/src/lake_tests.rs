//! A lake read in place by a workspace (RS7, `docs/reports-at-scale.md`), and what a locked
//! workspace may reach ([ADR 0008](../../../docs/adr/0008-a-locked-workspace-reads-a-named-bucket.md)).
//!
//! These run against MinIO from `docker-compose.yml` when `DBROVER_TEST_S3_URL` is set (see
//! `test-servers.env`), over the fixtures in `docker/minio/dbrover`, and skip otherwise. The
//! first of them in a run downloads DuckDB's `httpfs` extension, and the table formats' tests
//! `iceberg`, `avro` and `delta` besides.

use std::time::{Duration, SystemTime};

use dbrover_core::{LakeFormat, LakePath, ObjectStorage, Password, describe_error};

use crate::bucket_tests::test_bucket;
use crate::workspace_tests::{on_file, query, roomy};
use crate::{Allowed, BucketSignIn, LakeView, Workspace};

/// A workspace signed in to MinIO's bucket, before the lock.
struct Lake {
    workspace: Workspace,
    sign_in: BucketSignIn,
    storage: ObjectStorage,
    /// Kept so the workspace's file outlives the test's queries.
    _folder: tempfile::TempDir,
}

async fn signed_in() -> Option<Lake> {
    let test = test_bucket()?;
    let storage = ObjectStorage::S3(test.bucket);
    let folder = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    // The provider's extension holds the secret's type, so it is loaded first.
    workspace
        .load_lake_extensions(&storage, LakeFormat::Parquet)
        .await
        .unwrap();
    let mut sign_in = BucketSignIn::new(storage.clone(), Some(test.secret.clone()));
    workspace.sign_in_to_bucket(&mut sign_in).await.unwrap();
    Some(Lake {
        workspace,
        sign_in,
        storage,
        _folder: folder,
    })
}

fn view(storage: &ObjectStorage, id: &str, path: &str, format: LakeFormat, hive: bool) -> LakeView {
    let lake = LakePath::new(storage, path).unwrap();
    LakeView::new(id, storage, &lake, format, lake.url(), hive)
}

/// What DuckDB says for a path a locked workspace may not read.
const REFUSED: &str = "disabled by configuration";

#[tokio::test]
async fn a_locked_workspace_reads_under_the_lakes_prefix_and_nothing_else() {
    let Some(mut lake) = signed_in().await else {
        return;
    };
    let sales = view(
        &lake.storage,
        "sales",
        "sales/year=*/*.parquet",
        LakeFormat::Parquet,
        true,
    );
    lake.workspace.read_lake(&sales).await.unwrap();
    assert_eq!(sales.prefix(), "s3://dbrover/sales/");

    lake.workspace
        .lock(&Allowed {
            folders: &[],
            lakes: &[sales.prefix()],
        })
        .await
        .unwrap();

    let rows = query(
        &mut lake.workspace,
        "SELECT count(*), sum(amount) FROM sales",
    )
    .await
    .unwrap();
    assert_eq!(
        rows,
        [vec![Some("15".to_owned()), Some("300.00".to_owned())]],
        "the lake's own prefix is read"
    );
    // Everything else the bucket, the machine and the network hold is refused by the same
    // setting, with the path in the message.
    let elsewhere = [
        "SELECT * FROM read_csv('s3://dbrover/customers.csv')",
        "SELECT * FROM read_parquet('s3://dbrover/warehouse/products/*.parquet')",
        // A sibling prefix sharing the allowed one's start: the trailing `/` is what keeps it
        // out, so `salesx/` is not under `sales/`.
        "SELECT * FROM read_parquet('s3://dbrover/salesx/part-0.parquet')",
        "SELECT * FROM read_csv('/etc/hosts')",
        "SELECT * FROM read_csv('https://example.com/x.csv')",
    ];
    for sql in elsewhere {
        let error = query(&mut lake.workspace, sql).await.unwrap_err();
        assert!(describe_error(&error).contains(REFUSED), "{sql}: {error}");
    }
    for loading in ["INSTALL delta", "LOAD delta"] {
        let error = query(&mut lake.workspace, loading).await.unwrap_err();
        let message = describe_error(&error);
        assert!(
            message.contains(REFUSED) || message.contains("disabled through configuration"),
            "{loading}: {error}"
        );
    }
    lake.workspace.close().await;
}

#[tokio::test]
async fn a_lake_is_a_view_the_combine_query_joins_as_a_table() {
    let Some(mut lake) = signed_in().await else {
        return;
    };
    let sales = view(
        &lake.storage,
        "sales",
        "sales/year=*/*.parquet",
        LakeFormat::Parquet,
        true,
    );
    lake.workspace.read_lake(&sales).await.unwrap();
    // A loaded source, as a run's other sources are: two of the fixtures' items, each in a
    // region, so the join keeps part of the lake.
    query(
        &mut lake.workspace,
        "CREATE TABLE main.items AS SELECT * FROM (VALUES ('item 1', 'eu'), ('item 11', 'us')) \
         AS t(item, region)",
    )
    .await
    .unwrap();

    lake.workspace
        .lock(&Allowed {
            folders: &[],
            lakes: &[sales.prefix()],
        })
        .await
        .unwrap();

    let rows = query(
        &mut lake.workspace,
        "SELECT i.region, s.year, count(*) FROM sales s JOIN main.items i USING (item) \
         GROUP BY ALL ORDER BY s.year",
    )
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            vec![
                Some("eu".to_owned()),
                Some("2025".to_owned()),
                Some("1".to_owned())
            ],
            vec![
                Some("us".to_owned()),
                Some("2026".to_owned()),
                Some("1".to_owned())
            ],
        ],
        "the Hive partition's column is one of the view's, and the join is the combine's"
    );
    lake.workspace.close().await;
}

#[tokio::test]
async fn an_iceberg_table_and_a_delta_table_are_read_in_place() {
    let Some(mut lake) = signed_in().await else {
        return;
    };
    let products = view(
        &lake.storage,
        "products",
        "warehouse/products",
        LakeFormat::Delta,
        false,
    );
    for format in [LakeFormat::Delta, LakeFormat::Iceberg] {
        lake.workspace
            .load_lake_extensions(&lake.storage, format)
            .await
            .unwrap();
    }
    lake.workspace.read_lake(&products).await.unwrap();
    // An Iceberg table is read from its newest metadata file, which is under the table's own
    // prefix: C2's lister finds it for a run; here DuckDB's own glob does, before the lock.
    let orders_path = LakePath::new(&lake.storage, "warehouse/shop/orders").unwrap();
    let newest = query(
        &mut lake.workspace,
        &format!(
            "SELECT max(file) FROM glob('{}/metadata/*.metadata.json')",
            orders_path.url()
        ),
    )
    .await
    .unwrap();
    let newest = newest[0][0].clone().expect("a metadata file");
    let orders = LakeView::new(
        "orders",
        &lake.storage,
        &orders_path,
        LakeFormat::Iceberg,
        &newest,
        false,
    );
    lake.workspace.read_lake(&orders).await.unwrap();

    lake.workspace
        .lock(&Allowed {
            folders: &[],
            lakes: &[products.prefix(), orders.prefix()],
        })
        .await
        .unwrap();

    let rows = query(
        &mut lake.workspace,
        "SELECT (SELECT count(*) FROM products), (SELECT count(*) FROM orders)",
    )
    .await
    .unwrap();
    assert_eq!(
        rows,
        [vec![Some("4".to_owned()), Some("4".to_owned())]],
        "both tables' rows, read from the bucket"
    );
    lake.workspace.close().await;
}

/// A combine over a lake can run for longer than a temporary credential lasts, and the lock
/// leaves the secret the one thing DbRover may still change (ADR 0008).
#[tokio::test]
async fn the_buckets_credentials_are_replaced_while_a_locked_combine_runs() {
    let Some(mut lake) = signed_in().await else {
        return;
    };
    let sales = view(
        &lake.storage,
        "sales",
        "sales/year=*/*.parquet",
        LakeFormat::Parquet,
        false,
    );
    lake.workspace.read_lake(&sales).await.unwrap();
    lake.workspace
        .lock(&Allowed {
            folders: &[],
            lakes: &[sales.prefix()],
        })
        .await
        .unwrap();
    lake.sign_in
        .expire_at(SystemTime::now() + Duration::from_secs(1));

    lake.workspace
        .sign_in_to_bucket(&mut lake.sign_in)
        .await
        .unwrap();

    let rows = query(&mut lake.workspace, "SELECT count(*) FROM sales")
        .await
        .unwrap();
    assert_eq!(rows, [vec![Some("15".to_owned())]]);
    lake.workspace.close().await;
}

/// A bucket whose credentials are wrong fails the lake, not the run's start: DuckDB asks the
/// store only when the view is read.
#[tokio::test]
async fn a_lake_read_with_the_wrong_secret_gets_the_stores_refusal() {
    let Some(test) = test_bucket() else { return };
    let storage = ObjectStorage::S3(test.bucket);
    let folder = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&on_file(folder.path()), &roomy(folder.path()))
        .await
        .unwrap();
    workspace
        .load_lake_extensions(&storage, LakeFormat::Parquet)
        .await
        .unwrap();
    let mut sign_in = BucketSignIn::new(storage.clone(), Some(Password::new("not-the-secret")));
    workspace.sign_in_to_bucket(&mut sign_in).await.unwrap();
    let sales = view(
        &storage,
        "sales",
        "sales/*/*.parquet",
        LakeFormat::Parquet,
        false,
    );

    let error = workspace.read_lake(&sales).await.unwrap_err();

    let message = describe_error(&error);
    assert!(
        message.contains("403") || message.to_lowercase().contains("forbidden"),
        "{message}"
    );
    workspace.close().await;
}
