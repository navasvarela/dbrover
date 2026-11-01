//! Running statements: a page at a time with the protocol's own paging, and how ⌘. stops them.
//!
//! The protocol has no cancel. ⌘. stops waiting for the node's answer, so the console is free
//! at once; Cassandra finishes the statement or stops at its own timeout. On ScyllaDB, a
//! `SELECT` runs with `USING TIMEOUT` from the data source's statement timeout, when one is
//! set, so the server stops too.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, ColumnInfo, DriverError, Rejection, RejectionKind, ResultCursor, Row, ServerInfo,
};
use dbrover_sql::{Dialect, TokenKind, first_word, tokenize};
use scylla::client::session::Session;
use scylla::errors::{DbError, ExecutionError, PrepareError, RequestAttemptError};
use scylla::response::query_result::QueryResult;
use scylla::response::{PagingState, PagingStateResponse};
use scylla::statement::unprepared::Statement;
use scylla::value::Row as CqlRow;
use tokio::sync::watch;

use crate::errors::{self, Cancelled};
use crate::values;

/// What ScyllaDB calls itself in [`ServerInfo::product`].
pub(crate) const SCYLLADB: &str = "ScyllaDB";

/// Rows a request asks the node for: the grid's page.
const PAGE_SIZE: i32 = 500;

/// How much longer the client waits than a ScyllaDB statement's `USING TIMEOUT`, so the
/// server's own timeout error, which says what timed out, arrives first.
const CLIENT_GRACE: Duration = Duration::from_secs(2);

/// The run a connection is in, shared with its cursors and its cancel handle.
#[derive(Debug)]
pub(crate) struct Running {
    /// Bumped by each cancel; a request waiting for its answer gives up when it changes.
    stops: watch::Sender<u64>,
}

impl Default for Running {
    fn default() -> Self {
        Self {
            stops: watch::Sender::new(0),
        }
    }
}

impl Running {
    fn stop(&self) {
        self.stops.send_modify(|stops| *stops += 1);
    }

    /// Asks for one page of `statement`, giving up if the run is cancelled before the answer
    /// comes.
    async fn page(
        &self,
        session: &Session,
        statement: &Statement,
        paging: PagingState,
    ) -> Result<(QueryResult, PagingStateResponse), DriverError> {
        let mut stops = self.stops.subscribe();
        tokio::select! {
            answer = session.query_single_page(statement.clone(), &[], paging) => {
                answer.map_err(|error| errors::statement(error, statement.contents.as_str()))
            }
            _ = stops.changed() => Err(DriverError::Statement {
                source: Box::new(Cancelled),
                position: None,
                sqlstate: None,
            }),
        }
    }
}

/// Stops a connection's running statement from another task.
pub(crate) struct CassandraCancel(pub(crate) Arc<Running>);

#[async_trait]
impl CancelHandle for CassandraCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.0.stop();
        Ok(())
    }
}

/// How long a connection's statements may run, from the data source's statement timeout.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    statement: Option<Duration>,
    /// Whether the server takes `USING TIMEOUT`: ScyllaDB does, Cassandra doesn't.
    server_side: bool,
}

impl Timeouts {
    pub(crate) fn new(seconds: Option<NonZeroU32>, server: &ServerInfo) -> Self {
        Self {
            statement: seconds.map(|seconds| Duration::from_secs(u64::from(seconds.get()))),
            server_side: server.product == SCYLLADB,
        }
    }

    /// `sql` as it is sent, and how long the client waits for each page: `None` leaves the
    /// client's default.
    fn apply(self, sql: &str) -> (String, Option<Duration>) {
        let Some(timeout) = self.statement else {
            return (sql.to_owned(), None);
        };
        if self.server_side && is_select_without_timeout(sql) {
            let text = format!("{sql} USING TIMEOUT {}s", timeout.as_secs());
            return (text, Some(timeout + CLIENT_GRACE));
        }
        (sql.to_owned(), Some(timeout))
    }
}

/// Whether `sql` is a `SELECT` that doesn't set its own `USING TIMEOUT`, which ScyllaDB takes
/// at its end.
fn is_select_without_timeout(sql: &str) -> bool {
    if first_word(sql, Dialect::Cassandra).as_deref() != Some("select") {
        return false;
    }
    let words: Vec<String> = tokenize(sql, Dialect::Cassandra)
        .into_iter()
        .filter(|token| token.kind == TokenKind::Word)
        .map(|token| sql[token.range].to_ascii_lowercase())
        .collect();
    !words
        .windows(2)
        .any(|pair| pair[0] == "using" && pair[1] == "timeout")
}

