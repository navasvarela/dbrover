//! Statements that run as jobs, as cloud warehouses run them: submitted, polled by the job's
//! id until done, their result paged through, and cancelled by the id (`docs/cloud-data.md`,
//! C0f). A driver implements the four calls of [`JobService`]; [`Jobs`] turns them into the
//! cursor, cancel handle and progress handle every engine gives the rest of DbRover, so no
//! warehouse driver writes its own polling.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;

use crate::{ByteCount, CancelHandle, ColumnInfo, DriverError, PageToken, ResultCursor, Row};

/// The first pause between two polls of a running job: short, since most statements a
/// console runs finish within a second.
const FIRST_POLL_PAUSE: Duration = Duration::from_millis(100);

/// The longest pause between two polls: a job running for minutes is asked every two
/// seconds, which keeps the progress line moving without a request a few milliseconds apart.
const LONGEST_POLL_PAUSE: Duration = Duration::from_secs(2);

/// How many rows the first page of a finished job's result asks for, to learn its columns:
/// the console's first page, so that page needs no second request.
const FIRST_PAGE_ROWS: usize = 500;

/// A job's id, as the service names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a running statement has done so far, as far as its engine reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub rows_read: Option<u64>,
    pub bytes_read: Option<ByteCount>,
}

impl fmt::Display for JobProgress {
    /// `1234 rows · 2.1 GB read`, or what of it is known.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = [
            self.rows_read.map(|rows| format!("{rows} rows")),
            self.bytes_read.map(|bytes| format!("{bytes} read")),
        ]
        .into_iter()
        .flatten()
        .collect();
        f.write_str(&parts.join(" · "))
    }
}

/// Where a job is, as one poll found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running(JobProgress),
    Done {
        progress: JobProgress,
        /// The rows a statement without a result changed, when the service says.
        rows_changed: Option<u64>,
    },
    /// The job failed, or was cancelled: the service's message.
    Failed(String),
}

/// One page of a finished job's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPage {
    /// Every page names the columns; only the first one's are used.
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Row>,
    /// Where the next page starts; `None` on the last.
    pub next: Option<PageToken>,
}

/// A service that runs statements as jobs: Athena, BigQuery, Snowflake and Databricks (C3 to
/// C6), each over its own API.
#[async_trait]
pub trait JobService: Send + Sync {
    /// Starts running `statement`, and returns at once with its job's id.
    async fn submit(&self, statement: &str) -> Result<JobId, DriverError>;

    /// Where the job is now.
    async fn poll(&self, job: &JobId) -> Result<JobState, DriverError>;

    /// A page of a finished job's result: the first with `after` empty, then each after the
    /// one before. A service may return fewer rows than `max_rows`, and more pages after.
    async fn page(
        &self,
        job: &JobId,
        after: Option<&PageToken>,
        max_rows: usize,
    ) -> Result<JobPage, DriverError>;

    /// Asks the service to stop the job. Best effort: the next poll says whether it did.
    async fn cancel(&self, job: &JobId) -> Result<(), DriverError>;
}

/// Asks a running statement what it has done so far, from another task while it runs: what
/// the console's progress line shows.
#[async_trait]
pub trait ProgressHandle: Send + Sync {
    /// The running statement's progress; `None` when nothing runs, or nothing is known yet.
    async fn progress(&self) -> Result<Option<JobProgress>, DriverError>;
}

/// The job a connection is running, if any, and its progress at the last poll.
struct Running {
    job: JobId,
    progress: JobProgress,
}

/// A connection's jobs on one [`JobService`]: it runs them as cursors, and is the connection's
/// cancel and progress handle, which act on the job it is running.
pub struct Jobs {
    service: Arc<dyn JobService>,
    running: Mutex<Option<Running>>,
}

impl Jobs {
    pub fn new(service: Arc<dyn JobService>) -> Arc<Self> {
        Arc::new(Self {
            service,
            running: Mutex::new(None),
        })
    }

    /// Submits `statement`, polls it with a growing pause until it ends, and returns a cursor
    /// over its result.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] with the service's message when the job fails or is
    /// cancelled, and the service's own errors.
    pub async fn execute(
        self: &Arc<Self>,
        statement: &str,
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        let job = self.service.submit(statement).await?;
        self.set_running(Some(Running {
            job: job.clone(),
            progress: JobProgress::default(),
        }));
        let ended = self.wait(&job).await;
        let rows_changed = match ended {
            Ok(rows_changed) => rows_changed,
            Err(error) => {
                self.set_running(None);
                return Err(error);
            }
        };
        let first = match self.service.page(&job, None, FIRST_PAGE_ROWS).await {
            Ok(page) => page,
            Err(error) => {
                self.set_running(None);
                return Err(error);
            }
        };
        Ok(Box::new(JobCursor {
            jobs: Arc::clone(self),
            columns: first.columns,
            buffered: first.rows,
            next: first.next,
            job,
            rows_changed,
        }))
    }

