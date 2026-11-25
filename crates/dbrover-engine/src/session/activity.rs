//! A Server activity tab's reads and stops (W15, `docs/server-activity.md`).

use std::time::{Duration, Instant};

use dbrover_core::{
    DriverError, ServerSession, SessionId, StopKind, StopOutcome, describe_error, spoken_duration,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource, redact_secrets};
use jiff::Timestamp;

use super::{Disconnected, Worker, cancel_running_statement};
use crate::{Activity, Event};

/// How long one read may take before it is cancelled. A read stuck behind a server in trouble
/// mustn't leave the tab waiting; it reads again at its next refresh. Drivers with a limit of
/// their own (PostgreSQL's `statement_timeout`) set the same.
const READ_LIMIT: Duration = Duration::from_secs(10);

/// How much of a stopped session's statement the history keeps in its detail: enough to
/// recognise it, short enough to read on one line.
const STATEMENT_IN_DETAIL: usize = 120;

pub(super) async fn read(worker: &mut Worker<'_>) -> Result<(), Disconnected> {
    let cancel_handle = worker.cancel_handle;
    let work = async {
        let read = worker.connection.server_activity();
        tokio::pin!(read);
        tokio::select! {
            biased;
            result = &mut read => result,
            () = tokio::time::sleep(READ_LIMIT) => {
                cancel_running_statement(cancel_handle).await;
                read.await
            }
        }
    };
    let result = worker.signals.guard(work, None, cancel_handle).await?;
    let context = worker.context;
    let result = match result {
        Ok(server) => {
            worker.activity = Some(server.clone());
            Ok(Activity {
                server,
                own: context
                    .own_sessions
                    .on(context.data_source.id, context.owner),
            })
        }
        Err(error) => {
            let message = describe_error(&error);
            tracing::warn!(data_source = %context.name(), "could not read the server's activity: {message}");
            Err(message)
        }
    };
    context.send(Event::ActivityRead {
        data_source: context.data_source.id,
        result,
    });
    Ok(())
}

pub(super) async fn stop(
    worker: &mut Worker<'_>,
    session: SessionId,
    kind: StopKind,
) -> Result<(), Disconnected> {
    let result = stop_listed(worker, &session, kind).await?;
    worker.context.send(Event::SessionStopped {
        data_source: worker.context.data_source.id,
        session,
        kind,
        result,
    });
    Ok(())
}

async fn stop_listed(
    worker: &mut Worker<'_>,
    session: &SessionId,
    kind: StopKind,
) -> Result<Result<StopOutcome, String>, Disconnected> {
    let context = worker.context;
    if context.read_only.is_some() {
        return Ok(Err(format!(
            "Refused: \u{201c}{}\u{201d} is read-only, so its sessions aren't stopped.",
            context.name()
        )));
    }
    if context.own_sessions.of(context.owner).as_ref() == Some(session) {
        return Ok(Err(
            "This is the Server activity tab's own session.".to_owned()
        ));
    }
    let Some(listed) = worker
        .activity
        .as_ref()
        .and_then(|activity| {
            activity
                .sessions
                .iter()
                .find(|listed| &listed.id == session)
        })
        .cloned()
    else {
        return Ok(Err(format!(
            "Session {session} wasn't in the last read; refresh and try again."
        )));
    };
    let Some(stop) = listed.stop(kind).cloned() else {
        return Ok(Err(format!(
            "{} isn't offered for session {session}.",
            kind.label()
        )));
    };

    tracing::info!(data_source = %context.name(), "{}", stop.statement);
    let started_at = Timestamp::now();
    let started = Instant::now();
    let cancel_handle = worker.cancel_handle;
    let result = worker
        .signals
        .guard(worker.connection.stop_session(&stop), None, cancel_handle)
        .await?;
    context.record(HistoryEntry {
        data_source: context.data_source.id,
        executed_at: started_at,
        sql: stop.statement,
        duration: Some(started.elapsed()),
        rows: None,
        outcome: match result {
            Ok(_) => HistoryOutcome::Ok,
            Err(_) => HistoryOutcome::Error,
        },
        source: HistorySource::Activity {
            stopped: stopped(&listed, kind, result.as_ref().ok().copied()),
        },
    });
    Ok(result.map_err(|error| refusal(&error)))
}

