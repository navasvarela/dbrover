//! The client's errors as DbRover's: a statement the cluster refused, with where in its text
//! when the cluster says; a connection that failed; a run that ⌘. stopped.

use dbrover_core::{BoxError, DriverError};
use scylla::errors::{ExecutionError, RequestAttemptError};

/// The request was stopped by ⌘. before its answer came.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub(crate) struct Cancelled;

pub(crate) fn connect(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

pub(crate) fn connection(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connection(error.into())
}

/// A failed statement, at the position the cluster names in `sql` for a syntax error; or, when
/// no node could be reached, a failed connection.
pub(crate) fn statement(error: ExecutionError, sql: &str) -> DriverError {
    if unreachable(&error) {
        return connection(error);
    }
    let position = refusal(&error).and_then(|message| syntax_position(message, sql));
    DriverError::Statement {
        source: error.into(),
        position,
        sqlstate: None,
    }
}

/// The cluster's message, when it refused the statement: a syntax error, an unknown name, a
/// server timeout or a missing permission.
pub(crate) fn refusal(error: &ExecutionError) -> Option<&str> {
    match error {
        ExecutionError::LastAttemptError(RequestAttemptError::DbError(_, message)) => Some(message),
        _ => None,
    }
}

/// Whether no node could be asked: the statement itself may be fine.
fn unreachable(error: &ExecutionError) -> bool {
    matches!(
        error,
        ExecutionError::EmptyPlan
            | ExecutionError::ConnectionPoolError(_)
            | ExecutionError::LastAttemptError(RequestAttemptError::BrokenConnectionError(_))
    )
}

/// Where a syntax error is in `sql`: both servers write `line 1:23` (1-based line, 0-based
/// column, in characters) before what they couldn't read.
pub(crate) fn syntax_position(message: &str, sql: &str) -> Option<usize> {
    let (_, rest) = message.split_once("line ")?;
    let (line, rest) = rest.split_once(':')?;
    let column: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let line: usize = line.parse().ok()?;
    let column: usize = column.parse().ok()?;
    let line_start = match line.checked_sub(1)? {
        0 => 0,
        before => sql.match_indices('\n').nth(before - 1)?.0 + 1,
    };
    let rest = &sql[line_start..];
    let within = rest
        .char_indices()
        .nth(column)
        .map_or(rest.len(), |(offset, _)| offset);
    Some(line_start + within)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_syntax_errors_line_and_column() {
        let sql = "SELECT id\nFROM t WHERE id = = 1";
        let message = "line 2:18 no viable alternative at input '='";
        assert_eq!(
            syntax_position(message, sql),
            Some(sql.rfind('=').unwrap_or(0))
        );
        assert_eq!(
            syntax_position("line 1:3 mismatched input", "SEL x"),
            Some(3)
        );
        assert_eq!(syntax_position("Undefined column name nope", sql), None);
    }
}