    /// Polls `job` until it ends: the rows it changed, or why it failed.
    async fn wait(&self, job: &JobId) -> Result<Option<u64>, DriverError> {
        let mut pause = FIRST_POLL_PAUSE;
        loop {
            match self.service.poll(job).await? {
                JobState::Running(progress) => self.record(job, progress),
                JobState::Done {
                    progress,
                    rows_changed,
                } => {
                    self.record(job, progress);
                    return Ok(rows_changed);
                }
                JobState::Failed(message) => {
                    return Err(DriverError::Statement {
                        source: message.into(),
                        position: None,
                        sqlstate: None,
                    });
                }
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(LONGEST_POLL_PAUSE);
        }
    }

    fn record(&self, job: &JobId, progress: JobProgress) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = running.as_mut().filter(|running| running.job == *job) {
            running.progress = progress;
        }
    }

    fn set_running(&self, running: Option<Running>) {
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = running;
    }

    fn running_job(&self) -> Option<JobId> {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        running.as_ref().map(|running| running.job.clone())
    }

    /// Ends `job` as the running one, if it still is.
    fn end(&self, job: &JobId) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.as_ref().is_some_and(|running| running.job == *job) {
            *running = None;
        }
    }
}

#[async_trait]
impl CancelHandle for Jobs {
    async fn cancel(&self) -> Result<(), DriverError> {
        match self.running_job() {
            Some(job) => self.service.cancel(&job).await,
            None => Ok(()),
        }
    }
}

#[async_trait]
impl ProgressHandle for Jobs {
    async fn progress(&self) -> Result<Option<JobProgress>, DriverError> {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(running.as_ref().map(|running| running.progress))
    }
}

/// A finished job's result, read a page of the service's at a time.
struct JobCursor {
    jobs: Arc<Jobs>,
    job: JobId,
    columns: Vec<ColumnInfo>,
    /// Rows of the service's last page not handed out yet.
    buffered: Vec<Row>,
    next: Option<PageToken>,
    rows_changed: Option<u64>,
}

