//! Athena's and the SDK's errors as DbRover's: the service's code and message when it answered
//! (`InvalidRequestException: …`), the SDK's whole chain of causes when it couldn't be
//! reached, and a failed query's reason with the place in the statement it names.

use std::error::Error;
use std::fmt;

use aws_sdk_athena::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_athena::types::QueryExecutionStatus;
use dbrover_core::{DriverError, position_at};

/// What Athena or the SDK said, as one line.
#[derive(Debug)]
pub(crate) struct Refused(pub(crate) String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Refused {}

impl<E, R> From<SdkError<E, R>> for Refused
where
    E: ProvideErrorMetadata + Error + 'static,
    R: fmt::Debug,
{
    fn from(error: SdkError<E, R>) -> Self {
        let message = match error.as_service_error() {
            Some(service) => match (service.code(), service.message()) {
                (Some(code), Some(message)) => format!("{code}: {message}"),
                (Some(code), None) => code.to_owned(),
                _ => DisplayErrorContext(&error).to_string(),
            },
            None => DisplayErrorContext(&error).to_string(),
        };
        Self(message)
    }
}

/// A statement Athena refused to start, pointing into it where the message does.
pub(crate) fn statement(refused: Refused, statement: &str) -> DriverError {
    DriverError::Statement {
        position: position_in(statement, &refused.0),
        source: Box::new(refused),
        sqlstate: None,
    }
}

pub(crate) fn connect(error: impl Error + Send + Sync + 'static) -> DriverError {
    DriverError::Connect(Box::new(error))
}

pub(crate) fn connection(error: impl Error + Send + Sync + 'static) -> DriverError {
    DriverError::Connection(Box::new(error))
}

/// Why a query failed or was cancelled: the reason Athena gives with its state, else the
/// message of its error.
pub(crate) fn failure_message(status: &QueryExecutionStatus) -> String {
    status
        .state_change_reason()
        .or_else(|| {
            status
                .athena_error()
                .and_then(|error| error.error_message())
        })
        .unwrap_or("Athena gave no reason")
        .to_owned()
}

/// The byte of `statement` that `message` points at, when it names a place as Trino's errors
/// do: `line 1:8: mismatched input …`, possibly after a prefix of Athena's
/// (`COLUMN_NOT_FOUND: line 1:8: …`, `FAILED: SemanticException [Error 10001]: line 1:14: …`).
pub(crate) fn position_in(statement: &str, message: &str) -> Option<usize> {
    let (line, column) = line_and_column(message)?;
    position_at(statement, line, column)
}

/// The first `line L:C:` in `message`.
fn line_and_column(message: &str) -> Option<(usize, usize)> {
    message.match_indices("line ").find_map(|(at, word)| {
        let rest = &message[at + word.len()..];
        let (line, rest) = rest.split_once(':')?;
        let (column, _) = rest.split_once(':')?;
        Some((line.parse().ok()?, column.parse().ok()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_place_athenas_messages_name() {
        let statement = "SELECT *\nFORM orders";
        let cases = [
            ("line 2:1: mismatched input 'FORM'", Some(9)),
            (
                "COLUMN_NOT_FOUND: line 1:8: Column 'x' cannot be resolved",
                Some(7),
            ),
            (
                "FAILED: SemanticException [Error 10001]: line 1:14: Table not found",
                None,
            ),
            ("Query exhausted resources at this scale factor", None),
            ("the online line 3:x: text", None),
        ];
        for (message, expected) in cases {
            assert_eq!(position_in(statement, message), expected, "{message}");
        }
    }
}
