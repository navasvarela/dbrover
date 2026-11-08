//! BigQuery's query jobs as C0's jobs: `jobs.insert`, `jobs.get`, `jobs.getQueryResults` and
//! `jobs.cancel`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use dbrover_core::{
    ByteCount, DriverError, JobFailure, JobId, JobPage, JobProgress, JobService, JobState,
    PageToken,
};
use serde::Deserialize;
use serde_json::json;

use crate::api::{Api, segment};
use crate::errors;
use crate::results::{self, ApiRow, Schema};

/// The most rows DbRover asks `getQueryResults` for in one page. BigQuery also caps a page at
/// 10 MB of JSON, so a page of wide rows may hold fewer.
const MAX_PAGE_ROWS: usize = 10_000;

/// What starts every job id DbRover chooses, so a project's job history shows which are
/// DbRover's.
const JOB_ID_PREFIX: &str = "dbrover_";

/// Where a data source's jobs run, and what unqualified names mean in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobPlace {
    /// The project jobs run in and are billed to.
    pub project: String,
    /// The default dataset; empty for none.
    pub dataset: String,
    /// The location to run in; empty to let BigQuery choose from the tables a query reads.
    pub location: String,
}

/// A job DbRover started and hasn't seen end.
struct Started {
    /// Where BigQuery runs it, which every later call about it must name.
    location: Option<String>,
    statement: String,
}

/// A data source's jobs on one API client.
pub(crate) struct BigQueryJobs {
    api: Api,
    place: JobPlace,
    started: Mutex<HashMap<JobId, Started>>,
}

impl BigQueryJobs {
    pub(crate) fn new(api: Api, place: JobPlace) -> Self {
        Self {
            api,
            place,
            started: Mutex::new(HashMap::new()),
        }
    }

    fn started(&self) -> MutexGuard<'_, HashMap<JobId, Started>> {
        self.started.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The location to send with a call about `job`, as BigQuery named it when it started.
    fn location_of(&self, job: &JobId) -> Option<String> {
        self.started()
            .get(job)
            .and_then(|started| started.location.clone())
            .or_else(|| Some(self.place.location.clone()).filter(|location| !location.is_empty()))
    }

    fn job_path(&self, job: &JobId) -> String {
        format!(
            "/projects/{}/jobs/{}",
            segment(&self.place.project),
            segment(job.as_str())
        )
    }

    /// The body of `jobs.insert` for `statement`, as job `job`.
    fn insert_body(&self, job: &JobId, statement: &str) -> serde_json::Value {
        let mut reference = json!({ "projectId": self.place.project, "jobId": job.as_str() });
        if !self.place.location.is_empty() {
            reference["location"] = json!(self.place.location);
        }
        let mut query = json!({ "query": statement, "useLegacySql": false });
        if !self.place.dataset.is_empty() {
            query["defaultDataset"] =
                json!({ "projectId": self.place.project, "datasetId": self.place.dataset });
        }
        json!({ "jobReference": reference, "configuration": { "query": query } })
    }

    /// Why `job` failed, pointing into its statement where BigQuery's message does.
    fn failure(&self, job: &JobId, error: &ErrorResult) -> JobFailure {
        let statement = self.started().remove(job).map(|started| started.statement);
        let position = statement
            .as_deref()
            .and_then(|statement| errors::position_in(statement, &error.message));
        JobFailure {
            message: error.message.clone(),
            position,
        }
    }
}

/// A new job's id: DbRover's own, so that an insert retried after a dropped connection finds
/// the job it started rather than starting another.
fn new_job_id() -> JobId {
    JobId::new(format!("{JOB_ID_PREFIX}{}", uuid::Uuid::new_v4().simple()))
}

/// The parts of a job resource DbRover reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    #[serde(default)]
    job_reference: JobReference,
    #[serde(default)]
    status: JobStatus,
    #[serde(default)]
    statistics: JobStatistics,
}

