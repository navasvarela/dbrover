//! A statement whose result an engine writes as files in a bucket, for a report to read
//! instead of paging its rows (`docs/reports-at-scale.md`, RS7f).
//!
//! Paging a warehouse's API row by row is the slow way to move a result of many gigabytes:
//! Athena writes every result to S3 already, and BigQuery's `EXPORT DATA` writes Parquet to
//! Cloud Storage, which DuckDB reads in parallel. Writing to a bucket is a side effect a
//! report's read-only promise doesn't cover, so a `[[source]]` opts in with `handoff`, naming
//! a bucket data source and a prefix under it.
//!
//! Two sign-ins meet here, each staying where C0 and C2 put it: the engine writes the files
//! with its own data source's credentials, and the bucket is read and cleaned with the bucket
//! data source's.

use crate::{DriverKind, LakeFormat, ObjectStorage, StorageProvider};

/// What an engine is asked to do with a statement's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Write the files into the folder the run owns, which is empty: `UNLOAD` for Athena,
    /// `EXPORT DATA` for BigQuery.
    IntoFolder(HandoffFolder),
    /// Hand over the result file the engine writes anyway, wherever it writes it, which only
    /// Athena has. The folder the run owns is not used, and the engine's own location must be
    /// under the bucket the report named, or the run can neither read nor delete it.
    OwnResult,
}

/// The folder in a bucket a run owns for one statement's files: the bucket as the report's
/// bucket data source holds it, and the key under it, which no other run writes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffFolder {
    provider: StorageProvider,
    bucket: String,
    key: String,
    url: String,
}

impl HandoffFolder {
    /// The folder at `key` (ending in `/`) in `storage`.
    pub fn new(storage: &ObjectStorage, key: String) -> Self {
        Self {
            provider: storage.provider(),
            bucket: storage.bucket().to_owned(),
            url: format!("{}/{key}", storage.url()),
            key,
        }
    }

    pub fn provider(&self) -> StorageProvider {
        self.provider
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The key the provider's own listing takes, ending in `/`.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The address an engine writes to, ending in `/`: `s3://sales/dbrover-tmp/…/`.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// What an engine wrote for a [`Handoff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandedOver {
    pub files: HandoffFiles,
    /// How the files are read, which is the format the engine wrote: Parquet for an `UNLOAD`
    /// or an `EXPORT DATA`, CSV for Athena's own result file.
    pub format: LakeFormat,
    /// The bytes the statement scanned, as the engine reports them: what it bills.
    pub scanned: Option<u64>,
}

/// Where the files an engine wrote are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffFiles {
    /// The addresses the engine named, each a whole address (`s3://sales/…/result.csv`).
    Named {
        /// What the run reads into the source's table.
        read: Vec<String>,
        /// What the run deletes once it has read them, which can be more than it read: Athena
        /// writes a metadata file beside its result file.
        remove: Vec<String>,
    },
    /// The engine wrote into the folder it was given without naming what it wrote, as an
    /// `EXPORT DATA` whose `uri` holds a wildcard does: the run lists the folder, reads what
    /// it finds, and removes the folder when it ends.
    InFolder,
}

/// What an engine's handoff takes, where it has one.
///
/// Planning reads it, so a report that asks an engine for a handoff it hasn't got, or names a
/// bucket its export can't write to, is refused before the run starts rather than failing in
/// the engine's own words half a statement later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffSupport {
    /// The one provider the engine writes to.
    pub provider: StorageProvider,
    /// Whether the report must name a `prefix`: an engine with no result file of its own has
    /// nowhere else to write.
    pub needs_prefix: bool,
}

impl HandoffSupport {
    /// What `driver`'s handoff takes, or `None` for an engine that has none.
    ///
    /// Databricks's external links land with C6. Trino has none to give: it has no `UNLOAD`, and its spooled protocol answers with
    /// segments of the same rows in the server's own location, not files in a prefix a report
    /// names (`docs/reports-at-scale.md`, "The handoff against today's code").
    pub fn of(driver: DriverKind) -> Option<Self> {
        match driver {
            DriverKind::Athena => Some(Self {
                provider: StorageProvider::S3,
                needs_prefix: false,
            }),
            DriverKind::BigQuery => Some(Self {
                // `EXPORT DATA` writes to Cloud Storage; S3 and Blob Storage need BigQuery
                // Omni, which C4 doesn't carry.
                provider: StorageProvider::Gcs,
                needs_prefix: true,
            }),
            DriverKind::Snowflake => Some(Self {
                // `COPY INTO` a location can write to any of the three clouds through a
                // storage integration; C5h carries S3's, the providers' first, since a
                // handoff names one provider.
                provider: StorageProvider::S3,
                needs_prefix: true,
            }),
            DriverKind::Postgres
            | DriverKind::Sqlite
            | DriverKind::Mysql
            | DriverKind::SqlServer
            | DriverKind::DuckDb
            | DriverKind::ClickHouse
            | DriverKind::Redis
            | DriverKind::Mongo
            | DriverKind::Elasticsearch
            | DriverKind::Cassandra
            | DriverKind::DynamoDb
            | DriverKind::Trino => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::S3Bucket;

    #[test]
    fn a_folder_is_an_address_an_engine_writes_to_and_a_key_a_listing_takes() {
        let storage = ObjectStorage::S3(S3Bucket {
            bucket: "sales".to_owned(),
            ..S3Bucket::default()
        });
        let folder = HandoffFolder::new(&storage, "dbrover-tmp/shop/run-1-2/orders/".to_owned());
        assert_eq!(folder.url(), "s3://sales/dbrover-tmp/shop/run-1-2/orders/");
        assert_eq!(folder.key(), "dbrover-tmp/shop/run-1-2/orders/");
        assert_eq!(folder.bucket(), "sales");
        assert_eq!(folder.provider(), StorageProvider::S3);
    }

    #[test]
    fn athena_bigquery_and_snowflake_hand_a_result_over_today() {
        let with_handoff: Vec<DriverKind> = DriverKind::ALL
            .into_iter()
            .filter(|driver| HandoffSupport::of(*driver).is_some())
            .collect();
        assert_eq!(
            with_handoff,
            [
                DriverKind::Athena,
                DriverKind::BigQuery,
                DriverKind::Snowflake
            ]
        );
        // Athena writes every result to S3 anyway, so a report need name no prefix of its own.
        let athena = HandoffSupport::of(DriverKind::Athena).expect("Athena hands over");
        assert!(!athena.needs_prefix);
        let bigquery = HandoffSupport::of(DriverKind::BigQuery).expect("BigQuery hands over");
        assert!(bigquery.needs_prefix);
        assert_eq!(bigquery.provider, StorageProvider::Gcs);
        let snowflake = HandoffSupport::of(DriverKind::Snowflake).expect("Snowflake hands over");
        assert!(snowflake.needs_prefix);
        assert_eq!(snowflake.provider, StorageProvider::S3);
    }
}
