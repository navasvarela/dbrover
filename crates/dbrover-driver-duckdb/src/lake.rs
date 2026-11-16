//! A lake a report's combine step reads in place (`docs/reports-at-scale.md`, RS7): the
//! extensions DuckDB needs for it, the view the combine query reads it through, and the
//! bucket's credentials, from the data source's C2 sign-in.
//!
//! Everything here happens before [`crate::Workspace::lock`], which then allows the lake's
//! prefix and nothing else: extensions can't be loaded after the lock, and the prefix can't be
//! changed ([ADR 0008](../../../docs/adr/0008-a-locked-workspace-reads-a-named-bucket.md)).
//! The one exception is the bucket's secret, which the lock lets DbRover replace while the
//! combine runs, before the credentials it was made from expire.

use std::sync::Arc;
use std::time::SystemTime;

use dbrover_cloud::storage::Lister;
use dbrover_core::{
    DriverError, LakeFormat, LakePath, ObjectStorage, Password, StorageProvider, TableFormat,
};

use crate::bucket::{Bucket, BucketError, SecretStatement};
use crate::workspace::{MAIN_SCHEMA, identifier, string_literal};

/// An extension DuckDB downloads on first use, which a lake needs loaded before the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DuckDbExtension {
    /// S3 and Google Cloud Storage, and every `http://` or `https://` address.
    Httpfs,
    /// Azure Blob Storage, which `httpfs` doesn't serve.
    Azure,
    Iceberg,
    Delta,
}

impl DuckDbExtension {
    /// The name `INSTALL` and `LOAD` take, which is also what an error about it says.
    pub fn name(self) -> &'static str {
        match self {
            Self::Httpfs => "httpfs",
            Self::Azure => "azure",
            Self::Iceberg => "iceberg",
            Self::Delta => "delta",
        }
    }

    /// What reading a lake of `format` in `storage` needs: the provider's file system, and the
    /// table format's reader where it is one.
    ///
    /// They are loaded before the bucket's secret, which DuckDB refuses until the provider's
    /// own extension holds its type, and before the lock, which loads none.
    pub fn of(storage: &ObjectStorage, format: LakeFormat) -> Vec<Self> {
        let provider = match storage.provider() {
            StorageProvider::S3 | StorageProvider::Gcs => Self::Httpfs,
            StorageProvider::Azure => Self::Azure,
        };
        let reader = match format {
            LakeFormat::Iceberg => Some(Self::Iceberg),
            LakeFormat::Delta => Some(Self::Delta),
            LakeFormat::Parquet | LakeFormat::Csv | LakeFormat::Json => None,
        };
        [Some(provider), reader].into_iter().flatten().collect()
    }
}

/// A lake as the workspace reads it: the view it is read through, what it is read from, the
/// extensions that reader needs, and the prefix the lock allows for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeView {
    /// The view's name in the workspace's `main` schema: the source's id.
    id: String,
    /// The `FROM` clause: `read_parquet('s3://…')`, `delta_scan('s3://…')`.
    from: String,
    prefix: String,
    extensions: Vec<DuckDbExtension>,
}

impl LakeView {
    /// The view over `path`, read as `format` says.
    ///
    /// `read_from` is the address the reader is given, which is `path`'s own for every format
    /// but Iceberg, whose newest metadata file [`LakeTable::newest_metadata`] finds; it must be
    /// under `path`'s prefix, which is what the lock allows.
    pub fn new(
        id: &str,
        storage: &ObjectStorage,
        path: &LakePath,
        format: LakeFormat,
        read_from: &str,
        hive_partitioning: bool,
    ) -> Self {
        let address = string_literal(read_from);
        let from = match format {
            LakeFormat::Parquet if hive_partitioning => {
                format!("read_parquet({address}, hive_partitioning = true)")
            }
            LakeFormat::Parquet => format!("read_parquet({address})"),
            LakeFormat::Csv if hive_partitioning => {
                format!("read_csv({address}, hive_partitioning = true)")
            }
            LakeFormat::Csv => format!("read_csv({address})"),
            LakeFormat::Json if hive_partitioning => {
                format!("read_json({address}, hive_partitioning = true)")
            }
            LakeFormat::Json => format!("read_json({address})"),
            LakeFormat::Iceberg => format!("iceberg_scan({address})"),
            LakeFormat::Delta => format!("delta_scan({address})"),
        };
        Self {
            id: id.to_owned(),
            from,
            prefix: path.prefix(format),
            extensions: DuckDbExtension::of(storage, format),
        }
    }

