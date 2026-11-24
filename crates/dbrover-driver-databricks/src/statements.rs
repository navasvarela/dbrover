//! Databricks's statements as C0's jobs: a submission that answers at once (`wait_timeout`
//! `0s`), its status by its id, its result's chunks through their external links, and its
//! cancel.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dbrover_core::{
    DriverError, JobFailure, JobId, JobPage, JobProgress, JobService, JobState, PageToken, Row,
    TimeLimit,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Api, ApiError};
use crate::context::{Context, Use};
use crate::errors;
use crate::results::{self, Chunk, ExternalLink, Manifest};

/// The Statement Execution API's path below the workspace's address.
pub(crate) const STATEMENTS: &str = "/api/2.0/sql/statements";

/// The query tag every statement DbRover sends carries, so an administrator finds DbRover's
/// in the query history.
const QUERY_TAG: &str = "dbrover";

/// A statement DbRover started and hasn't read to its end.
struct Started {
    /// Kept until it ends, to point into it from a failure's message.
    statement: String,
    /// When DbRover submitted it, which the time limit counts from.
    submitted: Instant,
    /// Whether DbRover cancelled it at the data source's time limit, so its end says so.
    stopped_at_limit: bool,
    /// Its result, once a poll found it done: the columns and chunk 0, which came with it.
    result: Option<Finished>,
}

struct Finished {
    manifest: Manifest,
    first: Chunk,
}

/// One connection's statements, on one API client, with the context it sends them with.
pub(crate) struct DatabricksJobs {
    api: Api,
    warehouse: String,
    /// The data source's time limit, which DbRover keeps by cancelling at the poll past it:
    /// the API has no timeout of its own to send (C6f).
    limit: Option<TimeLimit>,
    context: Mutex<Context>,
    started: Mutex<HashMap<JobId, Started>>,
}

/// The parts of a statement's answer DbRover reads.
#[derive(Debug, Deserialize)]
struct Statement {
    statement_id: Option<String>,
    #[serde(default)]
    status: Status,
    #[serde(default)]
    manifest: Option<Manifest>,
    #[serde(default)]
    result: Option<Chunk>,
}

#[derive(Debug, Default, Deserialize)]
struct Status {
    #[serde(default)]
    state: State,
    #[serde(default)]
    error: Option<StatusError>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum State {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Closed,
}

#[derive(Debug, Deserialize)]
struct StatusError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
}

