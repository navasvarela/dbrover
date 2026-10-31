//! The servers' error answers as DbRover's errors, with where in a request's body the trouble
//! is when the server says.

use dbrover_core::{BoxError, DriverError};
use dbrover_document::Value;
use dbrover_language::search::{Body, ParseError, Request};

use crate::http::Answer;
use crate::json::{field, text_of};

/// A failure while connecting.
pub(crate) fn connect(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connect(error.into())
}

/// A failure reading the catalog or answering a ping.
pub(crate) fn connection(error: impl Into<BoxError>) -> DriverError {
    DriverError::Connection(error.into())
}

/// A request DbRover couldn't read, pointing at where in it the trouble is.
pub(crate) fn parse(error: ParseError) -> DriverError {
    let position = error.range.start;
    statement(error, Some(position))
}

pub(crate) fn statement(error: impl Into<BoxError>, position: Option<usize>) -> DriverError {
    DriverError::Statement {
        source: error.into(),
        position,
        sqlstate: None,
    }
}

/// The request was stopped by ⌘. before its answer came.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub(crate) struct Cancelled;

/// An answer with an error status: the server's own `type` and `reason`, and the reason it
/// was caused by when there is one, as Kibana shows them.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Refused {
    pub message: String,
    pub status: u16,
    /// The server's `type`, to tell kinds of refusal apart.
    pub kind: Option<String>,
    /// Where the server found the trouble in the body, as its 1-based line and column.
    pub line_column: Option<(usize, usize)>,
}

impl Refused {
    /// The refusal `answer` carries.
    pub(crate) fn of(answer: &Answer) -> Self {
        let status = answer.status.as_u16();
        let Ok(json) = answer.json() else {
            let text = answer.text();
            let message = if text.trim().is_empty() {
                format!("the server answered {}", answer.status)
            } else {
                text.trim().to_owned()
            };
            return Self {
                message,
                status,
                kind: None,
                line_column: None,
            };
        };
        let error = field(&json, "error");
        let (message, kind, line_column) = match error {
            Some(error @ Value::Object(_)) => {
                (describe(error), text_of(error, "type"), location(error))
            }
            Some(Value::String(message)) => (message.clone(), None, None),
            _ => (json.to_json(), None, None),
        };
        Self {
            message,
            status,
            kind,
            line_column,
        }
    }

    /// The byte of `request` the server's line and column point at, in its JSON body.
    pub(crate) fn position_in(&self, request: &Request) -> Option<usize> {
        let (line, column) = self.line_column?;
        let Some(Body::Json { text, offset, .. }) = &request.body else {
            return None;
        };
        let line_start: usize = text
            .split_inclusive('\n')
            .take(line.checked_sub(1)?)
            .map(str::len)
            .sum();
        let in_line = text[line_start..]
            .char_indices()
            .nth(column.checked_sub(1)?)
            .map_or(0, |(at, _)| at);
        Some(offset + line_start + in_line)
    }
}

/// `type: reason`, then `caused by` the cause's when it says more.
fn describe(error: &Value) -> String {
    let kind = text_of(error, "type");
    let reason = text_of(error, "reason").unwrap_or_default();
    let mut message = match kind {
        Some(kind) => format!("{kind}: {reason}"),
        None => reason,
    };
    if let Some(cause) = field(error, "caused_by")
        && let Some(cause_reason) = text_of(cause, "reason")
        && !message.contains(&cause_reason)
    {
        message.push_str("; caused by ");
        message.push_str(&cause_reason);
    }
    message
}

/// The line and column a parsing error names, on the error or its first root cause.
fn location(error: &Value) -> Option<(usize, usize)> {
    let read = |value: &Value| {
        let number = |name| match field(value, name)? {
            Value::Number(number) => number.parse::<usize>().ok(),
            _ => None,
        };
        Some((number("line")?, number("col")?))
    };
    read(error).or_else(|| match field(error, "root_cause")? {
        Value::Array(causes) => causes.first().and_then(read),
        _ => None,
    })
}
