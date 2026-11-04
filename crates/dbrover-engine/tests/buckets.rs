//! A DuckDB data source reading a bucket (C2), opened as every front end opens a saved data
//! source: the bucket's access key signs in with the secret kept in the keychain as the data
//! source's password. Runs against MinIO from `docker-compose.yml` when `DBROVER_TEST_S3_URL`
//! is set, and skips otherwise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::MemorySecrets;
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, ObjectStorage,
    Password, S3Bucket, S3SignIn, SafetyLevel, describe_error,
};
use dbrover_engine::{OpenError, PasswordCommands, ProcessRunner, Tunnels, open_data_source};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

const TEST_URL_VAR: &str = "DBROVER_TEST_S3_URL";

/// MinIO's data source from `http://<key>:<secret>@<host>:<port>/<bucket>`, and the key's
/// secret.
fn minio() -> Option<(DataSource, Password)> {
    let Ok(url) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    let (scheme, rest) = url.split_once("://")?;
    let (credentials, rest) = rest.split_once('@')?;
    let (key_id, secret) = credentials.split_once(':')?;
    let (authority, bucket) = rest.split_once('/')?;
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "lake".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::InMemory,
            storage: Some(ObjectStorage::S3(S3Bucket {
                bucket: bucket.to_owned(),
                region: String::new(),
                endpoint: format!("{scheme}://{authority}"),
                sign_in: S3SignIn::AccessKey {
                    key_id: key_id.to_owned(),
                },
            })),
        }),
    };
    Some((data_source, Password::new(secret)))
}

async fn open(
    data_source: &DataSource,
    secrets: &Arc<dyn SecretStore>,
) -> Result<Box<dyn dbrover_core::Connection>, OpenError> {
    let dir = tempfile::tempdir().unwrap();
    open_data_source(
        data_source,
        secrets,
        &Tunnels::new(dir.path().join("known_hosts")),
        &PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner)),
    )
    .await
}

#[tokio::test]
async fn counts_a_buckets_parquet_files_with_the_access_keys_saved_secret() {
    let Some((data_source, secret)) = minio() else {
        return;
    };
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(data_source.id, Secret::Database, &secret)
        .unwrap();
    let mut connection = open(&data_source, &secrets).await.unwrap();

    let mut cursor = connection
        .execute("SELECT count(*) FROM 's3://dbrover/sales/*/*.parquet'")
        .await
        .unwrap();
    let rows = cursor.next_page(1).await.unwrap();
    cursor.finish().await.unwrap();

    assert_eq!(rows, vec![vec![Some("15".to_owned())]]);
}

#[tokio::test]
async fn an_access_key_without_a_saved_secret_says_where_to_set_it() {
    let Some((data_source, _)) = minio() else {
        return;
    };
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());

    let error = open(&data_source, &secrets).await.err().unwrap();

    let message = describe_error(&error);
    assert!(
        message.contains("the data source has no secret for it"),
        "{message}"
    );
}
