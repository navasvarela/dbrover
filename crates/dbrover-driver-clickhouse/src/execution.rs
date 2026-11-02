//! Running user statements, reading their results, and cancelling them.
//!
//! Each statement is one request, answered in `JSONCompactEachRowWithNamesAndTypes`: a line of
//! column names, a line of types, then one JSON array per row, read only as the cursor asks
//! for pages. A statement without a result (`CREATE`, `INSERT`) answers with nothing at all.
//! Values are kept as the raw JSON ClickHouse wrote, so a 64-bit integer or a decimal keeps
//! every digit; strings are unquoted.
//!
//! The session runs one query at a time, so a cursor finished before its last row kills its
//! query (dropping the response alone leaves it running a while), or the next statement would
//! find the session locked.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use clickhouse::Client;
use clickhouse::query::{BytesCursor, Query};
use dbrover_core::{
    BoxError, ByteCount, CancelHandle, Cell, ColumnInfo, DriverError, JobProgress, ProgressHandle,
    Rejection, ResultCursor, Row,
};
use serde_json::value::RawValue;
use tokio::io::{AsyncBufReadExt, Lines};
use uuid::Uuid;

use crate::connection_error;
use crate::errors::{cancelled, ends_the_session, rejection, statement_error};

/// The output format of user statements: names, then types, then one JSON array per row.
const FORMAT: &str = "JSONCompactEachRowWithNamesAndTypes";

/// Put before a statement to have it parsed without running it.
const PARSE: &str = "EXPLAIN AST ";

/// Put before a query to have its names resolved without running it.
const PLAN: &str = "EXPLAIN PLAN ";

/// How `EXPLAIN AST` begins for a query, with `UNION`, `INTERSECT` or `WITH` or without.
const QUERY_ROOT: &str = "SelectWithUnionQuery";

/// A request that failed: ClickHouse's own error message, or the connection's.
pub(crate) enum Failure {
    Server(String),
    Connection(BoxError),
}

impl Failure {
    fn from_client(error: clickhouse::error::Error) -> Self {
        match error {
            clickhouse::error::Error::BadResponse(message) => Self::Server(message),
            other => Self::Connection(Box::new(other)),
        }
    }

    /// A read partway through a response fails with an I/O error carrying the client's.
    fn from_read(error: io::Error) -> Self {
        match error
            .into_inner()
            .map(|inner| inner.downcast::<clickhouse::error::Error>())
        {
            Some(Ok(client)) => Self::from_client(*client),
            Some(Err(other)) => Self::Connection(other),
            None => Self::Connection("the response ended unexpectedly".into()),
        }
    }

    fn statement_error(self, sql: &str) -> DriverError {
        if self.is_lost() {
            return DriverError::Lost(Box::new(self));
        }
        match self {
            Self::Server(message) => statement_error(&message, sql, 0),
            Self::Connection(error) => DriverError::Connection(error),
        }
    }

    /// Whether the session is over: the server can't be reached, or no longer knows the
    /// session, as after it restarts. A connection is a server session (`session_id`) over
    /// requests that each have their own HTTP connection, so a request failing at the
    /// network level means the server is gone, not only one socket.
    pub(crate) fn is_lost(&self) -> bool {
        match self {
            Self::Server(message) => ends_the_session(message),
            Self::Connection(error) => error
                .downcast_ref::<clickhouse::error::Error>()
                .is_some_and(|error| matches!(error, clickhouse::error::Error::Network(_))),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(message) => f.write_str(message),
            Self::Connection(error) => error.fmt(f),
        }
    }
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for Failure {}

/// Runs `query` and returns all it wrote, as text.
///
/// Not with `AsyncReadExt::read_to_string`: the cursor's `AsyncRead` can fill part of the
/// buffer and then return `Pending`, which tokio's readers reject with a panic.
async fn read_all(query: Query, format: &str) -> Result<String, Failure> {
    let mut cursor = query.fetch_bytes(format).map_err(Failure::from_client)?;
    let bytes = cursor.collect().await.map_err(Failure::from_client)?;
    String::from_utf8(bytes.to_vec()).map_err(|error| Failure::Connection(Box::new(error)))
}

/// The one value `sql` returns, as text.
pub(crate) async fn single_value(client: &Client, sql: &str) -> Result<String, Failure> {
    let text = read_all(client.query_raw(sql), "TabSeparatedRaw").await?;
    Ok(text.trim_end_matches('\n').to_owned())
}

/// The rows of a catalog query, `NULL` as `None`, with `{name:String}` parameters bound to
/// `params`.
pub(crate) async fn rows(
    client: &Client,
    sql: &str,
    params: &[(&str, &str)],
) -> Result<Vec<Row>, DriverError> {
    let query = params
        .iter()
        .fold(client.query_raw(sql), |query, (name, value)| {
            query.param(name, *value)
        });
    let text = read_all(query, "JSONCompactEachRow")
        .await
        .map_err(connection_error)?;
    text.lines()
        .map(|line| cells(line).map_err(connection_error))
        .collect()
}

/// One row: a JSON array, each value as ClickHouse wrote it but strings unquoted.
fn cells(line: &str) -> Result<Row, serde_json::Error> {
    let values: Vec<&RawValue> = serde_json::from_str(line)?;
    values.into_iter().map(cell).collect()
}

fn cell(value: &RawValue) -> Result<Cell, serde_json::Error> {
    let text = value.get();
    Ok(match text {
        "null" => None,
        quoted if quoted.starts_with('"') => Some(serde_json::from_str(quoted)?),
        other => Some(other.to_owned()),
    })
}

/// Stops a statement by its `query_id`, with `KILL QUERY` on a client without the session,
/// which the statement keeps busy, and says what it has done so far from
/// `system.processes`, found by the same id (C0f). Killing one's own queries, and seeing
/// them, needs no grant.
pub(crate) struct ClickHouseCancel {
    client: Client,
    running: Mutex<Option<String>>,
}

impl ClickHouseCancel {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            running: Mutex::new(None),
        }
    }

    fn start(&self) -> String {
        let query_id = Uuid::new_v4().to_string();
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = Some(query_id.clone());
        query_id
    }

    fn end(&self, query_id: &str) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.as_deref() == Some(query_id) {
            *running = None;
        }
    }

    /// Waits until the query has stopped (`SYNC`), so its session is free again.
    async fn kill(&self, query_id: &str) -> Result<(), Failure> {
        let query = self
            .client
            .query_raw("KILL QUERY WHERE query_id = {query_id:String} SYNC")
            .param("query_id", query_id);
        read_all(query, "TabSeparatedRaw").await.map(drop)
    }
}

