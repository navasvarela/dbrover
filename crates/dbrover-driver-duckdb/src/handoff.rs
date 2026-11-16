//! The files an engine wrote for a report's handoff (`docs/reports-at-scale.md`, RS7f): the
//! ones to read into the source's table, and the ones to remove once they are read.
//!
//! The bucket is the one the report's `handoff` names, read with that data source's own
//! sign-in through [`BucketSignIn`], as a lake's is. Unlike a lake, a handoff is loaded before
//! [`crate::Workspace::lock`], as every other source is, so none of its prefixes reaches
//! `allowed_directories` and [ADR 0008](../../../docs/adr/0008-a-locked-workspace-reads-a-named-bucket.md)
//! is unchanged.

use dbrover_cloud::storage::{Lister, StoredFile};
use dbrover_core::{CATALOG_PAGE_SIZE, DriverError, LakeFormat};

use crate::lake::BucketSignIn;

/// How many levels of a folder are walked before the walk gives up. A handoff's own files lie
/// directly in the statement's folder; the one level below that is a run folder's sources,
/// which the cleanup walks. Anything deeper is somebody else's.
const MAX_DEPTH: usize = 8;

/// How many pages of one level are listed before the walk gives up, which bounds a folder
/// somebody filled with millions of keys.
const MAX_PAGES: usize = 100;

/// How many deletes are in flight at once. One request per object is what a bucket's API
/// gives (S3's multi-object delete takes a signed body, which [`Lister`] doesn't build), and
/// an `EXPORT DATA` of a large result leaves hundreds of shards, so they go in small batches
/// rather than one at a time.
const DELETES_AT_ONCE: usize = 8;

impl BucketSignIn {
    /// Every file at or below `prefix` (ending in `/`), with its key and size.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when signing in to the bucket fails or it can't be listed.
    pub async fn files_under(&mut self, prefix: &str) -> Result<Vec<StoredFile>, DriverError> {
        let lister = self.handoff_lister().await?;
        let mut files = Vec::new();
        let mut folders = vec![prefix.to_owned()];
        for _ in 0..MAX_DEPTH {
            if folders.is_empty() {
                break;
            }
            let mut below = Vec::new();
            for folder in folders {
                let level = walk(&lister, &folder).await?;
                files.extend(level.files);
                below.extend(level.folders);
            }
            folders = below;
        }
        Ok(files)
    }

    /// The folders directly under `prefix`, each ending in `/`: a report's run folders, which
    /// the cleanup tells apart by their names.
    ///
    /// # Errors
    ///
    /// As [`BucketSignIn::files_under`].
    pub async fn folders_under(&mut self, prefix: &str) -> Result<Vec<String>, DriverError> {
        let lister = self.handoff_lister().await?;
        Ok(walk(&lister, prefix).await?.folders)
    }

    /// Deletes the object at `key`: one file an engine named, where it didn't write into a
    /// folder of the run's own.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when the bucket refuses the delete. A key that is already
    /// gone is not a refusal.
    pub async fn remove_key(&mut self, key: &str) -> Result<(), DriverError> {
        let lister = self.handoff_lister().await?;
        lister
            .delete(key)
            .await
            .map_err(|error| crate::connection_error(format!("could not delete {key}: {error}")))
    }

    /// Deletes every file at or below `prefix`, and answers how many it deleted.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when the bucket can't be listed, or refuses a delete. A key
    /// that is already gone is not a refusal.
    pub async fn remove_under(&mut self, prefix: &str) -> Result<usize, DriverError> {
        let keys: Vec<String> = self
            .files_under(prefix)
            .await?
            .into_iter()
            .map(|file| file.key)
            .collect();
        let lister = self.handoff_lister().await?;
        for batch in keys.chunks(DELETES_AT_ONCE) {
            let deletes = batch.iter().map(|key| lister.delete(key));
            futures_util::future::try_join_all(deletes)
                .await
                .map_err(|error| {
                    crate::connection_error(format!("could not delete {prefix}: {error}"))
                })?;
        }
        Ok(keys.len())
    }
}

/// One level of `folder`: its files and the folders below it, every page of it.
async fn walk(lister: &Lister, folder: &str) -> Result<Level, DriverError> {
    let mut level = Level::default();
    let mut after = None;
    for _ in 0..MAX_PAGES {
        let page = lister
            .list(folder, after.as_deref(), CATALOG_PAGE_SIZE)
            .await
            .map_err(|error| {
                crate::connection_error(format!("could not list {folder}: {error}"))
            })?;
        level.files.extend(page.files);
        level.folders.extend(page.folders);
        after = page.next;
        if after.is_none() {
            return Ok(level);
        }
    }
    Err(crate::connection_error(format!(
        "{folder} holds more keys than DbRover lists for a handoff ({} pages of \
         {CATALOG_PAGE_SIZE}). Give the handoff a prefix of its own, which nothing else \
         writes in.",
        MAX_PAGES
    )))
}

#[derive(Default)]
struct Level {
    files: Vec<StoredFile>,
    folders: Vec<String>,
}

/// The `FROM` clause reading the handoff's files: one reader over every file at once, so
/// DuckDB reads the shards of a large result in parallel.
///
/// `read_csv` is Athena's own result file, whose columns and types DuckDB works out as it
/// would any CSV's; `read_parquet` is an `UNLOAD` or an `EXPORT DATA`, which carry their own.
pub(crate) fn files_clause(files: &[String], format: LakeFormat) -> Result<String, String> {
    if files.is_empty() {
        return Err("the engine wrote no files".to_owned());
    }
    let addresses = files
        .iter()
        .map(|file| crate::workspace::string_literal(file))
        .collect::<Vec<_>>()
        .join(", ");
    let reader = match format {
        LakeFormat::Parquet => "read_parquet",
        LakeFormat::Csv => "read_csv",
        LakeFormat::Json => "read_json",
        // No engine hands a table format over: a handoff is a result written once, not a
        // table with a log or a snapshot.
        LakeFormat::Iceberg | LakeFormat::Delta => {
            return Err(format!("{} files can't be handed over", format.name()));
        }
    };
    Ok(format!("{reader}([{addresses}])"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_read_by_one_reader_so_the_shards_are_read_together() {
        let files = [
            "s3://sales/tmp/run-1/orders/part-0.parquet".to_owned(),
            "s3://sales/tmp/run-1/orders/part-1.parquet".to_owned(),
        ];

        assert_eq!(
            files_clause(&files, LakeFormat::Parquet).unwrap(),
            "read_parquet(['s3://sales/tmp/run-1/orders/part-0.parquet', \
             's3://sales/tmp/run-1/orders/part-1.parquet'])"
        );
    }

    #[test]
    fn athenas_own_result_file_is_read_as_the_csv_it_is() {
        let files = ["s3://sales/athena/4c3-2b1.csv".to_owned()];

        assert_eq!(
            files_clause(&files, LakeFormat::Csv).unwrap(),
            "read_csv(['s3://sales/athena/4c3-2b1.csv'])"
        );
    }

    #[test]
    fn a_handoff_of_no_files_or_of_a_table_format_is_a_problem() {
        assert!(files_clause(&[], LakeFormat::Parquet).is_err());
        let files = ["s3://sales/tmp/table".to_owned()];
        assert!(files_clause(&files, LakeFormat::Iceberg).is_err());
        assert!(files_clause(&files, LakeFormat::Delta).is_err());
    }
}
