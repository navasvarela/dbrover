//! BigQuery's errors as DbRover's, with the place in the statement a message names.

use dbrover_core::{DriverError, position_at};

use crate::api::ApiError;

/// A request about a statement that BigQuery refused, pointing into the statement where its
/// message does; an error reaching BigQuery is the connection's.
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

pub(crate) fn connect(error: ApiError) -> DriverError {
    DriverError::Connect(Box::new(error))
}

pub(crate) fn connection(error: ApiError) -> DriverError {
    DriverError::Connection(Box::new(error))
}

/// The byte of `statement` that `message` points at, from the last `[line:column]` in it, as
/// BigQuery ends its messages (`Syntax error: Unexpected end of statement at [2:23]`), or
/// `[at line:column]`, as the emulator does.
pub(crate) fn position_in(statement: &str, message: &str) -> Option<usize> {
    let (_, place) = message.rsplit_once('[')?;
    let place = place.split(']').next()?;
    let place = place.strip_prefix("at ").unwrap_or(place);
    let (line, column) = place.split_once(':')?;
    position_at(
        statement,
        line.trim().parse().ok()?,
        column.trim().parse().ok()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_point_at_their_line_and_column() {
        let statement = "SELECT id\nFROM shop.orders WHERE";
        for (message, position) in [
            (
                "Syntax error: Unexpected end of statement at [2:23]",
                Some(32),
            ),
            (
                "failed to parse statement: Syntax error: Unexpected end of statement [at 2:23]",
                Some(32),
            ),
            ("Unrecognized name: idd at [1:8]", Some(7)),
            (
                "Not found: Table p:shop.nope was not found in location EU",
                None,
            ),
            ("Array [1, 2] isn't a place", None),
        ] {
            assert_eq!(position_in(statement, message), position, "{message}");
        }
    }
}
