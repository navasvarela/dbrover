//! Running user statements over the simple query protocol.
//!
//! The simple protocol returns every value as text, formatted by the server exactly as `psql`
//! shows it, so no type needs decoding here. Rows stream from the socket as the cursor reads
//! them: tokio-postgres stops reading the socket while nobody consumes the stream, and TCP
//! backpressure then pauses the server.

use std::pin::Pin;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, ColumnInfo, DriverError, Rejection, RejectionKind, ResultCursor, Row,
    TransactionStatus,
};
use futures_util::StreamExt;
use tokio_postgres::error::{ErrorPosition, SqlState};
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage, SimpleQueryStream};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::connection_error;

pub(crate) async fn execute(
    client: &Client,
    sql: &str,
) -> Result<Box<dyn ResultCursor>, DriverError> {
    let to_error = |error| statement_error(sql, error);
    let stream = client.simple_query_raw(sql).await.map_err(to_error)?;
    let mut cursor = PostgresCursor {
        stream: Box::pin(stream),
        columns: Vec::new(),
        rows_reported: None,
        exhausted: false,
        sql: sql.to_owned(),
    };
    cursor.read_header().await?;
    Ok(Box::new(cursor))
}

/// Asks the server whether a transaction block is open.
///
/// The protocol reports this after every statement (`ReadyForQuery`), but tokio-postgres
/// doesn't expose it, so this asks with a query instead. Inside a transaction block `now()`
/// is when the block started, earlier than this statement; outside one, every statement is
/// its own transaction and the two are equal. In a failed block the query itself is refused
/// with `in_failed_sql_transaction`.
///
/// The query counts as the transaction's first statement, so in a `REPEATABLE READ` block
/// opened just before, it is what takes the snapshot, a moment earlier than the user's next
/// statement would have.
pub(crate) async fn transaction_status(client: &Client) -> Result<TransactionStatus, DriverError> {
    let messages = match client
        .simple_query("SELECT now() <> statement_timestamp()")
        .await
    {
        Ok(messages) => messages,
        Err(error) if error.code() == Some(&SqlState::IN_FAILED_SQL_TRANSACTION) => {
            return Ok(TransactionStatus::Failed);
        }
        Err(error) => return Err(connection_error(error)),
    };
    let in_block = messages.iter().find_map(|message| match message {
        SimpleQueryMessage::Row(row) => row.get(0),
        _ => None,
    });
    Ok(match in_block {
        Some("t") => TransactionStatus::InTransaction,
        _ => TransactionStatus::Idle,
    })
}

/// What a statement is checked with: `PREPARE` parses and analyses it but runs nothing, and
/// `DEALLOCATE`, in the same message, forgets it.
const CHECK_PREFIX: &str = "PREPARE dbrover_check AS ";
const CHECK_SUFFIX: &str = "\n;\nDEALLOCATE dbrover_check";

/// Checks `sql` with `PREPARE` and `DEALLOCATE` in one simple-protocol message (C1c).
///
/// Why not the protocol's own Parse (`Client::prepare`): it leaves a named statement on the
/// server until a later Close, and behind a pooler in transaction mode that Close may reach
/// another server connection. One message runs as one implicit transaction, which a pooler
/// keeps on one server connection, and a `PREPARE` that fails rolls the message back, so
/// nothing is left either way. Only data queries and changes are checked
/// (`dbrover_sql::is_checkable`), which are what `PREPARE` takes.
pub(crate) async fn validate(client: &Client, sql: &str) -> Result<Option<Rejection>, DriverError> {
    let check = format!("{CHECK_PREFIX}{sql}{CHECK_SUFFIX}");
    let error = match client.simple_query(&check).await {
        Ok(_) => return Ok(None),
        Err(error) => error,
    };
    let Some(db_error) = error.as_db_error() else {
        return Err(connection_error(error));
    };
    let code = db_error.code();
    let kind = if *code == SqlState::SYNTAX_ERROR {
        RejectionKind::Syntax
    } else if [
        SqlState::UNDEFINED_TABLE,
        SqlState::UNDEFINED_COLUMN,
        SqlState::UNDEFINED_FUNCTION,
        SqlState::UNDEFINED_OBJECT,
        SqlState::INVALID_SCHEMA_NAME,
    ]
    .contains(code)
    {
        RejectionKind::UnknownName
    } else {
        RejectionKind::Other
    };
    Ok(Some(Rejection {
        message: db_error.message().to_owned(),
        position: check_position(sql, &error),
        kind,
    }))
}

