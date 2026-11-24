//! Snowflake's statements as C0's jobs: a submission with `async=true`, its status by its
//! handle, its result's partitions, and its cancel.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use dbrover_core::{
    DriverError, Handoff, HandoffFiles, JobFailure, JobId, JobPage, JobProgress, JobService,
    JobState, PageToken, TimeLimit,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Api, ApiError, Reply, STATEMENTS};
use crate::context::{Context, Use};
use crate::errors;
use crate::results::{self, Metadata};

/// What every statement DbRover sends is tagged with, so an administrator finds DbRover's in
/// `QUERY_HISTORY`.
const QUERY_TAG: &str = "DbRover";

/// A statement DbRover started and hasn't read to its end.
struct Started {
    /// Kept until it ends, to point into it from a failure's message.
    statement: String,
    /// Its result set, once a poll found it done: the columns, the partitions there are, and
    /// partition 0's rows, which came with it.
    result: Option<Finished>,
}

struct Finished {
    metadata: Metadata,
    first: Vec<Vec<Value>>,
}

/// One connection's statements, on one API client, with the context it sends them with.
pub(crate) struct SnowflakeJobs {
    api: Api,
    context: Mutex<Context>,
    /// The data source's time limit, sent as each statement's `timeout` (C5f).
    limit: Option<TimeLimit>,
    started: Mutex<HashMap<JobId, Started>>,
}

/// The parts of a result set DbRover reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResultSet {
    #[serde(default)]
    result_set_meta_data: Metadata,
    #[serde(default)]
    data: Vec<Vec<Value>>,
    #[serde(default)]
    stats: Option<Stats>,
}

/// What a DML statement changed.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stats {
    #[serde(default)]
    num_rows_inserted: u64,
    #[serde(default)]
    num_rows_updated: u64,
    #[serde(default)]
    num_rows_deleted: u64,
}

impl Stats {
    fn rows_changed(&self) -> u64 {
        self.num_rows_inserted + self.num_rows_updated + self.num_rows_deleted
    }
}

impl SnowflakeJobs {
    pub(crate) fn new(api: Api, context: Context, limit: Option<TimeLimit>) -> Self {
        Self {
            api,
            context: Mutex::new(context),
            limit,
            started: Mutex::new(HashMap::new()),
        }
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

    /// The body of a submission: the statement, the connection's context and DbRover's tag.
    fn body(&self, statement: &str) -> Value {
        let mut body = self
            .context
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fields();
        body.insert("statement".to_owned(), Value::from(statement));
        // Without a limit of DbRover's, none is sent: `0` would ask for Snowflake's longest,
        // seven days, rather than its own default.
        if let Some(limit) = self.limit {
            body.insert("timeout".to_owned(), Value::from(limit.seconds()));
        }
        body.insert("parameters".to_owned(), json!({ "query_tag": QUERY_TAG }));
        Value::Object(body)
    }

    /// Posts `statement` with its own `requestId`, sending it again with `retry=true` while
    /// Snowflake asks for that, or once after a dropped connection: Snowflake doesn't run a
    /// statement twice for one `requestId` it has run.
    async fn send(&self, statement: &str) -> Result<Reply, ApiError> {
        let request_id = uuid::Uuid::new_v4();
        let body = self.body(statement);
        let mut attempt = 0;
        loop {
            let retry = if attempt == 0 { "" } else { "&retry=true" };
            let path = format!("{STATEMENTS}?requestId={request_id}&async=true{retry}");
            match self.api.post(&path, &body).await {
                Ok(reply) => match Api::retry_pause(&reply, attempt) {
                    Some(pause) => tokio::time::sleep(pause).await,
                    None => return Ok(reply),
                },
                Err(ApiError::Http(_)) if attempt == 0 => {}
                Err(error) => return Err(error),
            }
            attempt += 1;
        }
    }

    /// Why a statement failed, pointing into it where Snowflake's message does, with what a
    /// statement stopped at its timeout (HTTP 408) means in DbRover.
    fn failure(&self, job: &JobId, reply: &Reply) -> JobFailure {
        let mut message = reply.refusal().to_string();
        if reply.status == TIMED_OUT {
            message = self.timed_out(&message);
        }
        let statement = self.started().remove(job).map(|started| started.statement);
        JobFailure {
            position: statement
                .as_deref()
                .and_then(|statement| errors::position_in(statement, &message)),
            message,
        }
    }
}

/// The status Snowflake answers a statement it cancelled at its timeout with.
const TIMED_OUT: u16 = 408;

impl SnowflakeJobs {
    /// Snowflake's `message` for a statement it stopped at its timeout, saying which limit that
    /// was and what it cost.
    fn timed_out(&self, message: &str) -> String {
        match self.limit {
            Some(limit) => format!(
                "{message} Snowflake stopped it after {limit}, this data source's limit, and \
                 billed its warehouse until then. Raise Stop statements after in the data \
                 source's settings to let it run longer."
            ),
            None => format!(
                "{message} Snowflake stopped it at its own statement timeout \
                 (STATEMENT_TIMEOUT_IN_SECONDS, set on the account, the user or the warehouse)."
            ),
        }
    }
}

/// A finished result set's answer, as a [`ResultSet`].
fn result_set(reply: &Reply) -> Result<ResultSet, DriverError> {
    serde_json::from_value(reply.body.clone())
        .map_err(|error| errors::connection(ApiError::Unreadable(error.to_string())))
}

fn handle_path(job: &JobId) -> String {
    format!("{STATEMENTS}/{}", job.as_str())
}

#[async_trait]
impl JobService for SnowflakeJobs {
    async fn submit(&self, statement: &str) -> Result<JobId, DriverError> {
        let reply = self
            .send(statement)
            .await
            .map_err(|error| errors::statement(error, statement))?;
        let handle = match reply.status {
            200 | 202 => reply.body["statementHandle"].as_str(),
            _ => None,
        };
        let Some(handle) = handle else {
            return Err(errors::statement(reply.refusal(), statement));
        };
        let job = JobId::new(handle);
        self.started().insert(
            job.clone(),
            Started {
                statement: statement.to_owned(),
                result: None,
            },
        );
        Ok(job)
    }

