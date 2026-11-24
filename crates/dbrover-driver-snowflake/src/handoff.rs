//! Snowflake's handoff: a result unloaded as Parquet into the folder a report's run owns, with
//! `COPY INTO <location>`, for the report to read with DuckDB instead of paging partitions
//! (`docs/cloud-data.md`, C5h; `docs/reports-at-scale.md`, RS7).
//!
//! Snowflake writes to a bucket only through a storage integration, an account object an
//! administrator creates that holds Snowflake's own access to it; DbRover never creates one,
//! and the data source names it.

use dbrover_core::{Handoff, HandoffFiles};

use crate::catalog::{literal, quoted};

/// What the unloaded files' names start with: Snowflake adds `_0_0_0.snappy.parquet` and so on,
/// one file per thread and slice, so the run finds them by listing the folder.
const FILES: &str = "data";

/// The `COPY INTO` Snowflake runs for `handoff` through `integration`; `None` where the
/// handoff names no folder, which Snowflake can't do (it writes no result file of its own, and
/// planning refuses a handoff without a prefix).
pub(crate) fn statement(sql: &str, handoff: &Handoff, integration: &str) -> Option<String> {
    let Handoff::IntoFolder(folder) = handoff else {
        return None;
    };
    Some(format!(
        "COPY INTO {} FROM ({}) STORAGE_INTEGRATION = {} FILE_FORMAT = (TYPE = PARQUET) \
         HEADER = TRUE",
        literal(&format!("{}{FILES}", folder.url())),
        sql.trim().trim_end_matches(';'),
        quoted(integration)
    ))
}

/// What a finished `COPY INTO` wrote: whatever is in the folder it was given, which the run
/// owns, so everything in it is the unload's own files.
pub(crate) fn files(handoff: &Handoff) -> Option<HandoffFiles> {
    match handoff {
        Handoff::IntoFolder(_) => Some(HandoffFiles::InFolder),
        Handoff::OwnResult => None,
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{HandoffFolder, ObjectStorage, S3Bucket};

    use super::*;

    fn into_folder(key: &str) -> Handoff {
        let storage = ObjectStorage::S3(S3Bucket {
            bucket: "acme-lake".to_owned(),
            ..S3Bucket::default()
        });
        Handoff::IntoFolder(HandoffFolder::new(&storage, key.to_owned()))
    }

    #[test]
    fn a_handoff_unloads_parquet_into_the_folder_the_run_owns() {
        let handoff = into_folder("dbrover-tmp/shop/run-1-2/orders/");
        assert_eq!(
            statement(
                "SELECT item, amount FROM orders;\n",
                &handoff,
                "LAKE_WRITER"
            )
            .as_deref(),
            Some(
                "COPY INTO 's3://acme-lake/dbrover-tmp/shop/run-1-2/orders/data' FROM (SELECT \
                 item, amount FROM orders) STORAGE_INTEGRATION = \"LAKE_WRITER\" FILE_FORMAT = \
                 (TYPE = PARQUET) HEADER = TRUE"
            )
        );
        assert_eq!(files(&handoff), Some(HandoffFiles::InFolder));
        assert_eq!(
            statement("SELECT 1", &Handoff::OwnResult, "LAKE_WRITER"),
            None
        );
        assert_eq!(files(&Handoff::OwnResult), None);
    }
}