#[async_trait]
impl ResultCursor for JobCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        while self.buffered.len() < max_rows {
            let Some(after) = self.next.take() else {
                break;
            };
            let wanted = max_rows - self.buffered.len();
            let page = self
                .jobs
                .service
                .page(&self.job, Some(&after), wanted)
                .await?;
            self.buffered.extend(page.rows);
            self.next = page.next;
        }
        let taken = max_rows.min(self.buffered.len());
        Ok(self.buffered.drain(..taken).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.buffered.is_empty() && self.next.is_none()
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        self.jobs.end(&self.job);
        Ok(self.rows_changed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A service in memory: each job runs for a set number of polls, then has its rows, in
    /// pages of `page_size`. It records every call.
    #[derive(Default)]
    struct Fake {
        polls_needed: usize,
        page_size: usize,
        rows: Vec<Row>,
        failure: Option<String>,
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        polls: HashMap<String, usize>,
        cancelled: Vec<String>,
        pages_asked: usize,
        submitted: usize,
    }

    impl Fake {
        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }
    }

    #[async_trait]
    impl JobService for Fake {
        async fn submit(&self, _statement: &str) -> Result<JobId, DriverError> {
            let mut state = self.state();
            state.submitted += 1;
            Ok(JobId::new(format!("job-{}", state.submitted)))
        }

        async fn poll(&self, job: &JobId) -> Result<JobState, DriverError> {
            let mut state = self.state();
            if state.cancelled.iter().any(|id| id == job.as_str()) {
                return Ok(JobState::Failed("Job cancelled".to_owned()));
            }
            let polls = state.polls.entry(job.as_str().to_owned()).or_default();
            *polls += 1;
            let progress = JobProgress {
                rows_read: Some(*polls as u64 * 10),
                bytes_read: Some(ByteCount(*polls as u64 * 1000)),
            };
            if *polls < self.polls_needed {
                return Ok(JobState::Running(progress));
            }
            Ok(match &self.failure {
                Some(message) => JobState::Failed(message.clone()),
                None => JobState::Done {
                    progress,
                    rows_changed: None,
                },
            })
        }

        async fn page(
            &self,
            _job: &JobId,
            after: Option<&PageToken>,
            _max_rows: usize,
        ) -> Result<JobPage, DriverError> {
            self.state().pages_asked += 1;
            let start: usize = after.map_or(0, |token| token.as_str().parse().unwrap());
            let end = (start + self.page_size).min(self.rows.len());
            Ok(JobPage {
                columns: vec![ColumnInfo {
                    name: "n".to_owned(),
                    type_name: Some("INT64".to_owned()),
                }],
                rows: self.rows[start..end].to_vec(),
                next: (end < self.rows.len()).then(|| PageToken::new(end.to_string())),
            })
        }

        async fn cancel(&self, job: &JobId) -> Result<(), DriverError> {
            self.state().cancelled.push(job.as_str().to_owned());
            Ok(())
        }
    }

    fn rows(count: usize) -> Vec<Row> {
        (0..count).map(|n| vec![Some(n.to_string())]).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_job_is_polled_with_a_growing_pause_then_paged_through() {
        let fake = Arc::new(Fake {
            polls_needed: 6,
            page_size: 4,
            rows: rows(10),
            ..Fake::default()
        });
        let jobs = Jobs::new(fake.clone());
        let started = tokio::time::Instant::now();
        let mut cursor = jobs.execute("SELECT n FROM numbers").await.unwrap();
        // Five pauses before the sixth poll: 0.1 + 0.2 + 0.4 + 0.8 + 1.6 s.
        assert_eq!(started.elapsed(), Duration::from_millis(3100));
        assert_eq!(cursor.columns()[0].name, "n");
        assert_eq!(
            jobs.progress().await.unwrap(),
            Some(JobProgress {
                rows_read: Some(60),
                bytes_read: Some(ByteCount(6000)),
            })
        );

        let mut read = Vec::new();
        for _ in 0..3 {
            read.extend(cursor.next_page(4).await.unwrap());
        }
        assert_eq!(read, rows(10));
        assert!(cursor.is_exhausted());
        // The first page when the job ended, then the two after it.
        assert_eq!(fake.state().pages_asked, 3);
        assert_eq!(cursor.finish().await.unwrap(), None);
        assert_eq!(jobs.progress().await.unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_pause_between_polls_stops_growing_at_two_seconds() {
        let fake = Arc::new(Fake {
            polls_needed: 9,
            ..Fake::default()
        });
        let started = tokio::time::Instant::now();
        Jobs::new(fake).execute("SELECT 1").await.unwrap();
        // 0.1 + 0.2 + 0.4 + 0.8 + 1.6, then 2 s three times.
        assert_eq!(started.elapsed(), Duration::from_millis(9100));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_job_says_why() {
        let fake = Arc::new(Fake {
            polls_needed: 2,
            failure: Some("Syntax error: Unexpected keyword FORM at [1:10]".to_owned()),
            ..Fake::default()
        });
        let jobs = Jobs::new(fake);
        let Err(error) = jobs.execute("SELECT * FORM t").await else {
            panic!("the job failed");
        };
        assert!(
            crate::describe_error(&error).contains("Unexpected keyword FORM"),
            "{error}"
        );
        assert_eq!(jobs.progress().await.unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_stops_the_running_job_by_its_id() {
        let fake = Arc::new(Fake {
            polls_needed: usize::MAX,
            ..Fake::default()
        });
        let jobs = Jobs::new(fake.clone());
        // Nothing runs yet: nothing to cancel.
        jobs.cancel().await.unwrap();
        assert!(fake.state().cancelled.is_empty());

        let running = tokio::spawn({
            let jobs = Arc::clone(&jobs);
            async move { jobs.execute("SELECT * FROM huge").await.map(drop) }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(jobs.progress().await.unwrap().is_some());
        jobs.cancel().await.unwrap();
        let error = running.await.unwrap().unwrap_err();
        assert!(crate::describe_error(&error).contains("Job cancelled"));
        assert_eq!(fake.state().cancelled, ["job-1"]);
    }

    #[test]
    fn progress_shows_what_is_known() {
        let both = JobProgress {
            rows_read: Some(1234),
            bytes_read: Some(ByteCount(2_100_000_000)),
        };
        assert_eq!(both.to_string(), "1234 rows · 2.1 GB read");
        let rows = JobProgress {
            rows_read: Some(5),
            bytes_read: None,
        };
        assert_eq!(rows.to_string(), "5 rows");
        assert_eq!(JobProgress::default().to_string(), "");
    }
}