/// The columns of `system.processes` that say what a query has read so far.
const PROGRESS_SQL: &str = "SELECT read_rows, read_bytes FROM system.processes \
                            WHERE query_id = {query_id:String}";

#[async_trait]
impl ProgressHandle for ClickHouseCancel {
    async fn progress(&self) -> Result<Option<JobProgress>, DriverError> {
        let running = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(query_id) = running else {
            return Ok(None);
        };
        let query = self
            .client
            .query_raw(PROGRESS_SQL)
            .param("query_id", query_id.as_str());
        let text = read_all(query, "TabSeparatedRaw")
            .await
            .map_err(connection_error)?;
        Ok(read_progress(&text))
    }
}

/// `read_rows` and `read_bytes`, tab-separated, from `system.processes`; `None` once the query
/// has left it.
pub(crate) fn read_progress(text: &str) -> Option<JobProgress> {
    let (rows, bytes) = text.lines().next()?.split_once('\t')?;
    Some(JobProgress {
        rows_read: rows.parse().ok(),
        bytes_read: bytes.parse().ok().map(ByteCount),
    })
}

#[async_trait]
impl CancelHandle for ClickHouseCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let running = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match running {
            Some(query_id) => self.kill(&query_id).await.map_err(connection_error),
            None => Ok(()),
        }
    }
}

pub(crate) async fn execute(
    session: &Client,
    cancel: &Arc<ClickHouseCancel>,
    sql: &str,
) -> Result<Box<dyn ResultCursor>, DriverError> {
    let query_id = cancel.start();
    let (columns, lines, rows_changed) = match start(session, &query_id, sql).await {
        Ok(Started::Rows { columns, lines }) => (columns, Some(lines), None),
        Ok(Started::Done { rows_changed }) => {
            cancel.end(&query_id);
            (Vec::new(), None, rows_changed)
        }
        Err(failure) => {
            cancel.end(&query_id);
            return Err(failure.statement_error(sql));
        }
    };
    Ok(Box::new(ClickHouseCursor {
        columns,
        lines,
        rows_changed,
        query_id,
        cancel: Arc::clone(cancel),
        sql: sql.to_owned(),
    }))
}

/// How a statement's response begins.
enum Started {
    /// A result: its columns, then the rest of the response, a row to a line.
    Rows {
        columns: Vec<ColumnInfo>,
        lines: Lines<BytesCursor>,
    },
    /// No result, and how many rows the statement wrote, if any.
    Done { rows_changed: Option<u64> },
}