#[derive(Debug, Default, Deserialize)]
struct JobReference {
    location: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobStatus {
    /// `PENDING`, `RUNNING` or `DONE`.
    #[serde(default)]
    state: String,
    error_result: Option<ErrorResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorResult {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobStatistics {
    total_bytes_processed: Option<String>,
    query: Option<QueryStatistics>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryStatistics {
    total_bytes_processed: Option<String>,
    num_dml_affected_rows: Option<String>,
}

impl JobStatistics {
    /// What the job has processed so far, which on-demand pricing bills (with BigQuery's
    /// minimum per table). BigQuery doesn't count the rows a query reads.
    fn progress(&self) -> JobProgress {
        let processed = self
            .query
            .as_ref()
            .and_then(|query| query.total_bytes_processed.as_deref())
            .or(self.total_bytes_processed.as_deref())
            .and_then(|bytes| bytes.parse().ok());
        JobProgress {
            rows_read: None,
            bytes_read: processed.map(ByteCount),
        }
    }

    fn rows_changed(&self) -> Option<u64> {
        self.query
            .as_ref()
            .and_then(|query| query.num_dml_affected_rows.as_deref())
            .and_then(|rows| rows.parse().ok())
    }
}

/// The parts of `getQueryResults`' answer DbRover reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResults {
    schema: Option<Schema>,
    #[serde(default)]
    rows: Vec<ApiRow>,
    page_token: Option<String>,
    num_dml_affected_rows: Option<String>,
}

#[async_trait]
impl JobService for BigQueryJobs {
    async fn submit(&self, statement: &str) -> Result<JobId, DriverError> {
        let job = new_job_id();
        let path = format!("/projects/{}/jobs", segment(&self.place.project));
        let started: Job = self
            .api
            .post(&path, &[], &self.insert_body(&job, statement))
            .await
            .map_err(|error| errors::statement(error, statement))?;
        self.started().insert(
            job.clone(),
            Started {
                location: started.job_reference.location,
                statement: statement.to_owned(),
            },
        );
        Ok(job)
    }

    async fn poll(&self, job: &JobId) -> Result<JobState, DriverError> {
        let location = self.location_of(job);
        let query: Vec<(&str, &str)> = location
            .iter()
            .map(|location| ("location", location.as_str()))
            .collect();
        let found: Job = self
            .api
            .get(&self.job_path(job), &query)
            .await
            .map_err(errors::connection)?;
        let progress = found.statistics.progress();
        if found.status.state != "DONE" {
            return Ok(JobState::Running(progress));
        }
        if let Some(error) = &found.status.error_result {
            return Ok(JobState::Failed(self.failure(job, error)));
        }
        // Its statement is needed no more, but its location is, for its pages. What is left of
        // a finished job is its id and location, a few dozen bytes for each statement the
        // connection runs.
        if let Some(started) = self.started().get_mut(job) {
            started.statement = String::new();
        }
        Ok(JobState::Done {
            progress,
            rows_changed: found.statistics.rows_changed(),
        })
    }

    async fn page(
        &self,
        job: &JobId,
        after: Option<&PageToken>,
        max_rows: usize,
    ) -> Result<JobPage, DriverError> {
        let wanted = max_rows.clamp(1, MAX_PAGE_ROWS).to_string();
        let location = self.location_of(job);
        let mut query = vec![
            ("maxResults", wanted.as_str()),
            ("formatOptions.useInt64Timestamp", "true"),
        ];
        if let Some(location) = &location {
            query.push(("location", location));
        }
        if let Some(after) = after {
            query.push(("pageToken", after.as_str()));
        }
        let path = format!(
            "/projects/{}/queries/{}",
            segment(&self.place.project),
            segment(job.as_str())
        );
        let results: QueryResults = self
            .api
            .get(&path, &query)
            .await
            .map_err(errors::connection)?;
        let schema = results.schema.unwrap_or_default();
        Ok(JobPage {
            columns: schema.fields.iter().map(results::Field::column).collect(),
            rows: results::rows(&schema, &results.rows),
            next: results.page_token.map(PageToken::new),
            rows_changed: results
                .num_dml_affected_rows
                .filter(|_| schema.fields.is_empty())
                .and_then(|rows| rows.parse().ok()),
        })
    }

    async fn cancel(&self, job: &JobId) -> Result<(), DriverError> {
        let location = self.location_of(job);
        let query: Vec<(&str, &str)> = location
            .iter()
            .map(|location| ("location", location.as_str()))
            .collect();
        self.api
            .post::<serde_json::Value>(
                &format!("{}/cancel", self.job_path(job)),
                &query,
                &json!({}),
            )
            .await
            .map(drop)
            .map_err(errors::connection)
    }
}
