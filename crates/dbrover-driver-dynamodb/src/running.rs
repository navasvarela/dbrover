//! How ⌘. stops a connection's run. DynamoDB has no way to stop a request it has received,
//! and each request is short (at most 1 MB read), so a cancel drops the request in flight and
//! the run sends no more: a Scan stops between requests.

use std::future::Future;

use async_trait::async_trait;
use dbrover_core::{CancelHandle, DriverError};
use tokio::sync::watch;

use crate::errors::{self, Cancelled};

/// Shared by a connection, its cursors and its cancel handle.
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
    /// The cancels so far: a run, or a page of one, notes it when it starts, and stops once
    /// another cancel comes, even one that came between two of its requests.
    pub(crate) fn generation(&self) -> u64 {
        *self.stops.borrow()
    }

    /// Waits for `request`, giving up if the run that started at `generation` is cancelled
    /// first.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] when the run was cancelled, or with `request`'s own error.
    pub(crate) async fn send<T, E>(
        &self,
        generation: u64,
        request: impl Future<Output = Result<T, E>>,
    ) -> Result<T, DriverError>
    where
        E: Into<errors::Refused>,
    {
        let mut stops = self.stops.subscribe();
        tokio::select! {
            biased;
            _ = stops.wait_for(|stops| *stops != generation) => Err(errors::statement(Cancelled)),
            answer = request => answer.map_err(|error| errors::statement(error.into())),
        }
    }

    fn stop(&self) {
        self.stops.send_modify(|stops| *stops += 1);
    }
}

/// Stops a connection's running request from another task.
pub(crate) struct DynamoCancel(pub(crate) std::sync::Arc<Running>);

#[async_trait]
impl CancelHandle for DynamoCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.0.stop();
        Ok(())
    }
}
