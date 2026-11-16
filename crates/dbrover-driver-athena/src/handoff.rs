//! Athena's handoff: a result read from S3 as files, instead of paged a thousand rows at a
//! time through `GetQueryResults` (`docs/reports-at-scale.md`, RS7f).
//!
//! Athena writes every result to S3 anyway, so a `handoff` that names no prefix of its own
//! reads the job's own result file, whose location `GetQueryExecution` gives. A `handoff` that
//! names a prefix gets Parquet instead, through `UNLOAD`, which writes into the folder the run
//! owns.

use dbrover_core::{Handoff, HandoffFiles, LakeFormat};

/// What Athena writes beside a result file: the column types of the result, which has to go
/// with it when the run deletes what it read.
const RESULT_METADATA: &str = ".metadata";

/// The statement Athena runs for `handoff`, and the format it writes.
///
/// `UNLOAD` takes the query as it is, so the parameters a driver binds stay bound and the
/// query's own text is never rewritten. Its destination must be empty, which the run's own
/// folder is.
pub(crate) fn statement(sql: &str, handoff: &Handoff) -> (String, LakeFormat) {
    match handoff {
        Handoff::OwnResult => (sql.to_owned(), LakeFormat::Csv),
        Handoff::IntoFolder(folder) => (
            format!(
                "UNLOAD ({}) TO '{}' WITH (format = 'parquet')",
                sql.trim().trim_end_matches(';'),
                folder.url()
            ),
            LakeFormat::Parquet,
        ),
    }
}

/// The files a finished job wrote: for an `UNLOAD`, whatever is in the folder it was given,
/// which the run lists; otherwise the job's own result file at `location`, read as the CSV it
/// is, and deleted with the metadata file Athena writes beside it.
///
/// Athena's `UNLOAD` also writes a manifest in the workgroup's own output location, beside
/// every other query's result, which the run doesn't name and so doesn't remove: the
/// workgroup's own lifecycle rule covers it, as it covers every result Athena leaves there.
pub(crate) fn files(handoff: &Handoff, location: Option<&str>) -> Option<HandoffFiles> {
    match handoff {
        Handoff::IntoFolder(_) => Some(HandoffFiles::InFolder),
        Handoff::OwnResult => {
            let location = location?;
            Some(HandoffFiles::Named {
                read: vec![location.to_owned()],
                remove: vec![location.to_owned(), format!("{location}{RESULT_METADATA}")],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{HandoffFolder, ObjectStorage, S3Bucket};

    use super::*;

    fn into_folder(key: &str) -> Handoff {
        let storage = ObjectStorage::S3(S3Bucket {
            bucket: "sales".to_owned(),
            ..S3Bucket::default()
        });
        Handoff::IntoFolder(HandoffFolder::new(&storage, key.to_owned()))
    }

    #[test]
    fn a_handoff_with_a_prefix_unloads_parquet_into_the_folder_the_run_owns() {
        let handoff = into_folder("dbrover-tmp/shop/run-1-2/orders/");

        let (sent, format) = statement("SELECT * FROM orders WHERE day = ?1", &handoff);

        assert_eq!(
            sent,
            "UNLOAD (SELECT * FROM orders WHERE day = ?1) TO \
             's3://sales/dbrover-tmp/shop/run-1-2/orders/' WITH (format = 'parquet')"
        );
        assert_eq!(format, LakeFormat::Parquet);
        assert_eq!(files(&handoff, None), Some(HandoffFiles::InFolder));
    }

    /// A query a report's file ends with a semicolon still becomes one `UNLOAD`, which takes a
    /// query and not a statement.
    #[test]
    fn a_querys_own_semicolon_and_spacing_are_left_out_of_the_unload() {
        let handoff = into_folder("tmp/");

        let (sent, _) = statement("  SELECT 1;\n", &handoff);

        assert_eq!(
            sent,
            "UNLOAD (SELECT 1) TO 's3://sales/tmp/' WITH (format = 'parquet')"
        );
    }

    #[test]
    fn a_handoff_without_a_prefix_reads_the_jobs_own_result_file_and_removes_its_metadata() {
        let (sent, format) = statement("SELECT 1", &Handoff::OwnResult);

        assert_eq!(sent, "SELECT 1");
        assert_eq!(format, LakeFormat::Csv);
        assert_eq!(
            files(
                &Handoff::OwnResult,
                Some("s3://sales/athena/4c3-2b1-9f0.csv")
            ),
            Some(HandoffFiles::Named {
                read: vec!["s3://sales/athena/4c3-2b1-9f0.csv".to_owned()],
                remove: vec![
                    "s3://sales/athena/4c3-2b1-9f0.csv".to_owned(),
                    "s3://sales/athena/4c3-2b1-9f0.csv.metadata".to_owned(),
                ],
            })
        );
    }

    /// A job that ended without naming where its result went has nothing to hand over, which
    /// the run reports rather than reading an address it guessed.
    #[test]
    fn a_job_that_names_no_result_file_hands_nothing_over() {
        assert_eq!(files(&Handoff::OwnResult, None), None);
    }
}
