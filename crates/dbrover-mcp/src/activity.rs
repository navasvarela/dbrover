//! `server_activity` and `stop_session`: what a data source's server is doing, and stopping a
//! session, for a project that allows `activity` (and `admin`, to stop) (W15,
//! `docs/server-activity.md`).
//!
//! Both run on the data source's connection outside the per-call transaction the other tools
//! use: reading the server's views needs none, PostgreSQL's read opens its own, and a stop
//! must not be rolled back.

use std::time::Instant;

use dbrover_core::{
    Connection, DataSource, DriverError, SafetyLevel, ServerActivity, ServerSession, SessionId,
    StopKind, StopOutcome, blocking_order, describe_error,
};
use dbrover_store::{HistoryOutcome, redact_secrets};
use jiff::Timestamp;
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::Ran;
use crate::server::{DbRoverMcp, ToolError};
use crate::transaction::RunError;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ActivityArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct StopArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// The session's id, as server_activity lists it.
    session: String,
    /// "cancel" stops the running statement and keeps the session; "kill" ends the session,
    /// rolling back its open transaction.
    stop: StopChoice,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StopChoice {
    Cancel,
    Kill,
}

impl From<StopChoice> for StopKind {
    fn from(choice: StopChoice) -> Self {
        match choice {
            StopChoice::Cancel => Self::CancelQuery,
            StopChoice::Kill => Self::EndSession,
        }
    }
}

impl DbRoverMcp {
    pub(crate) async fn activity_json(&self, args: ActivityArgs) -> Result<Value, ToolError> {
        let tool = "server_activity";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        let read = self
            .on_bare_connection(tool, data_source, |connection| {
                Box::pin(async move {
                    let own = connection.own_session().await?;
                    Ok((connection.server_activity().await?, own))
                })
            })
            .await?;
        let (activity, own) = read;
        Ok(activity_reply(&activity, own.as_ref()))
    }

    pub(crate) async fn stop_json(&self, args: StopArgs) -> Result<Value, ToolError> {
        let tool = "stop_session";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        if data_source.safety == SafetyLevel::ReadOnly {
            let reason = format!(
                "\u{201c}{}\u{201d} is read-only, so its sessions aren't stopped",
                data_source.name
            );
            return Err(self.refuse(tool, data_source, reason));
        }
        let wanted = session_id(&args.session);
        let kind = StopKind::from(args.stop);
        // The session is read again, on this call's connection, so the stop is the one the
        // driver writes for it now: matched by its start, never a reused id.
        let (activity, own) = self
            .on_bare_connection(tool, data_source, |connection| {
                Box::pin(async move {
                    let own = connection.own_session().await?;
                    Ok((connection.server_activity().await?, own))
                })
            })
            .await?;
        if own.as_ref() == Some(&wanted) {
            let reason = "that is this MCP server's own session".to_owned();
            return Err(self.refuse(tool, data_source, reason));
        }
        let Some(session) = activity
            .sessions
            .iter()
            .find(|session| session.id == wanted)
        else {
            return Err(ToolError::invalid(format!(
                "no session {wanted} is listed now; it may have ended (see server_activity)"
            )));
        };
        let Some(stop) = session.stop(kind).cloned() else {
            let reason = match (kind, &activity.others_stopped_by) {
                (_, Some(privilege)) => {
                    format!(
                        "{} isn't offered for session {wanted}; stopping another user's session needs {privilege}",
                        kind.label()
                    )
                }
                (_, None) => format!("{} isn't offered for session {wanted}", kind.label()),
            };
            return Err(self.refuse(tool, data_source, reason));
        };
        let started = Timestamp::now();
        let clock = Instant::now();
        let statement = stop.statement.clone();
        let result = self
            .on_bare_connection(tool, data_source, move |connection| {
                Box::pin(async move { connection.stop_session(&stop).await })
            })
            .await;
        self.inner
            .audit
            .record(Ran {
                data_source,
                sql: &statement,
                started,
                elapsed: clock.elapsed(),
                rows: None,
                outcome: match result {
                    Ok(_) => HistoryOutcome::Ok,
                    Err(_) => HistoryOutcome::Error,
                },
            })
            .await;
        let outcome = result?;
        Ok(json!({
            "session": wanted.to_string(),
            "stop": stop_name(kind),
            "outcome": match outcome {
                StopOutcome::Stopped => "stopped",
                StopOutcome::AlreadyEnded => "already_ended",
            },
            "statement": statement,
        }))
    }