/// Maps a server error to [`DriverError::Statement`], with its position, unless it means
/// the connection is gone.
fn statement_error(sql: &str, error: tokio_postgres::Error) -> DriverError {
    if crate::is_lost(&error) {
        return DriverError::Lost(Box::new(error));
    }
    let position = error_position(sql, &error);
    let sqlstate = error.code().map(|code| code.code().to_owned());
    DriverError::Statement {
        source: Box::new(error),
        position,
        sqlstate,
    }
}

/// Where in `sql` an error in its check (`CHECK_PREFIX`, `sql`, `CHECK_SUFFIX`) is: the
/// server counts from the start of the prefix. `None` for a position outside `sql`.
fn check_position(sql: &str, error: &tokio_postgres::Error) -> Option<usize> {
    let ErrorPosition::Original(characters) = error.as_db_error()?.position()? else {
        return None;
    };
    let index = usize::try_from(*characters)
        .ok()?
        .checked_sub(1)?
        .checked_sub(CHECK_PREFIX.chars().count())?;
    sql.char_indices().nth(index).map(|(byte, _)| byte)
}

/// Converts PostgreSQL's 1-based character position into a byte offset into `sql`.
fn error_position(sql: &str, error: &tokio_postgres::Error) -> Option<usize> {
    error
        .as_db_error()
        .and_then(|db_error| db_error.position())
        .and_then(|position| match position {
            ErrorPosition::Original(characters) => {
                let index = usize::try_from(*characters).ok()?.checked_sub(1)?;
                sql.char_indices().nth(index).map(|(byte, _)| byte)
            }
            ErrorPosition::Internal { .. } => None,
        })
}

struct PostgresCursor {
    stream: Pin<Box<SimpleQueryStream>>,
    columns: Vec<ColumnInfo>,
    /// The server's row count from `CommandComplete`, once the statement has completed.
    rows_reported: Option<u64>,
    exhausted: bool,
    sql: String,
}

impl PostgresCursor {
    async fn next_message(&mut self) -> Result<Option<SimpleQueryMessage>, DriverError> {
        match self.stream.next().await {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(error)) => Err(statement_error(&self.sql, error)),
            None => Ok(None),
        }
    }

    /// Reads until the column list or the completion of a statement without rows.
    async fn read_header(&mut self) -> Result<(), DriverError> {
        loop {
            match self.next_message().await? {
                Some(SimpleQueryMessage::RowDescription(columns)) => {
                    self.columns = columns
                        .iter()
                        .map(|column| ColumnInfo {
                            name: column.name().to_owned(),
                            type_name: None,
                        })
                        .collect();
                    return Ok(());
                }
                Some(SimpleQueryMessage::CommandComplete(rows)) => {
                    self.complete(rows);
                    return Ok(());
                }
                // Rows can't precede their description; skip anything else.
                Some(_) => {}
                None => {
                    // An empty query (only comments) completes with no messages at all.
                    self.exhausted = true;
                    return Ok(());
                }
            }
        }
    }

    fn complete(&mut self, rows: u64) {
        self.rows_reported = Some(rows);
        self.exhausted = true;
    }
}

#[async_trait]
impl ResultCursor for PostgresCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let mut page = Vec::new();
        while !self.exhausted && page.len() < max_rows {
            match self.next_message().await? {
                Some(SimpleQueryMessage::Row(row)) => {
                    page.push(
                        (0..row.len())
                            .map(|i| row.get(i).map(str::to_owned))
                            .collect(),
                    );
                }
                Some(SimpleQueryMessage::CommandComplete(rows)) => self.complete(rows),
                // A second result set means the text held more than one statement, which the
                // console's splitter is meant to prevent. Show the first; `finish` drains
                // the rest.
                Some(_) | None => self.exhausted = true,
            }
        }
        Ok(page)
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Reads and discards whatever the server still sends, so the connection is ready for
    /// the next statement. After a cancel, that is the "canceling statement" error.
    async fn finish(mut self: Box<Self>) -> Result<Option<u64>, DriverError> {
        while let Some(message) = self.next_message().await? {
            if let SimpleQueryMessage::CommandComplete(rows) = message {
                self.rows_reported.get_or_insert(rows);
            }
        }
        Ok(self.rows_reported)
    }
}

/// Cancels through a separate connection to the server, as `psql`'s Ctrl+C does.
pub(crate) struct PostgresCancel {
    pub(crate) token: CancelToken,
    /// The session's own connector: the cancel request is a new connection, which a server
    /// that requires TLS refuses in plain text.
    pub(crate) tls: MakeRustlsConnect,
}

#[async_trait]
impl CancelHandle for PostgresCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.token
            .cancel_query(self.tls.clone())
            .await
            .map_err(connection_error)
    }
}