    /// The statement making the view, which the combine query then reads as a table.
    pub(crate) fn statement(&self) -> String {
        format!(
            "CREATE OR REPLACE VIEW {MAIN_SCHEMA}.{} AS SELECT * FROM {}",
            identifier(&self.id),
            self.from
        )
    }

    /// The prefix a locked workspace may read under for this lake, ending in `/`.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn extensions(&self) -> &[DuckDbExtension] {
        &self.extensions
    }

    /// What the view reads from, for the plan and the profile.
    pub fn reads_from(&self) -> &str {
        &self.from
    }
}

/// The glob that matches the Parquet files of a table format's folder, whose layout is the
/// table's own: Iceberg keeps them under `data/`, Delta beside its `_delta_log/`.
const TABLE_PARQUET_FILES: &str = "/**/*.parquet";

impl LakeView {
    /// What `parquet_metadata` is read over to size the lake: the path itself for Parquet
    /// files, and every Parquet file under the folder for a table format, which is more than
    /// the table's current snapshot holds, so the size is an upper bound.
    pub(crate) fn footer_glob(path: &LakePath, format: LakeFormat) -> Option<String> {
        match format {
            LakeFormat::Parquet => Some(path.url().to_owned()),
            LakeFormat::Iceberg | LakeFormat::Delta => {
                Some(format!("{}{TABLE_PARQUET_FILES}", path.url()))
            }
            // A text lake has no footer to read, and a listing gives a file's size only level
            // by level; the manual says the guard can't hold one.
            LakeFormat::Csv | LakeFormat::Json => None,
        }
    }
}

/// A bucket's credentials for a workspace, made from the data source's C2 sign-in and made
/// again before they expire.
///
/// It is the sign-in C2 gives a connection on the same data source, kept apart from it: a run
/// signs in for its workspace, not through the data source's connection, which it never opens
/// for a lake.
pub struct BucketSignIn {
    storage: ObjectStorage,
    bucket: Bucket,
}

impl BucketSignIn {
    /// Signs in to `storage` with `secret`, the data source's password, where its sign-in
    /// takes one (an access key's).
    pub fn new(storage: ObjectStorage, secret: Option<Password>) -> Self {
        Self {
            bucket: Bucket::new(storage.clone(), secret),
            storage,
        }
    }

    pub fn storage(&self) -> &ObjectStorage {
        &self.storage
    }

    /// The statement handing DuckDB the bucket's credentials, if it holds none made from a
    /// sign-in still fresh at `now`; `None` when the secret it holds is still good.
    ///
    /// # Errors
    ///
    /// [`BucketError`] when signing in to the bucket fails.
    pub(crate) async fn due_secret(
        &mut self,
        now: SystemTime,
    ) -> Result<Option<SecretStatement>, BucketError> {
        self.bucket.due_secret(now).await
    }

    /// Records that DuckDB took the secret [`BucketSignIn::due_secret`] gave.
    pub(crate) fn made(&mut self) {
        self.bucket.made();
    }

    /// The address an Iceberg table at `path` is read from: its newest metadata file, found by
    /// listing the table's metadata folder, as the explorer finds it (C2).
    ///
    /// DuckDB reads an Iceberg table from a metadata file, and refuses to guess the newest one
    /// unless `unsafe_enable_version_guessing` is set, which would read uncommitted data.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when signing in to the bucket fails, when the bucket can't
    /// be listed, or when the table's metadata folder holds no metadata file.
    pub async fn iceberg_metadata(&mut self, path: &LakePath) -> Result<String, DriverError> {
        let prefix = format!("{}/", path.key());
        let lister = self
            .lister(SystemTime::now())
            .await
            .map_err(crate::connection_error)?;
        let name = crate::table_source_name(&lister, &prefix, TableFormat::Iceberg)
            .await?
            .ok_or_else(|| {
                crate::connection_error(format!(
                    "{} holds no Iceberg metadata file: {}metadata/ has none, so there is no \
                     table to read. An Iceberg lake names the table's folder.",
                    path.url(),
                    path.url()
                ))
            })?;
        Ok(format!("{}/{name}", path.url()))
    }

    /// The client that lists the bucket, signed in as DuckDB's secret is.
    async fn lister(&mut self, now: SystemTime) -> Result<Arc<Lister>, BucketError> {
        self.bucket.lister(now).await
    }

    /// The client that lists and deletes a handoff's files, signed in now (RS7f).
    pub(crate) async fn handoff_lister(&mut self) -> Result<Arc<Lister>, DriverError> {
        self.lister(SystemTime::now())
            .await
            .map_err(crate::connection_error)
    }