    async fn poll(&self, job: &JobId) -> Result<JobState, DriverError> {
        let reply = self
            .api
            .get(&handle_path(job))
            .await
            .map_err(errors::connection)?;
        match reply.status {
            // A statement still running is 202, or 429 when Snowflake is busy answering.
            202 | 429 => Ok(JobState::Running(JobProgress::default())),
            200 => {
                let result = result_set(&reply)?;
                let rows_changed = result.stats.as_ref().map(Stats::rows_changed);
                if let Some(started) = self.started().get_mut(job) {
                    started.statement = String::new();
                    started.result = Some(Finished {
                        metadata: result.result_set_meta_data,
                        first: result.data,
                    });
                }
                Ok(JobState::Done {
                    progress: JobProgress::default(),
                    rows_changed,
                })
            }
            // 422: the statement failed; 408: it ran past its timeout and Snowflake
            // cancelled it.
            408 | 422 => Ok(JobState::Failed(self.failure(job, &reply))),
            _ => Err(errors::connection(reply.refusal())),
        }
    }

    async fn page(
        &self,
        job: &JobId,
        after: Option<&PageToken>,
        _max_rows: usize,
    ) -> Result<JobPage, DriverError> {
        let (metadata, partition, data) = match after {
            None => {
                let mut started = self.started();
                let finished = started
                    .get_mut(job)
                    .and_then(|started| started.result.as_mut())
                    .ok_or_else(|| {
                        errors::connection(ApiError::Unreadable(
                            "a statement's result was asked for before it finished".to_owned(),
                        ))
                    })?;
                let data = std::mem::take(&mut finished.first);
                (finished.metadata.clone(), 0, data)
            }
            Some(after) => {
                let partition: usize = after.as_str().parse().map_err(|_| {
                    errors::connection(ApiError::Unreadable(format!(
                        "no partition {}",
                        after.as_str()
                    )))
                })?;
                let metadata = self
                    .started()
                    .get(job)
                    .and_then(|started| started.result.as_ref())
                    .map(|finished| finished.metadata.clone())
                    .unwrap_or_default();
                let reply = self
                    .api
                    .get_retrying(&format!("{}?partition={partition}", handle_path(job)))
                    .await
                    .map_err(errors::connection)?;
                if reply.status != 200 {
                    return Err(errors::connection(reply.refusal()));
                }
                (metadata, partition, result_set(&reply)?.data)
            }
        };
        let next = partition + 1;
        let more = next < metadata.partition_info.len();
        if !more {
            // What is left of a statement once its last page is read is nothing at all.
            self.started().remove(job);
        }
        Ok(JobPage {
            columns: metadata
                .row_type
                .iter()
                .map(results::Column::info)
                .collect(),
            rows: results::rows(&metadata.row_type, &data),
            next: more.then(|| PageToken::new(next.to_string())),
            rows_changed: None,
        })
    }

    /// Where a finished `COPY INTO` wrote its files: the folder the statement named, which the
    /// run lists (C5h). Snowflake names the files itself.
    async fn result_files(
        &self,
        job: &JobId,
        handoff: &Handoff,
    ) -> Result<Option<HandoffFiles>, DriverError> {
        let _ = job;
        Ok(crate::handoff::files(handoff))
    }

    async fn cancel(&self, job: &JobId) -> Result<(), DriverError> {
        self.api
            .post(&format!("{}/cancel", handle_path(job)), &json!({}))
            .await
            .map(drop)
            .map_err(errors::connection)
    }
}
