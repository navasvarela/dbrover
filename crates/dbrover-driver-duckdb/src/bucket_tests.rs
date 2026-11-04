//! A DuckDB data source's bucket (C2): its secret, and reading it.
//!
//! The tests that read a bucket run against MinIO from `docker-compose.yml` when
//! `DBROVER_TEST_S3_URL` is set (see `test-servers.env`), signing in with MinIO's own access
//! key, and skip otherwise. The first of them in a run downloads DuckDB's `httpfs` extension.

use dbrover_core::conformance;
use dbrover_core::{NodeKind, ObjectKind, ObjectStorage, S3Bucket, S3SignIn};

use super::*;
use crate::tests::{failure, in_memory, rows, text};

const TEST_URL_VAR: &str = "DBROVER_TEST_S3_URL";

/// MinIO's bucket from `http://<key>:<secret>@<host>:<port>/<bucket>`, with its access key,
/// and the key's secret.
struct TestBucket {
    bucket: S3Bucket,
    secret: Password,
}

fn test_bucket() -> Option<TestBucket> {
    let Ok(url) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    let (scheme, rest) = url.split_once("://")?;
    let (credentials, rest) = rest.split_once('@')?;
    let (key_id, secret) = credentials.split_once(':')?;
    let (authority, bucket) = rest.split_once('/')?;
    Some(TestBucket {
        bucket: S3Bucket {
            bucket: bucket.to_owned(),
            region: String::new(),
            endpoint: format!("{scheme}://{authority}"),
            sign_in: S3SignIn::AccessKey {
                key_id: key_id.to_owned(),
            },
        },
        secret: Password::new(secret),
    })
}

fn with_bucket(bucket: S3Bucket) -> DuckDbConfig {
    DuckDbConfig {
        storage: Some(ObjectStorage::S3(bucket)),
        ..in_memory()
    }
}

#[tokio::test]
async fn reads_parquet_files_from_a_bucket_through_a_glob() {
    let Some(test) = test_bucket() else { return };
    let mut connection = connect_with_secret(&with_bucket(test.bucket), Some(&test.secret))
        .await
        .unwrap();

    let counted = rows(
        &mut connection,
        "SELECT count(*), sum(amount) FROM 's3://dbrover/sales/*/*.parquet'",
    )
    .await;

    assert_eq!(counted, vec![text(&[Some("15"), Some("300.00")])]);
}

#[tokio::test]
async fn a_bucket_read_without_credentials_gets_the_stores_refusal() {
    let Some(test) = test_bucket() else { return };
    let anonymous = S3Bucket {
        sign_in: S3SignIn::Anonymous,
        ..test.bucket
    };
    let mut connection = connect_with_secret(&with_bucket(anonymous), None)
        .await
        .unwrap();

    let refusal = failure(
        &mut connection,
        "SELECT count(*) FROM 's3://dbrover/sales/*/*.parquet'",
    )
    .await;

    assert!(refusal.contains("403"), "{refusal}");
}

#[tokio::test]
async fn an_expiring_secret_is_made_again_before_the_next_statement() {
    let Some(test) = test_bucket() else { return };
    let mut connection = connect_with_secret(&with_bucket(test.bucket), Some(&test.secret))
        .await
        .unwrap();
    rows(&mut connection, "DROP SECRET dbrover_bucket").await;
    let Some(bucket) = &mut connection.bucket else {
        panic!("the connection has no bucket");
    };
    bucket.expire_at(SystemTime::now());

    let counted = rows(
        &mut connection,
        "SELECT count(*) FROM 's3://dbrover/sales/year=2026/*.parquet'",
    )
    .await;

    assert_eq!(counted, vec![text(&[Some("5")])]);
}

/// Needs no bucket: an `http` secret is DuckDB's own, with no extension to download.
#[tokio::test]
async fn secrets_live_in_memory_and_none_can_be_made_persistent() {
    let mut connection = connect(&in_memory()).await.unwrap();
    rows(
        &mut connection,
        "CREATE TEMPORARY SECRET probe (TYPE http, BEARER_TOKEN 'hunter2')",
    )
    .await;

    let listed = rows(
        &mut connection,
        "SELECT persistent, storage, secret_string LIKE '%hunter2%' FROM duckdb_secrets()",
    )
    .await;
    let refusal = failure(
        &mut connection,
        "CREATE PERSISTENT SECRET kept (TYPE http, BEARER_TOKEN 'hunter2')",
    )
    .await;

    assert_eq!(
        listed,
        vec![text(&[Some("false"), Some("memory"), Some("false")])]
    );
    assert!(
        refusal.contains("Persistent secrets are disabled"),
        "{refusal}"
    );
}