/// Sends the statement and reads its column names and types, or, for a statement without a
/// result, everything, and how many rows it wrote.
async fn start(session: &Client, query_id: &str, sql: &str) -> Result<Started, Failure> {
    let cursor = session
        .query_raw(sql)
        .with_setting("query_id", query_id)
        .fetch_bytes(FORMAT)
        .map_err(Failure::from_client)?;
    let mut lines = cursor.lines();
    let Some(names) = lines.next_line().await.map_err(Failure::from_read)? else {
        let rows_changed = lines
            .into_inner()
            .summary()
            .and_then(|summary| summary.written_rows())
            .filter(|rows| *rows > 0);
        return Ok(Started::Done { rows_changed });
    };
    let types = lines.next_line().await.map_err(Failure::from_read)?;
    let names: Vec<String> = header(Some(names))?;
    let types: Vec<String> = header(types)?;
    let columns = names
        .into_iter()
        .zip(types)
        .map(|(name, type_name)| ColumnInfo {
            name,
            type_name: Some(type_name),
        })
        .collect();
    Ok(Started::Rows { columns, lines })
}

/// One of the two header lines: a JSON array of column names or of their types.
fn header(line: Option<String>) -> Result<Vec<String>, Failure> {
    let line = line.ok_or_else(|| Failure::Connection("the result has no types line".into()))?;
    serde_json::from_str(&line).map_err(|error| Failure::Connection(Box::new(error)))
}

/// Checks a statement in two steps: ClickHouse parses it (any statement), then, if it is a
/// query, plans it, which resolves its names without reading anything. ClickHouse plans
/// nothing but queries, so other statements are checked for syntax only.
pub(crate) async fn validate(
    session: &Client,
    sql: &str,
) -> Result<Option<Rejection>, DriverError> {
    let tree = match explain(session, PARSE, sql).await? {
        Ok(tree) => tree,
        Err(rejection) => return Ok(Some(rejection)),
    };
    if !tree.starts_with(QUERY_ROOT) {
        return Ok(None);
    }
    Ok(explain(session, PLAN, sql).await?.err())
}

/// `EXPLAIN <how> <sql>`: what ClickHouse wrote, or what it found wrong with `sql`.
async fn explain(
    session: &Client,
    how: &str,
    sql: &str,
) -> Result<Result<String, Rejection>, DriverError> {
    match read_all(session.query_raw(&format!("{how}{sql}")), "TabSeparatedRaw").await {
        Ok(text) => Ok(Ok(text)),
        Err(Failure::Server(message)) => Ok(Err(rejection(&message, sql, how.len()))),
        Err(Failure::Connection(error)) => Err(DriverError::Connection(error)),
    }
}

struct ClickHouseCursor {
    columns: Vec<ColumnInfo>,
    /// `None` once the result has been read to its end.
    lines: Option<Lines<BytesCursor>>,
    rows_changed: Option<u64>,
    query_id: String,
    cancel: Arc<ClickHouseCancel>,
    sql: String,
}

impl ClickHouseCursor {
    fn ended(&mut self) {
        self.lines = None;
        self.cancel.end(&self.query_id);
    }
}

#[async_trait]
impl ResultCursor for ClickHouseCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let mut rows = Vec::new();
        while rows.len() < max_rows {
            let Some(lines) = self.lines.as_mut() else {
                break;
            };
            match lines.next_line().await {
                Ok(Some(line)) => match cells(&line) {
                    Ok(row) => rows.push(row),
                    Err(error) => {
                        self.ended();
                        return Err(DriverError::Connection(Box::new(error)));
                    }
                },
                Ok(None) => self.ended(),
                Err(error) => {
                    self.ended();
                    return Err(Failure::from_read(error).statement_error(&self.sql));
                }
            }
        }
        Ok(rows)
    }

    fn is_exhausted(&self) -> bool {
        self.lines.is_none()
    }

    /// Stops the query if rows were left unread, so its session is free for the next
    /// statement, and reports it as cancelled, as the other engines do.
    ///
    /// The response is dropped before the kill: a query blocked writing rows nobody reads
    /// never sees the kill, so `KILL QUERY … SYNC` would wait for it forever. Closing the
    /// response stops it (`cancel_http_readonly_queries_on_client_close`), and the kill then
    /// waits until it has, which a closed response alone doesn't.
    async fn finish(mut self: Box<Self>) -> Result<Option<u64>, DriverError> {
        if self.lines.is_none() {
            return Ok(self.rows_changed);
        }
        self.lines = None;
        let killed = self.cancel.kill(&self.query_id).await;
        self.ended();
        killed.map_err(connection_error)?;
        Err(cancelled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_values_as_clickhouse_writes_them_but_unquotes_strings() {
        let row =
            cells(r#"[1, "it's", null, 18446744073709551615, 12.50, [1,2], "{\"a\":1}"]"#).unwrap();
        let expected = [
            Some("1"),
            Some("it's"),
            None,
            Some("18446744073709551615"),
            Some("12.50"),
            Some("[1,2]"),
            Some(r#"{"a":1}"#),
        ];
        assert_eq!(row, expected.map(|value| value.map(str::to_owned)).to_vec());
    }
}
