//! BigQuery's dry run: a query job sent with `dryRun`, which BigQuery checks and estimates
//! without running, free and outside the slot quota (`docs/cloud-data.md`, C4). One dry run
//! answers both the console's check as the statement is typed (`Connection::validate`) and the
//! cost guard's question before it runs (`Connection::estimate_cost`), so each answer is kept
//! for its statement's text.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use dbrover_core::{ByteCount, DriverError, Rejection, RejectionKind, ScanEstimate};
use serde::Deserialize;
use serde_json::json;

use crate::api::{Api, ApiError, segment};
use crate::errors;
use crate::jobs::JobPlace;

/// How many statements' answers are kept. A console checks the statements of its text as they
/// change, then runs one of them: a few answers cover the ones in sight.
const KEPT_ANSWERS: usize = 16;

/// What a dry run said about a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DryRun {
    Accepted(ScanEstimate),
    Rejected(Rejection),
}

/// A data source's dry runs, and the last answers.
pub(crate) struct DryRuns {
    api: Api,
    place: JobPlace,
    answers: Mutex<VecDeque<(String, DryRun)>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DryRunJob {
    #[serde(default)]
    statistics: DryRunStatistics,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DryRunStatistics {
    total_bytes_processed: Option<String>,
    query: Option<DryRunQuery>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DryRunQuery {
    total_bytes_processed: Option<String>,
    /// `PRECISE`, `UPPER_BOUND`, `LOWER_BOUND` or `UNKNOWN`.
    total_bytes_processed_accuracy: Option<String>,
}

impl DryRuns {
    pub(crate) fn new(api: Api, place: JobPlace) -> Self {
        Self {
            api,
            place,
            answers: Mutex::new(VecDeque::new()),
        }
    }

    /// What BigQuery says about `statement`: the bytes it would scan, or why it won't run.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when BigQuery can't be asked, or refuses for a reason that
    /// isn't the statement's (the sign-in, the project, its quota).
    pub(crate) async fn check(&self, statement: &str) -> Result<DryRun, DriverError> {
        if let Some(answer) = self.kept(statement) {
            return Ok(answer);
        }
        let path = format!("/projects/{}/jobs", segment(&self.place.project));
        let asked = self
            .api
            .post::<DryRunJob>(&path, &[], &self.body(statement))
            .await;
        let answer = match asked {
            Ok(job) => DryRun::Accepted(job.statistics.estimate()),
            Err(error) => DryRun::Rejected(rejection(error, statement)?),
        };
        self.keep(statement, answer.clone());
        Ok(answer)
    }

    fn body(&self, statement: &str) -> serde_json::Value {
        let mut reference = json!({ "projectId": self.place.project });
        if !self.place.location.is_empty() {
            reference["location"] = json!(self.place.location);
        }
        let mut query = json!({ "query": statement, "useLegacySql": false });
        if !self.place.dataset.is_empty() {
            query["defaultDataset"] =
                json!({ "projectId": self.place.project, "datasetId": self.place.dataset });
        }
        json!({
            "jobReference": reference,
            "configuration": { "dryRun": true, "query": query },
        })
    }

    fn kept(&self, statement: &str) -> Option<DryRun> {
        let answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers
            .iter()
            .find(|(kept, _)| kept == statement)
            .map(|(_, answer)| answer.clone())
    }

    fn keep(&self, statement: &str, answer: DryRun) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers.retain(|(kept, _)| kept != statement);
        if answers.len() == KEPT_ANSWERS {
            answers.pop_front();
        }
        answers.push_back((statement.to_owned(), answer));
    }
}

impl DryRunStatistics {
    fn estimate(&self) -> ScanEstimate {
        let query = self.query.as_ref();
        let bytes = query
            .and_then(|query| query.total_bytes_processed.as_deref())
            .or(self.total_bytes_processed.as_deref())
            .and_then(|bytes| bytes.parse().ok())
            .unwrap_or(0);
        let accuracy = query.and_then(|query| query.total_bytes_processed_accuracy.as_deref());
        ScanEstimate {
            bytes: ByteCount(bytes),
            at_least: matches!(accuracy, Some("LOWER_BOUND" | "UNKNOWN")),
        }
    }
}

/// Why BigQuery won't run `statement`, from its refusal of the dry run: a statement it can't
/// read or whose names it can't find is answered with HTTP 400 or 404. Any other refusal isn't
/// the statement's, and is the connection's error.
fn rejection(error: ApiError, statement: &str) -> Result<Rejection, DriverError> {
    let ApiError::Refused {
        status: 400 | 404,
        reason,
        message,
    } = &error
    else {
        return Err(errors::connection(error));
    };
    let kind = if message.starts_with("Syntax error") {
        RejectionKind::Syntax
    } else if reason.as_deref() == Some("notFound") || message.starts_with("Unrecognized name") {
        RejectionKind::UnknownName
    } else {
        RejectionKind::Other
    };
    Ok(Rejection {
        position: errors::position_in(statement, message),
        message: message.clone(),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(status: u16, reason: &str, message: &str) -> ApiError {
        ApiError::Refused {
            status,
            reason: Some(reason.to_owned()),
            message: message.to_owned(),
        }
    }

    #[test]
    fn refusals_of_the_statement_are_rejections_and_others_are_errors() {
        let statement = "SELECT idd FROM orders";
        let rejected = |error| {
            rejection(error, statement)
                .ok()
                .map(|rejection| (rejection.kind, rejection.position))
        };
        assert_eq!(
            rejected(refused(
                400,
                "invalidQuery",
                "Syntax error: Unexpected keyword FROM at [1:12]"
            )),
            Some((RejectionKind::Syntax, Some(11)))
        );
        assert_eq!(
            rejected(refused(
                400,
                "invalidQuery",
                "Unrecognized name: idd at [1:8]"
            )),
            Some((RejectionKind::UnknownName, Some(7)))
        );
        assert_eq!(
            rejected(refused(
                404,
                "notFound",
                "Not found: Table p:sales.orders was not found in location EU"
            )),
            Some((RejectionKind::UnknownName, None))
        );
        assert_eq!(
            rejected(refused(
                400,
                "invalidQuery",
                "No matching signature for operator + for argument types: STRING, INT64 at [1:8]"
            )),
            Some((RejectionKind::Other, Some(7)))
        );
        assert_eq!(
            rejected(refused(403, "accessDenied", "Access Denied")),
            None
        );
        assert_eq!(
            rejected(refused(429, "rateLimitExceeded", "Too many")),
            None
        );
    }

    #[test]
    fn a_lower_bound_says_so() {
        let statistics = |json: &str| -> DryRunStatistics { serde_json::from_str(json).unwrap() };
        assert_eq!(
            statistics(r#"{"query": {"totalBytesProcessed": "1400000000", "totalBytesProcessedAccuracy": "PRECISE"}}"#).estimate(),
            ScanEstimate::exactly(ByteCount(1_400_000_000))
        );
        assert_eq!(
            statistics(r#"{"query": {"totalBytesProcessed": "0", "totalBytesProcessedAccuracy": "LOWER_BOUND"}}"#).estimate(),
            ScanEstimate { bytes: ByteCount(0), at_least: true }
        );
        assert_eq!(
            statistics(r#"{"totalBytesProcessed": "65"}"#).estimate(),
            ScanEstimate::exactly(ByteCount(65))
        );
    }
}