    /// Makes the sign-in expire at `at`, as temporary credentials do.
    #[cfg(test)]
    pub(crate) fn expire_at(&mut self, at: SystemTime) {
        self.bucket.expire_at(at);
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{AzureContainer, GcsBucket, S3Bucket};

    use super::*;

    fn s3() -> ObjectStorage {
        ObjectStorage::S3(S3Bucket {
            bucket: "lake".to_owned(),
            ..S3Bucket::default()
        })
    }

    fn view(path: &str, format: LakeFormat, hive: bool) -> LakeView {
        let storage = s3();
        let lake = LakePath::new(&storage, path).unwrap();
        LakeView::new("sales", &storage, &lake, format, lake.url(), hive)
    }

    #[test]
    fn each_format_is_read_with_the_reader_and_the_extensions_duckdb_needs_for_it() {
        let cases = [
            (
                view("sales/year=*/*.parquet", LakeFormat::Parquet, true),
                "read_parquet('s3://lake/sales/year=*/*.parquet', hive_partitioning = true)",
                vec![DuckDbExtension::Httpfs],
            ),
            (
                view("sales/*.parquet", LakeFormat::Parquet, false),
                "read_parquet('s3://lake/sales/*.parquet')",
                vec![DuckDbExtension::Httpfs],
            ),
            (
                view("customers.csv", LakeFormat::Csv, false),
                "read_csv('s3://lake/customers.csv')",
                vec![DuckDbExtension::Httpfs],
            ),
            (
                view("events/*.json", LakeFormat::Json, false),
                "read_json('s3://lake/events/*.json')",
                vec![DuckDbExtension::Httpfs],
            ),
            (
                view("warehouse/products", LakeFormat::Delta, false),
                "delta_scan('s3://lake/warehouse/products')",
                vec![DuckDbExtension::Httpfs, DuckDbExtension::Delta],
            ),
        ];
        for (view, from, extensions) in cases {
            assert_eq!(view.reads_from(), from);
            assert_eq!(view.extensions(), extensions, "{from}");
        }
    }

    #[test]
    fn an_iceberg_table_is_read_from_the_metadata_file_under_its_own_prefix() {
        let storage = s3();
        let lake = LakePath::new(&storage, "warehouse/shop/orders").unwrap();
        let newest = "s3://lake/warehouse/shop/orders/metadata/00002-a.metadata.json";

        let view = LakeView::new(
            "orders",
            &storage,
            &lake,
            LakeFormat::Iceberg,
            newest,
            false,
        );

        assert_eq!(view.reads_from(), format!("iceberg_scan('{newest}')"));
        assert_eq!(view.prefix(), "s3://lake/warehouse/shop/orders/");
        assert!(newest.starts_with(view.prefix()));
        assert_eq!(
            view.extensions(),
            [DuckDbExtension::Httpfs, DuckDbExtension::Iceberg]
        );
    }

    #[test]
    fn azure_is_read_through_its_own_extension_and_google_cloud_storage_through_httpfs() {
        let azure = ObjectStorage::Azure(AzureContainer {
            account: "acme".to_owned(),
            container: "raw".to_owned(),
            ..AzureContainer::default()
        });
        let gcs = ObjectStorage::Gcs(GcsBucket {
            bucket: "lake".to_owned(),
            ..GcsBucket::default()
        });
        let of = |storage: &ObjectStorage| {
            let lake = LakePath::new(storage, "sales/*.parquet").unwrap();
            LakeView::new(
                "sales",
                storage,
                &lake,
                LakeFormat::Parquet,
                lake.url(),
                false,
            )
        };

        assert_eq!(of(&azure).extensions(), [DuckDbExtension::Azure]);
        assert_eq!(
            of(&azure).reads_from(),
            "read_parquet('az://raw/sales/*.parquet')"
        );
        assert_eq!(of(&gcs).extensions(), [DuckDbExtension::Httpfs]);
        assert_eq!(
            of(&gcs).reads_from(),
            "read_parquet('gs://lake/sales/*.parquet')"
        );
    }

    #[test]
    fn a_views_name_and_a_lakes_address_are_written_as_duckdb_literals() {
        let storage = s3();
        let lake = LakePath::new(&storage, "sales/o'brien/*.parquet").unwrap();
        let odd = LakeView::new(
            "the \"sales\" lake",
            &storage,
            &lake,
            LakeFormat::Parquet,
            lake.url(),
            false,
        );

        assert_eq!(
            odd.statement(),
            "CREATE OR REPLACE VIEW main.\"the \"\"sales\"\" lake\" AS SELECT * FROM \
             read_parquet('s3://lake/sales/o''brien/*.parquet')"
        );
    }
}
