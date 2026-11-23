//! Snowflake's errors as DbRover's, with the place in the statement a message names.

use dbrover_core::{DriverError, position_at};

use crate::api::ApiError;

pub(crate) fn connect(error: ApiError) -> DriverError {
    DriverError::Connect(Box::new(error))
}

pub(crate) fn connection(error: ApiError) -> DriverError {
    DriverError::Connection(Box::new(error))
}

/// A request about a statement that Snowflake refused, pointing into the statement where its
/// message does; an error reaching Snowflake is the connection's.
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

/// The byte of `statement` that `message` points at, from Snowflake's `error line 2 at
/// position 7`: the line counted from 1, the position from 0, in characters.
pub(crate) fn position_in(statement: &str, message: &str) -> Option<usize> {
    let (_, place) = message.split_once("error line ")?;
    let (line, rest) = place.split_once(" at position ")?;
    let position: String = rest.chars().take_while(char::is_ascii_digit).collect();
    position_at(
        statement,
        line.trim().parse().ok()?,
        position.parse::<usize>().ok()? + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_point_at_their_line_and_position() {
        let statement = "SELECT id\nFROM orders WHERE";
        for (message, position) in [
            (
                "SQL compilation error: error line 1 at position 7\ninvalid identifier 'ID'",
                Some(7),
            ),
            (
                "SQL compilation error:\nsyntax error line 2 at position 17 unexpected '<EOF>'.",
                Some(27),
            ),
            (
                "SQL compilation error:\nObject 'ORDERS' does not exist or not authorized.",
                None,
            ),
        ] {
            assert_eq!(position_in(statement, message), position, "{message}");
        }
    }
}
