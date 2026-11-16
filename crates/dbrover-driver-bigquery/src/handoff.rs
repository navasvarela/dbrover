//! BigQuery's handoff: the result written to Cloud Storage as Parquet by `EXPORT DATA`, for a
//! report to read with DuckDB instead of paging `getQueryResults` (`docs/reports-at-scale.md`,
//! RS7f and RS7g).
//!
//! This is the path C4 left open when it chose not to build the Storage Read API: `EXPORT DATA`
//! needs no gRPC, nothing billed apart from the query, and the files land in a bucket C2
//! already reads.

use dbrover_core::{Handoff, HandoffFiles};

/// What `EXPORT DATA`'s `uri` ends with. BigQuery insists on exactly one wildcard and replaces
/// it with `000000000000` upwards, one key a shard, so the run can't be told the file names in
/// the answer and finds them by listing the folder instead.
const SHARDS: &str = "part-*.parquet";

/// The statement BigQuery runs for `handoff`; `None` where the handoff names no folder, which
/// is a handoff BigQuery can't do — it writes no result file of its own, and planning refuses
/// one without a prefix.
pub(crate) fn statement(sql: &str, handoff: &Handoff) -> Option<String> {
    let Handoff::IntoFolder(folder) = handoff else {
        return None;
    };
    Some(format!(
        "EXPORT DATA OPTIONS (uri = '{}{SHARDS}', format = 'PARQUET', overwrite = true) AS {}",
        folder.url(),
        sql.trim().trim_end_matches(';')
    ))
}

/// What a finished `EXPORT DATA` wrote: whatever is in the folder it was given, which the run
/// owns, so everything in it is the export's own shards.
pub(crate) fn files(handoff: &Handoff) -> Option<HandoffFiles> {
    match handoff {
        Handoff::IntoFolder(_) => Some(HandoffFiles::InFolder),
        Handoff::OwnResult => None,
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{GcsBucket, HandoffFolder, ObjectStorage};

    use super::*;

    fn into_folder(key: &str) -> Handoff {
        let storage = ObjectStorage::Gcs(GcsBucket {
            bucket: "dbrover".to_owned(),
            ..GcsBucket::default()
        });
        Handoff::IntoFolder(HandoffFolder::new(&storage, key.to_owned()))
    }

    #[test]
    fn a_handoff_exports_parquet_shards_into_the_folder_the_run_owns() {
        let handoff = into_folder("dbrover-tmp/shop/run-1-2/orders/");

        assert_eq!(
            statement("SELECT item, amount FROM orders", &handoff).as_deref(),
            Some(
                "EXPORT DATA OPTIONS (uri = \
                 'gs://dbrover/dbrover-tmp/shop/run-1-2/orders/part-*.parquet', \
                 format = 'PARQUET', overwrite = true) AS SELECT item, amount FROM orders"
            )
        );
        assert_eq!(files(&handoff), Some(HandoffFiles::InFolder));
    }

    /// A query a report's file ends with a semicolon still becomes one statement, which
    /// `EXPORT DATA … AS` takes a query for.
    #[test]
    fn a_querys_own_semicolon_and_spacing_are_left_out_of_the_export() {
        let handoff = into_folder("tmp/");

        assert_eq!(
            statement("  SELECT 1;\n", &handoff).as_deref(),
            Some(
                "EXPORT DATA OPTIONS (uri = 'gs://dbrover/tmp/part-*.parquet', \
                 format = 'PARQUET', overwrite = true) AS SELECT 1"
            )
        );
    }

    /// BigQuery writes no result file of its own, so a handoff without a folder is one it
    /// can't do; planning refuses it before a run reaches here.
    #[test]
    fn a_handoff_without_a_folder_is_one_bigquery_cant_do() {
        assert_eq!(statement("SELECT 1", &Handoff::OwnResult), None);
        assert_eq!(files(&Handoff::OwnResult), None);
    }
}