/// Needs no network: the extension repository is an address nothing listens on, and the
/// extension folder is empty, so DuckDB fails to download `httpfs` the way it does offline.
#[tokio::test]
async fn an_extension_duckdb_cant_download_is_explained_and_doesnt_stop_the_connection() {
    let folder = tempfile::tempdir().unwrap();
    let mut connection = connect(&in_memory()).await.unwrap();
    rows(
        &mut connection,
        &format!(
            "SET extension_directory = '{}'",
            folder.path().display().to_string().replace('\'', "''")
        ),
    )
    .await;
    rows(
        &mut connection,
        "SET autoinstall_extension_repository = 'http://127.0.0.1:9'",
    )
    .await;
    connection.bucket = Some(bucket::Bucket::new(
        ObjectStorage::S3(S3Bucket {
            bucket: "dbrover".to_owned(),
            sign_in: S3SignIn::Anonymous,
            ..S3Bucket::default()
        }),
        None,
    ));

    let local = rows(&mut connection, "SELECT 42").await;
    let refusal = failure(&mut connection, "SELECT * FROM 's3://dbrover/a.parquet'").await;

    assert_eq!(local, vec![text(&[Some("42")])]);
    assert!(
        refusal.contains("DuckDB couldn't download its httpfs extension"),
        "{refusal}"
    );
}

#[tokio::test]
async fn a_statement_naming_a_remote_file_isnt_checked_as_it_is_typed() {
    let mut connection = connect(&in_memory()).await.unwrap();

    let remote = connection
        .validate("SELECT * FROM 's3://nowhere/missing.parquet'")
        .await
        .unwrap();
    let local = connection
        .validate("SELECT * FROM missing_table")
        .await
        .unwrap();

    assert_eq!(remote, None);
    assert!(local.is_some());
}

fn bucket_path() -> CatalogPath {
    CatalogPath::root().child(NodeKind::Object(ObjectKind::Bucket), "s3://dbrover")
}

#[tokio::test]
async fn walks_the_bucket_beside_the_database_down_to_files_columns() {
    let Some(test) = test_bucket() else { return };
    let mut connection = connect_with_secret(&with_bucket(test.bucket), Some(&test.secret))
        .await
        .unwrap();

    let nodes = conformance::walk_catalog(&mut connection, 5).await;

    let sales = bucket_path().child(NodeKind::Object(ObjectKind::Folder), "sales");
    let parquet = sales
        .child(NodeKind::Object(ObjectKind::Folder), "year=2025")
        .child(NodeKind::Object(ObjectKind::File), "part-0.parquet");
    let csv = bucket_path().child(NodeKind::Object(ObjectKind::File), "customers.csv");
    let columns = |file: &CatalogPath| -> Vec<(String, Option<String>)> {
        nodes
            .iter()
            .filter(|node| node.path.parent().as_ref() == Some(file))
            .map(|node| (node.label.clone(), node.detail.clone()))
            .collect()
    };
    let bucket = nodes
        .iter()
        .find(|node| node.path == bucket_path())
        .unwrap();
    assert_eq!(bucket.label, "dbrover");
    assert!(nodes.iter().any(|node| node.path == sales));
    // DuckDB reads `year` from the Hive-style folder, as `SELECT *` on the file does.
    assert_eq!(
        columns(&parquet),
        [
            ("id".to_owned(), Some("INTEGER".to_owned())),
            ("item".to_owned(), Some("VARCHAR".to_owned())),
            ("amount".to_owned(), Some("DECIMAL(10,2)".to_owned())),
            ("sold_on".to_owned(), Some("DATE".to_owned())),
            ("year".to_owned(), Some("BIGINT".to_owned())),
        ]
    );
    let csv_columns: Vec<_> = columns(&csv).into_iter().map(|(name, _)| name).collect();
    assert_eq!(csv_columns, ["id", "name", "city"]);
}

#[tokio::test]
async fn pages_a_level_with_the_stores_continuation_and_filters_it() {
    let Some(test) = test_bucket() else { return };
    let mut connection = connect_with_secret(&with_bucket(test.bucket), Some(&test.secret))
        .await
        .unwrap();
    let sales = bucket_path().child(NodeKind::Object(ObjectKind::Folder), "sales");

    let labels = conformance::check_children_pages(&mut connection, &sales, 1, 2, "2026").await;

    assert_eq!(labels, ["year=2025", "year=2026"]);
}

#[tokio::test]
async fn a_listing_the_store_refuses_says_why() {
    let Some(test) = test_bucket() else { return };
    let anonymous = S3Bucket {
        sign_in: S3SignIn::Anonymous,
        ..test.bucket
    };
    let mut connection = connect_with_secret(&with_bucket(anonymous), None)
        .await
        .unwrap();

    let error = connection.children(&bucket_path()).await.unwrap_err();

    let message = dbrover_core::describe_error(&error);
    assert!(message.contains("AccessDenied"), "{message}");
}
