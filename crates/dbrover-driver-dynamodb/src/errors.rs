//! DynamoDB's and the SDK's errors as DbRover's: the service's code and message when it
//! answered (`ValidationException: …`), and the SDK's whole chain of causes when it couldn't
//! be reached.

use std::error::Error;
use std::fmt;

use aws_sdk_dynamodb::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use dbrover_core::DriverError;
use dbrover_language::partiql::ParseError;

/// What DynamoDB or the SDK said, as one line.
#[derive(Debug)]
pub(crate) struct Refused(pub(crate) String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Refused {}

/// The run was cancelled with ⌘. before DynamoDB answered.
#[derive(Debug, thiserror::Error)]
#[error("cancelled; a request DynamoDB had already received may still have been applied")]
pub(crate) struct Cancelled;

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

pub(crate) fn statement(error: impl Error + Send + Sync + 'static) -> DriverError {
    DriverError::Statement {
        source: Box::new(error),
        position: None,
        sqlstate: None,
    }
}

pub(crate) fn parse(error: ParseError) -> DriverError {
    DriverError::Statement {
        position: Some(error.range.start),
        source: Box::new(error),
        sqlstate: None,
    }
}

pub(crate) fn connect(error: impl Error + Send + Sync + 'static) -> DriverError {
    DriverError::Connect(Box::new(error))
}

pub(crate) fn connection(error: impl Error + Send + Sync + 'static) -> DriverError {
    DriverError::Connection(Box::new(error))
}