/// Runs `sql` and reads its first page.
pub(crate) async fn execute(
    session: &Arc<Session>,
    running: &Arc<Running>,
    timeouts: &Timeouts,
    sql: &str,
) -> Result<Box<dyn ResultCursor>, DriverError> {
    let (text, timeout) = timeouts.apply(sql);
    let mut statement = Statement::new(text).with_page_size(PAGE_SIZE);
    statement.set_request_timeout(timeout);
    let mut cursor = CqlCursor {
        session: Arc::clone(session),
        running: Arc::clone(running),
        statement,
        columns: Vec::new(),
        rows: VecDeque::new(),
        next: Some(PagingState::start()),
    };
    cursor.fetch().await?;
    Ok(Box::new(cursor))
}

/// The rows of one statement, a page of the node's at a time.
struct CqlCursor {
    session: Arc<Session>,
    running: Arc<Running>,
    statement: Statement,
    columns: Vec<ColumnInfo>,
    /// Rows read from the node and not yet handed out.
    rows: VecDeque<Row>,
    /// Where the next page starts, or `None` after the last.
    next: Option<PagingState>,
}

impl CqlCursor {
    /// Reads the next page into `rows`.
    async fn fetch(&mut self) -> Result<(), DriverError> {
        let Some(paging) = self.next.take() else {
            return Ok(());
        };
        let (result, paging) = self
            .running
            .page(&self.session, &self.statement, paging)
            .await?;
        self.next = match paging {
            PagingStateResponse::HasMorePages { state } => Some(state),
            _ => None,
        };
        let Ok(rows) = result.into_rows_result() else {
            // `INSERT`, `USE`, `CREATE` and the like answer with no rows.
            return Ok(());
        };
        if self.columns.is_empty() {
            self.columns = rows
                .column_specs()
                .iter()
                .map(|spec| ColumnInfo {
                    name: spec.name().to_owned(),
                    type_name: Some(values::type_name(spec.typ())),
                })
                .collect();
        }
        let page = rows
            .rows::<CqlRow>()
            .map_err(|error| statement_error(error, None))?;
        for row in page {
            let row = row.map_err(|error| statement_error(error, None))?;
            self.rows.push_back(
                row.columns
                    .iter()
                    .map(|value| value.as_ref().map(values::text))
                    .collect(),
            );
        }
        Ok(())
    }
}

fn statement_error(
    error: impl Into<dbrover_core::BoxError>,
    position: Option<usize>,
) -> DriverError {
    DriverError::Statement {
        source: error.into(),
        position,
        sqlstate: None,
    }
}

#[async_trait]
impl ResultCursor for CqlCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    /// Reads pages until `max_rows` are in hand or the rows end. A page may come back empty
    /// with more to follow, as a filtering scan's do.
    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        while self.rows.len() < max_rows && self.next.is_some() {
            self.fetch().await?;
        }
        let count = max_rows.min(self.rows.len());
        Ok(self.rows.drain(..count).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.rows.is_empty() && self.next.is_none()
    }

    /// Nothing to end on the server: the pages not asked for are never read. CQL doesn't say
    /// how many rows a change touched.
    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}

/// The first column of the first row of `cql`, as text.
pub(crate) async fn single_text(
    session: &Session,
    cql: &str,
) -> Result<Option<String>, Box<ExecutionError>> {
    let rows = session
        .query_unpaged(cql, &[])
        .await
        .map_err(Box::new)?
        .into_rows_result();
    let Ok(rows) = rows else {
        return Ok(None);
    };
    let row = rows.maybe_first_row::<CqlRow>().ok().flatten();
    Ok(row
        .and_then(|row| row.columns.into_iter().next().flatten())
        .map(|value| values::text(&value)))
}

/// Prepares `sql`, which has the node check it and every name in it without running it.
pub(crate) async fn validate(
    session: &Session,
    sql: &str,
) -> Result<Option<Rejection>, DriverError> {
    match session.prepare(sql).await {
        Ok(_) => Ok(None),
        Err(PrepareError::AllAttemptsFailed {
            first_attempt: RequestAttemptError::DbError(error, message),
        }) => Ok(Some(rejection(&error, message, sql))),
        Err(error) => Err(errors::connection(error)),
    }
}

