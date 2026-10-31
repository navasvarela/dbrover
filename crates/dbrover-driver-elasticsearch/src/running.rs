//! A connection's run, and how ⌘. stops it: each request of a run is tagged with an
//! `X-Opaque-Id` unique to it, so the tasks it started can be found with the tasks API and
//! cancelled, and the request itself is dropped, which Elasticsearch also takes as a cancel.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use dbrover_core::{CancelHandle, DriverError};
use dbrover_document::Value;
use hyper::Method;
use tokio::sync::watch;

use crate::errors::{self, Cancelled, Refused};
use crate::http::{Answer, Call, Client, OPAQUE_ID};
use crate::json::{at, field, text_of};

/// The run a connection is in, shared with its cursors and its cancel handle.
#[derive(Debug)]
pub(crate) struct Running {
    opaque_id: Mutex<Option<String>>,
    /// Bumped by each cancel; a request waiting for its answer gives up when it changes.
    stops: watch::Sender<u64>,
}

impl Default for Running {
    fn default() -> Self {
        Self {
            opaque_id: Mutex::new(None),
            stops: watch::Sender::new(0),
        }
    }
}

impl Running {
    /// An id unique to a new run, kept until the next.
    pub(crate) fn start(&self) -> String {
        let id = format!("dbrover-{}", uuid::Uuid::new_v4());
        *self
            .opaque_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(id.clone());
        id
    }

    fn current(&self) -> Option<String> {
        self.opaque_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn stop(&self) {
        self.stops.send_modify(|stops| *stops += 1);
    }

    /// Sends `call`, giving up if the run is cancelled before the answer comes.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] when the run was cancelled or the node can't be reached.
    pub(crate) async fn send(
        &self,
        client: &Client,
        call: Call<'_>,
    ) -> Result<Answer, DriverError> {
        let mut stops = self.stops.subscribe();
        tokio::select! {
            answer = client.send(call) => answer.map_err(|error| errors::statement(error, None)),
            _ = stops.changed() => Err(errors::statement(Cancelled, None)),
        }
    }
}

/// Stops a connection's running request from another task.
pub(crate) struct SearchCancel {
    pub(crate) client: Client,
    pub(crate) running: Arc<Running>,
}

#[async_trait]
impl CancelHandle for SearchCancel {
    /// Cancels the run's tasks, then drops its request. Listing tasks needs the `monitor`
    /// cluster privilege, which a user who may only read may lack, so a failure there is
    /// logged and the request is dropped all the same: Elasticsearch stops a search whose
    /// client went away.
    async fn cancel(&self) -> Result<(), DriverError> {
        if let Some(opaque_id) = self.running.current()
            && let Err(error) = cancel_tasks(&self.client, &opaque_id).await
        {
            tracing::warn!(
                "could not cancel the running request's tasks: {}",
                dbrover_core::describe_error(error.as_ref())
            );
        }
        self.running.stop();
        Ok(())
    }
}

async fn cancel_tasks(client: &Client, opaque_id: &str) -> Result<(), dbrover_core::BoxError> {
    let answer = client
        .send(Call::get("/_tasks?detailed=true&group_by=none"))
        .await?;
    if !answer.status.is_success() {
        return Err(Box::new(Refused::of(&answer)));
    }
    let tasks = answer.json()?;
    let Some(Value::Array(tasks)) = field(&tasks, "tasks") else {
        return Ok(());
    };
    for task in tasks {
        let tagged = at(task, &["headers", OPAQUE_ID])
            .is_some_and(|tag| matches!(tag, Value::String(id) if id == opaque_id));
        let id = match (text_of(task, "node"), field(task, "id")) {
            (Some(node), Some(Value::Number(id))) => format!("{node}:{id}"),
            _ => continue,
        };
        if tagged {
            let target = format!("/_tasks/{id}/_cancel");
            client
                .send(Call {
                    method: Method::POST,
                    target: &target,
                    body: None,
                    opaque_id: None,
                })
                .await?;
        }
    }
    Ok(())
}
