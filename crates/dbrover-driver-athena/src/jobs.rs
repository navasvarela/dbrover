//! Athena's query executions as C0's jobs: `StartQueryExecution`, `GetQueryExecution`,
//! `GetQueryResults` and `StopQueryExecution`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use aws_sdk_athena::Client;
use aws_sdk_athena::types::{QueryExecution, QueryExecutionContext, QueryExecutionState};
use aws_sdk_athena::types::{QueryExecutionStatus, ResultConfiguration};
use dbrover_core::{
    ByteCount, DriverError, JobFailure, JobId, JobPage, JobProgress, JobService, JobState,
    PageToken,
};

use crate::errors::{self, Refused};
use crate::results;

/// The most rows `GetQueryResults` returns in one page, as Athena's API reference sets it.
const MAX_PAGE_ROWS: usize = 1000;

/// Where a data source's queries run, and where their results go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryPlace {
    pub workgroup: String,
    pub catalog: String,
    pub database: String,
    /// The location to send with each query: `None` when the workgroup's own is used.
    pub output_location: Option<String>,
}

/// A data source's queries on one Athena client.
pub(crate) struct AthenaJobs {
    client: Client,
    place: QueryPlace,
    /// Each running query's statement, to turn the line and column an error names into a
    /// position in it. Removed when the query ends.
    statements: Mutex<HashMap<JobId, String>>,
}

impl AthenaJobs {
    pub(crate) fn new(client: Client, place: QueryPlace) -> Self {
        Self {
            client,
            place,
            statements: Mutex::new(HashMap::new()),
        }
    }

    fn statements(&self) -> std::sync::MutexGuard<'_, HashMap<JobId, String>> {
        self.statements
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Why the query failed, pointing into its statement where Athena's message does.
    fn failure(&self, job: &JobId, status: Option<&QueryExecutionStatus>) -> JobFailure {
        let message = status.map_or_else(
            || "Athena gave no reason".to_owned(),
            errors::failure_message,
        );
        let statement = self.statements().remove(job);
        let position = statement
            .as_deref()
            .and_then(|statement| errors::position_in(statement, &message));
        JobFailure { message, position }
    }
}

#[async_trait]
impl JobService for AthenaJobs {
    async fn submit(&self, statement: &str) -> Result<JobId, DriverError> {
        let context = QueryExecutionContext::builder()
            .catalog(&self.place.catalog)
            .database(&self.place.database)
            .build();
        let mut request = self
            .client
            .start_query_execution()
            .query_string(statement)
            .query_execution_context(context)
            .work_group(&self.place.workgroup);
        if let Some(location) = &self.place.output_location {
            request = request.result_configuration(
                ResultConfiguration::builder()
                    .output_location(location)
                    .build(),
            );
        }
        let started = request
            .send()
            .await
            .map_err(|error| errors::statement(Refused::from(error), statement))?;
        let job = started
            .query_execution_id()
            .map(JobId::new)
            .ok_or_else(|| {
                DriverError::Connection("Athena started the query without naming it".into())
            })?;
        self.statements().insert(job.clone(), statement.to_owned());
        Ok(job)
    }

    async fn poll(&self, job: &JobId) -> Result<JobState, DriverError> {
        let output = self
            .client
            .get_query_execution()
            .query_execution_id(job.as_str())
            .send()
            .await
            .map_err(|error| errors::connection(Refused::from(error)))?;
        let Some(execution) = output.query_execution() else {
            return Ok(JobState::Failed(JobFailure::new(
                "Athena answered without the query execution",
            )));
        };
        let progress = progress(execution);
        let status = execution.status();
        Ok(match status.and_then(QueryExecutionStatus::state) {
            Some(QueryExecutionState::Succeeded) => {
                self.statements().remove(job);
                JobState::Done {
                    progress,
                    rows_changed: None,
                }
            }
            Some(QueryExecutionState::Failed | QueryExecutionState::Cancelled) => {
                JobState::Failed(self.failure(job, status))
            }
            // Queued, running, and any state Athena adds later: asked again after a pause.
            _ => JobState::Running(progress),
        })
    }

    async fn page(
        &self,
        job: &JobId,
        after: Option<&PageToken>,
        max_rows: usize,
    ) -> Result<JobPage, DriverError> {
        let wanted = max_rows.clamp(1, MAX_PAGE_ROWS);
        let output = self
            .client
            .get_query_results()
            .query_execution_id(job.as_str())
            .set_next_token(after.map(|token| token.as_str().to_owned()))
            .max_results(i32::try_from(wanted).unwrap_or(i32::MAX))
            .send()
            .await
            .map_err(|error| errors::connection(Refused::from(error)))?;
        Ok(results::page(&output, after.is_none()))
    }

    async fn cancel(&self, job: &JobId) -> Result<(), DriverError> {
        self.client
            .stop_query_execution()
            .query_execution_id(job.as_str())
            .send()
            .await
            .map(drop)
            .map_err(|error| errors::connection(Refused::from(error)))
    }
}

/// What a query has scanned so far, which is what Athena bills. Athena doesn't count rows
/// until the result is read.
fn progress(execution: &QueryExecution) -> JobProgress {
    let scanned = execution
        .statistics()
        .and_then(|statistics| statistics.data_scanned_in_bytes())
        .and_then(|bytes| u64::try_from(bytes).ok());
    JobProgress {
        rows_read: None,
        bytes_read: scanned.map(ByteCount),
    }
}