    /// Runs `work` on the data source's connection without a transaction around it, then
    /// releases the connection as other calls do. A connection that fails is discarded, so
    /// the next call opens a new one.
    pub(crate) async fn on_bare_connection<T: Send>(
        &self,
        tool: &str,
        data_source: &DataSource,
        work: impl for<'c> FnOnce(
            &'c mut dyn Connection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, DriverError>> + Send + 'c>,
        >,
    ) -> Result<T, ToolError> {
        let result = match self.inner.connections.checkout(data_source.id).await {
            Ok(mut connection) => {
                let result = work(&mut *connection).await;
                if matches!(
                    result,
                    Err(DriverError::Lost(_) | DriverError::Connection(_))
                ) {
                    connection.discard();
                }
                result
            }
            Err(error) => Err(DriverError::Connect(Box::new(error))),
        };
        self.inner.connections.release(data_source.id).await;
        result.map_err(|error| {
            let message = describe_error(&error);
            self.inner
                .audit
                .failed(tool, &data_source.name, &message, None);
            ToolError::from_run(RunError::Driver(error), 0)
        })
    }
}

/// The sessions in the order the app draws them, each blocking chain from its root; other
/// users' statements redacted as the history's are.
fn activity_reply(activity: &ServerActivity, own: Option<&SessionId>) -> Value {
    let rows = blocking_order(&activity.sessions, |_| true);
    let sessions: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let session = activity.sessions.get(row.session)?;
            Some(session_json(session, row.depth, own == Some(&session.id)))
        })
        .collect();
    let hidden: Vec<String> = activity
        .restrictions
        .iter()
        .map(|restriction| {
            let what = match restriction.hidden {
                dbrover_core::Hidden::OthersSessions => "other users' sessions",
                dbrover_core::Hidden::OthersStatements => "statements",
                dbrover_core::Hidden::Blocking => "who blocks whom",
            };
            format!("{what} (shown with {})", restriction.shown_by)
        })
        .collect();
    json!({
        "sessions": sessions,
        "hidden": hidden,
        "stopping_others_needs": activity.others_stopped_by,
    })
}

fn session_json(session: &ServerSession, depth: usize, own: bool) -> Value {
    let stops: Vec<&str> = session
        .stops
        .iter()
        .map(|stop| stop_name(stop.kind))
        .collect();
    json!({
        "session": session.id.to_string(),
        "depth": depth,
        "this_mcp_server": own,
        "user": session.user,
        "database": session.database,
        "application": session.application,
        "client": session.client,
        "state": session.state.label(),
        "server_state": session.engine_state,
        "seconds_in_state": session.in_state_for.map(|time| time.as_secs()),
        "transaction_seconds": session.transaction_for.map(|time| time.as_secs()),
        "statement": session.statement.as_ref().map(|statement| redact_secrets(&statement.text)),
        "statement_running": session.statement.as_ref().map(|statement| statement.running),
        "waits_on": session.wait,
        "waits_for": session.lock.as_ref().map(ToString::to_string),
        "blocked_by": session.blocked_by.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "facts": session.extra.iter().map(|(label, value)| json!({ label: value })).collect::<Vec<_>>(),
        "stops": if own { Vec::new() } else { stops },
    })
}

fn stop_name(kind: StopKind) -> &'static str {
    match kind {
        StopKind::CancelQuery => "cancel",
        StopKind::EndSession => "kill",
    }
}

/// A number names a PostgreSQL, MySQL or SQL Server session; anything else a ClickHouse query.
fn session_id(text: &str) -> SessionId {
    match text.trim().parse::<i64>() {
        Ok(number) => SessionId::Number(number),
        Err(_) => SessionId::Query(text.trim().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::{SessionState, SessionStatement, SessionStop};

    use super::*;

    #[test]
    fn reads_session_ids_as_numbers_or_query_ids() {
        assert_eq!(session_id(" 4211 "), SessionId::Number(4211));
        assert_eq!(
            session_id("4f1c-9a"),
            SessionId::Query("4f1c-9a".to_owned())
        );
    }

    #[test]
    fn replies_in_blocking_order_with_statements_redacted() {
        let holder = ServerSession {
            id: SessionId::Number(1),
            state: SessionState::IdleInTransaction,
            transaction_for: Some(Duration::from_secs(90)),
            statement: Some(SessionStatement {
                text: "ALTER ROLE app PASSWORD 's3cret'".to_owned(),
                running: false,
                cut_short: false,
            }),
            stops: vec![SessionStop {
                kind: StopKind::EndSession,
                statement: "…".to_owned(),
            }],
            ..ServerSession::default()
        };
        let waiter = ServerSession {
            id: SessionId::Number(2),
            state: SessionState::Waiting,
            blocked_by: vec![SessionId::Number(1)],
            ..ServerSession::default()
        };
        let activity = ServerActivity {
            sessions: vec![waiter, holder],
            ..ServerActivity::default()
        };

        let reply = activity_reply(&activity, Some(&SessionId::Number(2)));

        let sessions = reply["sessions"].as_array().unwrap();
        assert_eq!(sessions[0]["session"], "1");
        assert_eq!(sessions[0]["stops"], json!(["kill"]));
        assert!(
            !sessions[0]["statement"]
                .as_str()
                .unwrap()
                .contains("s3cret"),
            "{reply}"
        );
        assert_eq!(sessions[1]["session"], "2");
        assert_eq!(sessions[1]["depth"], 1);
        assert_eq!(sessions[1]["this_mcp_server"], true);
    }
}
