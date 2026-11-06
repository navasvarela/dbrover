//! Running a statement: `POST /v1/statement`, then a `GET` of each answer's `nextUri` until
//! there is none. A query ends only once its result has been read, so it isn't polled to its
//! end and paged after, as C0's jobs are: the cursor asks for the next answer when the console
//! asks for rows, and an abandoned one is cancelled with a `DELETE` of its `nextUri`, as the
//! protocol asks.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, ColumnInfo, DriverError, JobProgress, ProgressHandle, ResultCursor, Row,
};
use hyper::{Method, StatusCode};

use crate::errors::{self, Refused};
use crate::http::{Answer, Client};
use crate::protocol::{QueryError, QueryResults, Session};

/// How many times a `503 Service Unavailable` is asked again, which the protocol says a
/// coordinator answers when it is busy, and how long to wait first. A busy coordinator is
/// asked for up to about five seconds before the statement fails.
const BUSY_RETRIES: u32 = 50;
const BUSY_PAUSE: Duration = Duration::from_millis(100);

/// A connection's statement machinery: its client and session, and the query it runs.
#[derive(Clone)]
pub(crate) struct Runner {
    pub client: Client,
    pub session: Arc<Mutex<Session>>,
    pub running: Arc<Running>,
}

/// What a statement ended in, when it couldn't be read.
pub(crate) enum Outcome {
    /// The coordinator answered with an error: the statement's.
    Failed(QueryError),
    /// The coordinator couldn't be reached, or answered with something else.
    Unreachable(DriverError),
}

impl Runner {
    pub(crate) fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts `sql` and reads until its columns are known, or to its end for a statement
    /// without a result.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] with Trino's message and its position when the statement
    /// fails, and [`DriverError::Connection`] when the coordinator can't be reached.
    pub(crate) async fn execute(&self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let cursor = self
            .start(sql)
            .await
            .map_err(|outcome| outcome.into_error(sql))?;
        Ok(Box::new(cursor))
    }

    /// [`Runner::execute`], keeping Trino's error as it is, for callers that read its name
    /// (the statement check) or want it whole.
    pub(crate) async fn start(&self, sql: &str) -> Result<TrinoCursor, Outcome> {
        let headers = self.session().request_headers(self.client.headers);
        let answer = self
            .send_retrying(Method::POST, "/v1/statement", Some(sql), &headers)
            .await
            .map_err(Outcome::Unreachable)?;
        let mut cursor = TrinoCursor {
            runner: self.clone(),
            sql: sql.to_owned(),
            columns: None,
            buffered: VecDeque::new(),
            next_uri: None,
            update_type: None,
            update_count: None,
            progress: None,
        };
        cursor.take_answer(&answer)?;
        while cursor.columns.is_none() && cursor.next_uri.is_some() {
            cursor.fetch().await?;
        }
        // A statement without a result (`INSERT`, `CREATE TABLE`, `USE`) answers with a column
        // or two of its own, `rows` for an `INSERT`, and its count: it is read to its end, and
        // shown as what it changed.
        if cursor.update_type.is_some() {
            while cursor.next_uri.is_some() {
                cursor.fetch().await?;
            }
            cursor.columns = Some(Vec::new());
            cursor.buffered.clear();
        }
        Ok(cursor)
    }

    /// Runs `sql` to its end and returns its rows: the explorer's catalog queries.
    ///
    /// # Errors
    ///
    /// As [`Runner::execute`], as [`DriverError::Connection`]: the explorer's queries are
    /// DbRover's own.
    pub(crate) async fn rows(&self, sql: &str) -> Result<Vec<Row>, DriverError> {
        let mut cursor = self.start(sql).await.map_err(|outcome| match outcome {
            Outcome::Failed(error) => DriverError::Connection(Box::new(Refused::from(error))),
            Outcome::Unreachable(error) => error,
        })?;
        let mut rows = Vec::new();
        while !cursor.is_exhausted() {
            rows.extend(cursor.next_page(usize::MAX).await?);
        }
        Ok(rows)
    }

    /// Sends a request, asking again while the coordinator says it is busy.
    async fn send_retrying(
        &self,
        method: Method,
        uri: &str,
        body: Option<&str>,
        headers: &[(String, String)],
    ) -> Result<Answer, DriverError> {
        let mut tries = 0;
        loop {
            let answer = self
                .client
                .send(method.clone(), uri, body, headers)
                .await
                .map_err(errors::connection)?;
            if answer.status != StatusCode::SERVICE_UNAVAILABLE || tries >= BUSY_RETRIES {
                return Ok(answer);
            }
            tries += 1;
            tokio::time::sleep(BUSY_PAUSE).await;
        }
    }
}

impl Outcome {
    pub(crate) fn into_error(self, sql: &str) -> DriverError {
        match self {
            Self::Failed(error) => errors::statement(error, sql),
            Self::Unreachable(error) => error,
        }
    }
}