impl DatabricksJobs {
    pub(crate) fn new(
        api: Api,
        warehouse: String,
        context: Context,
        limit: Option<TimeLimit>,
    ) -> Self {
        Self {
            api,
            warehouse,
            limit,
            context: Mutex::new(context),
            started: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn api(&self) -> &Api {
        &self.api
    }

    /// Changes what this connection's later statements are sent with.
    pub(crate) fn apply(&self, change: &Use) {
        self.context
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply(change);
    }

    fn started(&self) -> MutexGuard<'_, HashMap<JobId, Started>> {
        self.started.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The body of a submission: the statement, the warehouse, the connection's context,
    /// DbRover's tag, an answer at once, and the rows through external links as JSON.
    ///
    /// External links, not `INLINE`: an inline result fails past 25 MiB, and a statement
    /// can't be run again to fetch its rows another way, since it may have written
    /// (`docs/cloud-data.md`, C6, "Rows as text in the console, through external links").
    fn body(&self, statement: &str) -> Value {
        let mut body = self
            .context
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fields();
        body.insert("statement".to_owned(), Value::from(statement));
        body.insert(
            "warehouse_id".to_owned(),
            Value::from(self.warehouse.as_str()),
        );
        body.insert("wait_timeout".to_owned(), Value::from("0s"));
        body.insert("disposition".to_owned(), Value::from("EXTERNAL_LINKS"));
        body.insert("format".to_owned(), Value::from("JSON_ARRAY"));
        body.insert("query_tags".to_owned(), json!([{ "key": QUERY_TAG }]));
        Value::Object(body)
    }

    /// Whether `job` has run past the data source's time limit, marking it so when it has.
    fn past_limit(&self, job: &JobId) -> bool {
        let Some(limit) = self.limit else {
            return false;
        };
        let mut started = self.started();
        let Some(started) = started.get_mut(job) else {
            return false;
        };
        let past = started.submitted.elapsed() > Duration::from_secs(u64::from(limit.seconds()));
        started.stopped_at_limit |= past;
        past
    }

    fn stopped_at_limit(&self, job: &JobId) -> bool {
        self.started()
            .get(job)
            .is_some_and(|started| started.stopped_at_limit)
    }

    /// What a statement DbRover stopped at the data source's time limit says.
    fn timed_out(&self) -> String {
        let limit = self
            .limit
            .map_or_else(|| "its time limit".to_owned(), |limit| limit.to_string());
        format!(
            "DbRover stopped the statement after {limit}, this data source's limit, and \
             warehouse {} was billed until then. Raise Stop statements after in the data \
             source's settings to let it run longer.",
            self.warehouse
        )
    }

    /// Why a statement failed, pointing into it where Databricks's message does.
    fn failure(&self, job: &JobId, status: &Status) -> JobFailure {
        let statement = self.started().remove(job).map(|started| started.statement);
        let said = status
            .error
            .as_ref()
            .and_then(|error| error.message.clone().or_else(|| error.error_code.clone()));
        let message = said.unwrap_or_else(|| {
            match status.state {
                State::Canceled => "The statement was cancelled.",
                State::Closed => "The statement's result was closed before DbRover read it.",
                _ => "The statement failed, and Databricks didn't say why.",
            }
            .to_owned()
        });
        JobFailure {
            position: statement
                .as_deref()
                .and_then(|statement| errors::position_in(statement, &message)),
            message,
        }
    }

    /// Chunk `index`'s rows, through its links: chunk 0's as the statement's answer gave it
    /// (`known`), any other asked for. A link the storage refuses is renewed once by asking
    /// for its chunk again: a link lasts 15 minutes at most, and a console may page a result
    /// later than that.
    async fn rows_of(
        &self,
        job: &JobId,
        index: u64,
        known: Option<Chunk>,
    ) -> Result<(Vec<Row>, Option<u64>), DriverError> {
        let chunk = match known {
            Some(chunk) => chunk,
            None => self.chunk(job, index).await?,
        };
        if let Some(rows) = self.chunk_rows(&chunk).await? {
            return Ok((rows, chunk.next_chunk_index));
        }
        let renewed = self.chunk(job, index).await?;
        let rows = self.chunk_rows(&renewed).await?.ok_or_else(|| {
            errors::connection(ApiError::Unreadable(format!(
                "chunk {index}'s link was refused, and refused again once renewed"
            )))
        })?;
        Ok((rows, renewed.next_chunk_index))
    }

    /// The rows of `chunk`'s links and of its own `data_array`; `None` when the storage
    /// refused a link.
    async fn chunk_rows(&self, chunk: &Chunk) -> Result<Option<Vec<Row>>, DriverError> {
        let mut rows = Vec::new();
        for link in &chunk.external_links {
            match self.download(link).await? {
                Downloaded::Rows(data) => rows.extend(results::rows(data)),
                Downloaded::Refused => return Ok(None),
            }
        }
        if let Some(data) = &chunk.data_array {
            rows.extend(results::rows(data.clone()));
        }
        Ok(Some(rows))
    }

    async fn chunk(&self, job: &JobId, index: u64) -> Result<Chunk, DriverError> {
        let body = self
            .api
            .get(&format!("{}/result/chunks/{index}", statement_path(job)))
            .await
            .map_err(errors::connection)?;
        serde_json::from_value(body)
            .map_err(|error| errors::connection(ApiError::Unreadable(error.to_string())))
    }

    /// A link's rows, or [`Downloaded::Refused`] when the storage refuses it, as it does once
    /// the link has expired; the status that says so differs between clouds.
    async fn download(&self, link: &ExternalLink) -> Result<Downloaded, DriverError> {
        let answer = self
            .api
            .download(&link.external_link, &link.headers())
            .await
            .map_err(errors::connection)?;
        match answer.status {
            200 => serde_json::from_slice(&answer.body)
                .map(Downloaded::Rows)
                .map_err(|error| {
                    errors::connection(ApiError::Unreadable(format!("a chunk's rows: {error}")))
                }),
            400 | 403 | 404 | 410 => Ok(Downloaded::Refused),
            status => Err(errors::connection(ApiError::Unreadable(format!(
                "a chunk's download answered HTTP {status}"
            )))),
        }
    }
}

/// What a link's download gave.
enum Downloaded {
    Rows(Vec<Vec<Value>>),
    /// The storage refused the link, which a fresh one may not be.
    Refused,
}

fn statement_path(job: &JobId) -> String {
    format!("{STATEMENTS}/{}", job.as_str())
}

fn read_statement(body: Value) -> Result<Statement, DriverError> {
    serde_json::from_value(body)
        .map_err(|error| errors::connection(ApiError::Unreadable(error.to_string())))
}

#[async_trait]
impl JobService for DatabricksJobs {
    async fn submit(&self, statement: &str) -> Result<JobId, DriverError> {
        let reply = self
            .api
            .post(STATEMENTS, &self.body(statement))
            .await
            .map_err(|error| errors::statement(error, statement))?;
        if reply.status != 200 {
            return Err(errors::statement(reply.refusal(), statement));
        }
        let answer = read_statement(reply.body)?;
        let Some(id) = answer.statement_id else {
            return Err(errors::connection(ApiError::Unreadable(
                "the submission's answer has no statement_id".to_owned(),
            )));
        };
        let job = JobId::new(id);
        self.started().insert(
            job.clone(),
            Started {
                statement: statement.to_owned(),
                submitted: Instant::now(),
                stopped_at_limit: false,
                result: None,
            },
        );
        Ok(job)
    }

    async fn poll(&self, job: &JobId) -> Result<JobState, DriverError> {
        let body = self
            .api
            .get(&statement_path(job))
            .await
            .map_err(errors::connection)?;
        let answer = read_statement(body)?;
        match answer.status.state {
            State::Pending | State::Running => {
                if self.past_limit(job) {
                    // Sent again at each poll until Databricks says it stopped: its cancel
                    // "might silently fail".
                    self.cancel(job).await?;
                }
                Ok(JobState::Running(JobProgress::default()))
            }
            State::Canceled | State::Failed | State::Closed if self.stopped_at_limit(job) => {
                self.started().remove(job);
                Ok(JobState::Failed(JobFailure::new(self.timed_out())))
            }
            State::Succeeded => {
                if let Some(started) = self.started().get_mut(job) {
                    started.statement = String::new();
                    started.result = Some(Finished {
                        manifest: answer.manifest.unwrap_or_default(),
                        first: answer.result.unwrap_or_default(),
                    });
                }
                Ok(JobState::Done {
                    progress: JobProgress::default(),
                    rows_changed: None,
                })
            }
            State::Failed | State::Canceled | State::Closed => {
                Ok(JobState::Failed(self.failure(job, &answer.status)))
            }
        }
    }

    async fn page(
        &self,
        job: &JobId,
        after: Option<&PageToken>,
        _max_rows: usize,
    ) -> Result<JobPage, DriverError> {
        let (manifest, index, known) = {
            let mut started = self.started();
            let finished = started
                .get_mut(job)
                .and_then(|started| started.result.as_mut())
                .ok_or_else(|| {
                    errors::connection(ApiError::Unreadable(
                        "a statement's result was asked for before it finished".to_owned(),
                    ))
                })?;
            match after {
                None => (
                    finished.manifest.clone(),
                    0,
                    Some(std::mem::take(&mut finished.first)),
                ),
                Some(after) => {
                    let index = after.as_str().parse().map_err(|_| {
                        errors::connection(ApiError::Unreadable(format!(
                            "no chunk {}",
                            after.as_str()
                        )))
                    })?;
                    (finished.manifest.clone(), index, None)
                }
            }
        };
        let (rows, next) = self.rows_of(job, index, known).await?;
        if next.is_none() {
            // What is left of a statement once its last chunk is read is nothing at all.
            self.started().remove(job);
        }
        Ok(JobPage {
            columns: manifest
                .schema
                .columns
                .iter()
                .map(results::Column::info)
                .collect(),
            rows,
            next: next.map(|next| PageToken::new(next.to_string())),
            rows_changed: None,
        })
    }

    async fn cancel(&self, job: &JobId) -> Result<(), DriverError> {
        self.api
            .post(&format!("{}/cancel", statement_path(job)), &json!({}))
            .await
            .map(drop)
            .map_err(errors::connection)
    }
}
