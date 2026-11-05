//! Trino's errors as DbRover's: a statement's error with its name and the place in the
//! statement it names, and the coordinator's other answers with their status.

use std::error::Error;
use std::fmt;

use dbrover_core::{DriverError, RejectionKind, position_at};
use hyper::StatusCode;

use crate::http::Answer;
use crate::protocol::QueryError;

/// What the coordinator said, as one line.
#[derive(Debug)]
pub(crate) struct Refused(pub(crate) String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Refused {}

/// `TABLE_NOT_FOUND: line 1:15: Table 'memory.default.t' does not exist`, as Trino's own
/// command-line client shows an error.
impl From<QueryError> for Refused {
    fn from(error: QueryError) -> Self {
        Self(match error.error_name {
            Some(name) => format!("{name}: {}", error.message),
            None => error.message,
        })
    }
}

/// A statement's error, pointing into `sql` where Trino's location does.
pub(crate) fn statement(error: QueryError, sql: &str) -> DriverError {
    let position = error
        .error_location
        .and_then(|at| position_at(sql, at.line_number, at.column_number));
    DriverError::Statement {
        source: Box::new(Refused::from(error)),
        position,
        sqlstate: None,
    }
}

/// An answer that isn't a statement's: a refused sign-in, an unknown path, a coordinator that
/// stays busy.
pub(crate) fn status(answer: &Answer) -> DriverError {
    let detail = answer.text();
    let message = match answer.status {
        StatusCode::UNAUTHORIZED => {
            "the coordinator refused the user or password (401 Unauthorized)".to_owned()
        }
        status if detail.trim().is_empty() => format!("the coordinator answered {status}"),
        status => format!("the coordinator answered {status}: {}", detail.trim()),
    };
    DriverError::Connection(Box::new(Refused(message)))
}

pub(crate) fn connect(error: impl Into<Box<dyn Error + Send + Sync>>) -> DriverError {
    DriverError::Connect(error.into())
}

pub(crate) fn connection(error: impl Into<Box<dyn Error + Send + Sync>>) -> DriverError {
    DriverError::Connection(error.into())
}

/// What kind of mistake Trino's error names: one it couldn't read, a name it couldn't find,
/// or anything else.
pub(crate) fn rejection_kind(error_name: Option<&str>) -> RejectionKind {
    match error_name {
        Some("SYNTAX_ERROR") => RejectionKind::Syntax,
        Some(name) if name.ends_with("_NOT_FOUND") => RejectionKind::UnknownName,
        _ => RejectionKind::Other,
    }
}