/// A statement's answers, read as the console asks for rows.
pub(crate) struct TrinoCursor {
    runner: Runner,
    sql: String,
    /// `None` until an answer names them; empty for a statement without a result.
    columns: Option<Vec<ColumnInfo>>,
    buffered: VecDeque<Row>,
    next_uri: Option<String>,
    update_type: Option<String>,
    update_count: Option<u64>,
    progress: Option<JobProgress>,
}

impl TrinoCursor {
    /// Asks for the next answer.
    async fn fetch(&mut self) -> Result<(), Outcome> {
        let Some(uri) = self.next_uri.clone() else {
            return Ok(());
        };
        let answer = self
            .runner
            .send_retrying(Method::GET, &uri, None, &[])
            .await
            .map_err(Outcome::Unreachable)?;
        self.take_answer(&answer)
    }

    /// Takes in one answer: the session's changes, the columns, rows, progress and where to
    /// go next, or the statement's error.
    fn take_answer(&mut self, answer: &Answer) -> Result<(), Outcome> {
        let headers = self.runner.client.headers;
        self.runner.session().apply(&answer.headers, headers);
        if answer.status != StatusCode::OK {
            self.end();
            return Err(Outcome::Unreachable(errors::status(answer)));
        }
        let results: QueryResults = serde_json::from_slice(&answer.body)
            .map_err(|error| Outcome::Unreachable(errors::connection(error)))?;
        if let Some(error) = results.error {
            self.end();
            return Err(Outcome::Failed(error));
        }
        if self.columns.is_none() {
            self.columns = results.columns();
        }
        self.buffered.extend(
            results
                .rows()
                .map_err(|error| Outcome::Unreachable(errors::connection(error)))?,
        );
        self.progress = results.progress().or(self.progress);
        self.update_type = results.update_type.clone().or(self.update_type.take());
        self.update_count = results.update_count.or(self.update_count);
        self.next_uri = results.next_uri.clone();
        match &self.next_uri {
            Some(next) => self.runner.running.set(next, self.progress),
            None => self.end(),
        }
        Ok(())
    }

    fn end(&mut self) {
        self.next_uri = None;
        self.runner.running.clear();
    }
}

#[async_trait]
impl ResultCursor for TrinoCursor {
    fn columns(&self) -> &[ColumnInfo] {
        self.columns.as_deref().unwrap_or_default()
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        while self.buffered.len() < max_rows && self.next_uri.is_some() {
            self.fetch()
                .await
                .map_err(|outcome| outcome.into_error(&self.sql))?;
        }
        let taken = max_rows.min(self.buffered.len());
        Ok(self.buffered.drain(..taken).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.buffered.is_empty() && self.next_uri.is_none()
    }

    fn progress(&self) -> Option<JobProgress> {
        self.progress
    }

    /// Stops a query whose answers weren't all read, as the protocol asks of a client that
    /// leaves one.
    async fn finish(mut self: Box<Self>) -> Result<Option<u64>, DriverError> {
        if let Some(uri) = self.next_uri.take() {
            self.runner.running.clear();
            // Only a courtesy to the coordinator, which ends an abandoned query by itself
            // after a few minutes: a failure here changes nothing for the user.
            let _ = self
                .runner
                .client
                .send(Method::DELETE, &uri, None, &[])
                .await;
        }
        Ok(self.update_count)
    }
}

/// The query a connection is running, for cancelling it and asking how far it got from other
/// tasks.
pub(crate) struct Running {
    client: Client,
    state: Mutex<Option<RunningQuery>>,
}

struct RunningQuery {
    next_uri: String,
    progress: Option<JobProgress>,
}

impl Running {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            state: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, Option<RunningQuery>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, next_uri: &str, progress: Option<JobProgress>) {
        *self.state() = Some(RunningQuery {
            next_uri: next_uri.to_owned(),
            progress,
        });
    }

    fn clear(&self) {
        *self.state() = None;
    }
}

#[async_trait]
impl CancelHandle for Running {
    /// Cancels with a `DELETE` of the query's latest `nextUri`; its next answer is then the
    /// error that it was cancelled.
    async fn cancel(&self) -> Result<(), DriverError> {
        let uri = self.state().as_ref().map(|query| query.next_uri.clone());
        let Some(uri) = uri else {
            return Ok(());
        };
        let answer = self
            .client
            .send(Method::DELETE, &uri, None, &[])
            .await
            .map_err(errors::connection)?;
        // A query already over answers 404 or 410: nothing left to stop.
        if answer.status.is_success()
            || matches!(answer.status, StatusCode::NOT_FOUND | StatusCode::GONE)
        {
            Ok(())
        } else {
            Err(errors::status(&answer))
        }
    }
}

#[async_trait]
impl ProgressHandle for Running {
    async fn progress(&self) -> Result<Option<JobProgress>, DriverError> {
        Ok(self.state().as_ref().and_then(|query| query.progress))
    }
}