/// The server's own words for a refused stop, without the driver's wrapping.
fn refusal(error: &DriverError) -> String {
    match error {
        DriverError::Statement { source, .. } => describe_error(source.as_ref()),
        other => describe_error(other),
    }
}

/// What the history says was stopped: *Ended session 4211 (app on orders-api, shop), idle in
/// transaction for 41 min, last ran UPDATE orders SET …*. Another user's statement can hold
/// anything they typed, so it is redacted as the history's own statements are.
fn stopped(session: &ServerSession, kind: StopKind, outcome: Option<StopOutcome>) -> String {
    let verb = match kind {
        StopKind::CancelQuery => "Cancelled the statement of session",
        StopKind::EndSession => "Ended session",
    };
    let mut text = format!("{verb} {}", session.id);
    let whose: Vec<&str> = [&session.user, &session.application, &session.database]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !whose.is_empty() {
        text.push_str(&format!(" ({})", whose.join(", ")));
    }
    text.push_str(&format!(", {}", session.state.label()));
    if let Some(time) = session.in_state_for {
        text.push_str(&format!(" for {}", spoken_duration(time)));
    }
    if let Some(statement) = &session.statement {
        let verb = if statement.running {
            "running"
        } else {
            "last ran"
        };
        let redacted = redact_secrets(&statement.text);
        let line = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
        let shown: String = line.chars().take(STATEMENT_IN_DETAIL).collect();
        let more = if shown.len() < line.len() {
            "\u{2026}"
        } else {
            ""
        };
        text.push_str(&format!(", {verb} {shown}{more}"));
    }
    if outcome == Some(StopOutcome::AlreadyEnded) {
        text.push_str(match kind {
            StopKind::CancelQuery => "; its statement had already ended",
            StopKind::EndSession => "; it had already ended",
        });
    }
    text
}

#[cfg(test)]
mod tests {
    use dbrover_core::{SessionState, SessionStatement};

    use super::*;

    fn session() -> ServerSession {
        ServerSession {
            id: SessionId::Number(4211),
            user: Some("app".to_owned()),
            application: Some("orders-api".to_owned()),
            database: Some("shop".to_owned()),
            state: SessionState::IdleInTransaction,
            in_state_for: Some(Duration::from_secs(41 * 60 + 7)),
            statement: Some(SessionStatement {
                text: "UPDATE orders\n   SET status = 'paid'\n WHERE id = 3".to_owned(),
                running: false,
                cut_short: false,
            }),
            ..ServerSession::default()
        }
    }

    #[test]
    fn says_what_was_stopped_and_whose_it_was() {
        assert_eq!(
            stopped(&session(), StopKind::EndSession, Some(StopOutcome::Stopped)),
            "Ended session 4211 (app, orders-api, shop), idle in transaction for 41 min, \
             last ran UPDATE orders SET status = 'paid' WHERE id = 3"
        );
        let bare = ServerSession {
            id: SessionId::Query("4f1c".to_owned()),
            state: SessionState::Active,
            ..ServerSession::default()
        };
        assert_eq!(
            stopped(
                &bare,
                StopKind::CancelQuery,
                Some(StopOutcome::AlreadyEnded)
            ),
            "Cancelled the statement of session 4f1c, active; its statement had already ended"
        );
    }

    #[test]
    fn redacts_and_shortens_the_statement() {
        let mut secret = session();
        secret.statement = Some(SessionStatement {
            text: format!("ALTER ROLE app PASSWORD 's3cret' -- {}", "x".repeat(300)),
            running: true,
            cut_short: false,
        });

        let text = stopped(&secret, StopKind::EndSession, None);

        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.contains("running ALTER ROLE app PASSWORD"), "{text}");
        assert!(text.ends_with('\u{2026}'), "{text}");
    }
}