/// What the node said of a statement it wouldn't prepare, and where.
fn rejection(error: &DbError, message: String, sql: &str) -> Rejection {
    let unknown = unknown_name(&message);
    let kind = match (error, unknown) {
        (DbError::SyntaxError, _) => RejectionKind::Syntax,
        (_, Some(_)) => RejectionKind::UnknownName,
        _ => RejectionKind::Other,
    };
    let position = errors::syntax_position(&message, sql)
        .or_else(|| unknown.and_then(|name| word_position(sql, name)));
    Rejection {
        message,
        position,
        kind,
    }
}

/// The name a refusal says doesn't exist: `Undefined column name nope` (Cassandra), or
/// `Unrecognized name nope` (ScyllaDB, preparing), `unconfigured table orders`, `Keyspace shop
/// does not exist`.
fn unknown_name(message: &str) -> Option<&str> {
    let after = |prefix: &str| {
        let start = message.find(prefix)? + prefix.len();
        message[start..].split_whitespace().next()
    };
    after("Undefined column name ")
        .or_else(|| after("Unrecognized name "))
        .or_else(|| after("unconfigured table "))
        .or_else(|| {
            message
                .strip_prefix("Keyspace ")
                .and_then(|rest| rest.split_whitespace().next())
                .filter(|_| message.ends_with("does not exist"))
        })
}

/// Where `name`, written unquoted or quoted, is first used in `sql`; the last part of a
/// qualified one.
fn word_position(sql: &str, name: &str) -> Option<usize> {
    let name = name.rsplit('.').next().unwrap_or(name);
    tokenize(sql, Dialect::Cassandra)
        .into_iter()
        .find_map(|token| {
            let text = &sql[token.range.clone()];
            let matches = match token.kind {
                TokenKind::Word => text.eq_ignore_ascii_case(name),
                TokenKind::QuotedIdentifier => text.trim_matches('"') == name,
                _ => false,
            };
            matches.then_some(token.range.start)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeouts(seconds: u32, product: &str) -> Timeouts {
        Timeouts::new(
            NonZeroU32::new(seconds),
            &ServerInfo {
                product: product.to_owned(),
                version: "1".to_owned(),
            },
        )
    }

    #[test]
    fn asks_scylladb_to_stop_a_select_at_the_statement_timeout() {
        let scylla = timeouts(30, SCYLLADB);
        assert_eq!(
            scylla.apply("SELECT * FROM t"),
            (
                "SELECT * FROM t USING TIMEOUT 30s".to_owned(),
                Some(Duration::from_secs(32))
            )
        );
        assert_eq!(
            scylla.apply("SELECT * FROM t USING TIMEOUT 5s").0,
            "SELECT * FROM t USING TIMEOUT 5s"
        );
        assert_eq!(
            scylla.apply("INSERT INTO t (id) VALUES (1)"),
            (
                "INSERT INTO t (id) VALUES (1)".to_owned(),
                Some(Duration::from_secs(30))
            )
        );
        assert_eq!(
            timeouts(30, "Apache Cassandra").apply("SELECT * FROM t"),
            ("SELECT * FROM t".to_owned(), Some(Duration::from_secs(30)))
        );
        assert_eq!(
            timeouts(0, SCYLLADB).apply("SELECT * FROM t"),
            ("SELECT * FROM t".to_owned(), None)
        );
    }

    #[test]
    fn points_at_the_name_a_refusal_says_is_unknown() {
        let sql = "SELECT id, nope FROM shop.orders";
        let cases = [
            (
                "Undefined column name nope in table shop.orders",
                Some("nope"),
            ),
            ("Undefined column name nope", Some("nope")),
            ("Unrecognized name nope", Some("nope")),
            ("unconfigured table orders", Some("orders")),
            ("Keyspace shop does not exist", Some("shop")),
            ("Invalid STRING constant", None),
        ];
        for (message, expected) in cases {
            assert_eq!(unknown_name(message), expected, "{message}");
        }
        assert_eq!(word_position(sql, "nope"), sql.find("nope"));
        assert_eq!(word_position(sql, "shop.orders"), sql.find("orders"));
    }
}
