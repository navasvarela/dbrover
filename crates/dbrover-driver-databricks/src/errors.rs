//! Databricks's errors as DbRover's, with the place in the statement a message names.

use dbrover_core::{DriverError, position_at};

use crate::api::ApiError;

pub(crate) fn connect(error: ApiError) -> DriverError {
    DriverError::Connect(Box::new(error))
}

pub(crate) fn connection(error: ApiError) -> DriverError {
    DriverError::Connection(Box::new(error))
}

/// A request about a statement that Databricks refused, pointing into the statement where its
/// message does; an error reaching Databricks is the connection's.
pub(crate) fn statement(error: ApiError, statement: &str) -> DriverError {
    match error {
        ApiError::Refused { ref message, .. } => DriverError::Statement {
            position: position_in(statement, message),
            source: Box::new(error),
            sqlstate: None,
        },
        other => connection(other),
    }
}

/// The byte of `statement` that `message` points at, from Spark's place in it: `line 1 pos 7`
/// after an analysis error, `(line 1, pos 7)` after a syntax error; the line counted from 1,
/// the position from 0, in characters. A message may name the place in an inner statement (a
/// view's text); DbRover can only point into the one it sent, so a place past its end points
/// nowhere.
pub(crate) fn position_in(statement: &str, message: &str) -> Option<usize> {
    let (_, place) = message.rsplit_once("line ")?;
    let line: String = place.chars().take_while(char::is_ascii_digit).collect();
    let rest = place[line.len()..].trim_start_matches([',', ' ']);
    let rest = rest.strip_prefix("pos ")?;
    let position: String = rest.chars().take_while(char::is_ascii_digit).collect();
    position_at(
        statement,
        line.parse().ok()?,
        position.parse::<usize>().ok()? + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_point_at_their_line_and_position() {
        let statement = "SELECT idd\nFROM orders WHERE";
        for (message, position) in [
            (
                "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column, variable, or function parameter \
                 with name `idd` cannot be resolved. Did you mean one of the following? [`id`]. \
                 SQLSTATE: 42703; line 1 pos 7;",
                Some(7),
            ),
            (
                "[PARSE_SYNTAX_ERROR] Syntax error at or near end of input. SQLSTATE: 42601 \
                 (line 2, pos 17)\n\n== SQL ==\nSELECT idd\nFROM orders WHERE",
                Some(28),
            ),
            (
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `orders` cannot be found. \
                 SQLSTATE: 42P01",
                None,
            ),
        ] {
            assert_eq!(position_in(statement, message), position, "{message}");
        }
    }
}
