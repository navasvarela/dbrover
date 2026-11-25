//! The server's sessions, from `pg_stat_activity`, who waits on whose locks, and the statements
//! that stop them (W15, `docs/server-activity.md`).

use std::collections::HashMap;
use std::time::Duration;

use dbrover_core::{
    DriverError, Hidden, LockWait, Restriction, ServerActivity, ServerSession, SessionId,
    SessionState, SessionStatement, SessionStop, StopKind, StopOutcome,
};
use tokio_postgres::types::{FromSql, Type};
use tokio_postgres::{Client, Row, SimpleQueryMessage};

use crate::connection_error;
use crate::execution::statement_error;

/// The role whose members see every session's statement, state and times.
const SEES_EVERYTHING: &str = "pg_read_all_stats";
/// The role whose members may cancel and end other roles' sessions, superusers' apart.
const STOPS_OTHERS: &str = "pg_signal_backend";
/// What `pg_stat_activity` shows in `query` for a session the login may not look into.
const HIDDEN_QUERY: &str = "<insufficient privilege>";
/// The `backend_type` of a client's session; every other type is the server's own work.
const CLIENT_BACKEND: &str = "client backend";
const PARALLEL_WORKER: &str = "parallel worker";

/// A read stuck behind a server in trouble mustn't leave the tab waiting: the tab reads again
/// on its next refresh.
const READ_TIMEOUT: &str = "10s";

/// What this login may see and do, asked with each read since a grant can change meanwhile.
struct Login {
    superuser: bool,
    sees_everything: bool,
    stops_others: bool,
    /// The bytes of a statement the server keeps (`track_activity_query_size`), less the one
    /// its terminating zero takes.
    statement_limit: usize,
    /// `leader_pid` came in PostgreSQL 13.
    has_leader_pid: bool,
}

/// Reads every session in one read-only transaction, so `pg_stat_activity` and `pg_locks` are
/// read at one moment, under a time limit of their own.
pub(crate) async fn read(client: &Client) -> Result<ServerActivity, DriverError> {
    client
        .batch_execute(&format!(
            "BEGIN READ ONLY; SET LOCAL statement_timeout = '{READ_TIMEOUT}'"
        ))
        .await
        .map_err(connection_error)?;
    let result = read_in_transaction(client).await;
    let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
    let ended = client.batch_execute(end).await.map_err(connection_error);
    let activity = result?;
    ended?;
    Ok(activity)
}

async fn read_in_transaction(client: &Client) -> Result<ServerActivity, DriverError> {
    let login = login(client).await?;
    let rows = client
        .query_typed(&sessions_query(&login), &[])
        .await
        .map_err(connection_error)?;
    let listed = rows
        .iter()
        .map(Listed::from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let waiting: Vec<i32> = listed
        .iter()
        .filter(|session| !session.blocked_by.is_empty())
        .map(|session| session.pid)
        .collect();
    let mut locks = if waiting.is_empty() {
        HashMap::new()
    } else {
        lock_waits(client, &waiting).await?
    };
    Ok(activity(&login, listed, &mut locks))
}

async fn login(client: &Client) -> Result<Login, DriverError> {
    let row = client
        .query_typed(
            &format!(
                "SELECT r.rolsuper, \
                        pg_has_role(current_user, '{SEES_EVERYTHING}', 'MEMBER'), \
                        pg_has_role(current_user, '{STOPS_OTHERS}', 'MEMBER'), \
                        (SELECT setting::int FROM pg_settings \
                          WHERE name = 'track_activity_query_size'), \
                        current_setting('server_version_num')::int \
                 FROM pg_roles r WHERE r.rolname = current_user"
            ),
            &[],
        )
        .await
        .map_err(connection_error)?
        .into_iter()
        .next()
        .ok_or(DriverError::Unsupported(
            "The login's own role can't be read, so its privileges are unknown.",
        ))?;
    let statement_limit: i32 = get(&row, 3)?;
    let version: i32 = get(&row, 4)?;
    Ok(Login {
        superuser: get(&row, 0)?,
        sees_everything: get(&row, 1)?,
        stops_others: get(&row, 2)?,
        statement_limit: usize::try_from(statement_limit)
            .unwrap_or_default()
            .saturating_sub(1),
        has_leader_pid: version >= 130_000,
    })
}

fn sessions_query(login: &Login) -> String {
    let leader = if login.has_leader_pid {
        "a.leader_pid"
    } else {
        "NULL::int"
    };
    // `pg_blocking_pids` takes the lock manager's locks, so it is asked only of the sessions
    // waiting on a lock. Ages are the server's, so the client's clock doesn't matter.
    format!(
        "SELECT a.pid, a.backend_start::text, a.usename::text, a.datname::text, \
                NULLIF(a.application_name, ''), host(a.client_addr), a.state, a.backend_type, \
                a.wait_event_type, a.wait_event, a.query, a.query_start::text, \
                extract(epoch FROM clock_timestamp() - a.state_change)::float8, \
                extract(epoch FROM clock_timestamp() - a.xact_start)::float8, \
                CASE WHEN a.wait_event_type = 'Lock' THEN pg_blocking_pids(a.pid) \
                     ELSE '{{}}'::int[] END, \
                {leader}, \
                a.usesysid IS NOT NULL AND pg_has_role(a.usesysid, 'MEMBER'), \
                COALESCE((SELECT r.rolsuper FROM pg_roles r WHERE r.oid = a.usesysid), false) \
         FROM pg_stat_activity a"
    )
}

/// A row of `pg_stat_activity`, as read.
struct Listed {
    pid: i32,
    backend_start: Option<String>,
    user: Option<String>,
    database: Option<String>,
    application: Option<String>,
    client: Option<String>,
    state: Option<String>,
    backend_type: Option<String>,
    wait_event_type: Option<String>,
    wait_event: Option<String>,
    query: Option<String>,
    query_start: Option<String>,
    in_state_seconds: Option<f64>,
    transaction_seconds: Option<f64>,
    blocked_by: Vec<i32>,
    leader: Option<i32>,
    /// The login is a member of the session's role, so it may stop it whatever else it has.
    same_role: bool,
    superuser_session: bool,
}

impl Listed {
    fn from_row(row: &Row) -> Result<Self, DriverError> {
        Ok(Self {
            pid: get(row, 0)?,
            backend_start: get(row, 1)?,
            user: get(row, 2)?,
            database: get(row, 3)?,
            application: get(row, 4)?,
            client: get(row, 5)?,
            state: get(row, 6)?,
            backend_type: get(row, 7)?,
            wait_event_type: get(row, 8)?,
            wait_event: get(row, 9)?,
            query: get(row, 10)?,
            query_start: get(row, 11)?,
            in_state_seconds: get(row, 12)?,
            transaction_seconds: get(row, 13)?,
            blocked_by: get(row, 14)?,
            leader: get(row, 15)?,
            same_role: get(row, 16)?,
            superuser_session: get(row, 17)?,
        })
    }

    fn is_client(&self) -> bool {
        self.backend_type.as_deref() == Some(CLIENT_BACKEND)
    }

    fn is_hidden(&self) -> bool {
        self.query.as_deref() == Some(HIDDEN_QUERY)
    }

    fn state(&self) -> SessionState {
        // Without the privilege, another role's session shows neither its state nor even its
        // kind of process.
        match self.backend_type.as_deref() {
            None => return SessionState::Hidden,
            Some(CLIENT_BACKEND) => {}
            Some(_) => return SessionState::Background,
        }
        match self.state.as_deref() {
            Some("active" | "fastpath function call") if !self.blocked_by.is_empty() => {
                SessionState::Waiting
            }
            Some("active" | "fastpath function call") => SessionState::Active,
            Some(state) if state.starts_with("idle in transaction") => {
                SessionState::IdleInTransaction
            }
            Some(_) => SessionState::Idle,
            None => SessionState::Hidden,
        }
    }

    fn may_stop(&self, login: &Login) -> bool {
        login.superuser || self.same_role || (login.stops_others && !self.superuser_session)
    }

    /// The statements that stop this session, matched by its start (pids are reused) and,
    /// for Cancel, by its statement's start, so a statement begun since isn't the one stopped.
    fn stops(&self, login: &Login, state: SessionState) -> Vec<SessionStop> {
        let Some(backend_start) = &self.backend_start else {
            return Vec::new();
        };
        if !self.is_client() || !self.may_stop(login) {
            return Vec::new();
        }
        let session = format!(
            "FROM pg_stat_activity WHERE pid = {pid} AND backend_start = {start}",
            pid = self.pid,
            start = literal(backend_start),
        );
        let mut stops = Vec::new();
        if let (SessionState::Active | SessionState::Waiting, Some(query_start)) =
            (state, &self.query_start)
        {
            stops.push(SessionStop {
                kind: StopKind::CancelQuery,
                statement: format!(
                    "SELECT pg_cancel_backend(pid) {session} AND query_start = {}",
                    literal(query_start)
                ),
            });
        }
        stops.push(SessionStop {
            kind: StopKind::EndSession,
            statement: format!("SELECT pg_terminate_backend(pid) {session}"),
        });
        stops
    }

    fn statement(&self, login: &Login, state: SessionState) -> Option<SessionStatement> {
        let text = self.query.as_deref().filter(|text| !text.is_empty())?;
        if self.is_hidden() {
            return None;
        }
        Some(SessionStatement {
            text: text.to_owned(),
            running: matches!(state, SessionState::Active | SessionState::Waiting),
            cut_short: login.statement_limit > 0 && text.len() >= login.statement_limit,
        })
    }

    fn into_session(self, login: &Login, lock: Option<LockWait>, workers: usize) -> ServerSession {
        let state = self.state();
        let running = matches!(state, SessionState::Active | SessionState::Waiting);
        let wait = match (&self.wait_event_type, &self.wait_event) {
            (Some(kind), Some(event)) if running => Some(format!("{kind}: {event}")),
            _ => None,
        };
        // PostgreSQL's own word where it says more than DbRover's: aborted transactions, a
        // fast-path call, or tracking switched off.
        let engine_state = self
            .state
            .clone()
            .filter(|word| word.as_str() != state.label());
        let mut extra = Vec::new();
        if let Some(backend_type) = self.backend_type.as_ref().filter(|_| !self.is_client()) {
            extra.push(("Process".to_owned(), backend_type.clone()));
        }
        if workers > 0 {
            extra.push(("Parallel workers".to_owned(), workers.to_string()));
        }
        ServerSession {
            id: SessionId::Number(i64::from(self.pid)),
            statement: self.statement(login, state),
            stops: self.stops(login, state),
            user: self.user,
            database: self.database,
            application: self.application,
            client: self.client,
            state,
            engine_state,
            in_state_for: duration(self.in_state_seconds),
            transaction_for: duration(self.transaction_seconds),
            wait,
            lock,
            blocked_by: self
                .blocked_by
                .into_iter()
                .map(|pid| SessionId::Number(i64::from(pid)))
                .collect(),
            extra,
        }
    }
}

fn activity(
    login: &Login,
    listed: Vec<Listed>,
    locks: &mut HashMap<i32, LockWait>,
) -> ServerActivity {
    let hides_statements =
        !login.superuser && !login.sees_everything && listed.iter().any(Listed::is_hidden);
    let pids: Vec<i32> = listed.iter().map(|session| session.pid).collect();
    // A parallel worker is part of its leader's statement: it is counted on the leader's row
    // rather than listed, unless its leader isn't listed.
    let mut workers: HashMap<i32, usize> = HashMap::new();
    let mut shown = Vec::with_capacity(listed.len());
    for session in listed {
        let folded = session.backend_type.as_deref() == Some(PARALLEL_WORKER)
            && session.leader.is_some_and(|leader| pids.contains(&leader));
        match session.leader.filter(|_| folded) {
            Some(leader) => *workers.entry(leader).or_default() += 1,
            None => shown.push(session),
        }
    }
    let sessions = shown
        .into_iter()
        .map(|session| {
            let lock = locks.remove(&session.pid);
            let workers = workers.get(&session.pid).copied().unwrap_or_default();
            session.into_session(login, lock, workers)
        })
        .collect();
    let mut restrictions = Vec::new();
    if hides_statements {
        restrictions.push(Restriction {
            hidden: Hidden::OthersStatements,
            shown_by: SEES_EVERYTHING.to_owned(),
        });
    }
    ServerActivity {
        sessions,
        restrictions,
        others_stopped_by: (!login.superuser && !login.stops_others)
            .then(|| STOPS_OTHERS.to_owned()),
    }
}

/// The lock each waiting session waits for. A row lock is waited for as the holder's
/// transaction id, which names no table, so the row's table comes from the `tuple` lock the
/// waiter holds while it waits.
async fn lock_waits(client: &Client, pids: &[i32]) -> Result<HashMap<i32, LockWait>, DriverError> {
    // A relation in another database can't be named from this one; its oid is shown instead.
    let sql = "SELECT l.pid, l.granted, l.locktype, l.mode, \
                      CASE WHEN l.relation IS NULL THEN NULL \
                           WHEN l.database = (SELECT oid FROM pg_database \
                                               WHERE datname = current_database()) \
                             THEN l.relation::regclass::text \
                           ELSE 'relation ' || l.relation END, \
                      l.transactionid::text, l.virtualxid \
               FROM pg_locks l \
               WHERE l.pid = ANY($1) AND (NOT l.granted OR l.locktype = 'tuple')";
    let rows = client
        .query_typed(sql, &[(&pids, Type::INT4_ARRAY)])
        .await
        .map_err(connection_error)?;
    let mut waits: HashMap<i32, LockWait> = HashMap::new();
    let mut rows_of: HashMap<i32, String> = HashMap::new();
    for row in &rows {
        let pid: i32 = get(row, 0)?;
        let granted: bool = get(row, 1)?;
        let kind: String = get(row, 2)?;
        let relation: Option<String> = get(row, 4)?;
        if granted {
            if let Some(relation) = relation {
                rows_of.insert(pid, relation);
            }
            continue;
        }
        let transaction: Option<String> = get(row, 5)?;
        let virtual_transaction: Option<String> = get(row, 6)?;
        let object = match kind.as_str() {
            "relation" => relation,
            "tuple" => relation.map(|relation| format!("a row of {relation}")),
            "transactionid" => transaction.map(|id| format!("transaction {id}")),
            "virtualxid" => virtual_transaction.map(|id| format!("virtual transaction {id}")),
            _ => {
                Some(relation.map_or_else(|| kind.clone(), |relation| format!("{kind} {relation}")))
            }
        };
        waits.insert(
            pid,
            LockWait {
                object,
                mode: get(row, 3)?,
            },
        );
    }
    for (pid, relation) in rows_of {
        if let Some(wait) = waits.get_mut(&pid)
            && let Some(object) = wait.object.take()
        {
            wait.object = Some(if object.starts_with("transaction ") {
                format!("a row of {relation}, held by {object}")
            } else {
                object
            });
        }
    }
    Ok(waits)
}

/// Sends a stop [`read`] wrote, as it is, and reads its answer: one row saying whether the
/// signal was sent, or none when the session (or its statement) no longer matches.
pub(crate) async fn stop(client: &Client, stop: &SessionStop) -> Result<StopOutcome, DriverError> {
    let messages = client
        .simple_query(&stop.statement)
        .await
        .map_err(|error| statement_error(&stop.statement, error))?;
    let signalled = messages.iter().any(|message| match message {
        SimpleQueryMessage::Row(row) => row.get(0) == Some("t"),
        _ => false,
    });
    Ok(if signalled {
        StopOutcome::Stopped
    } else {
        StopOutcome::AlreadyEnded
    })
}

pub(crate) async fn own_session(client: &Client) -> Result<SessionId, DriverError> {
    let row = client
        .query_typed("SELECT pg_backend_pid()", &[])
        .await
        .map_err(connection_error)?
        .into_iter()
        .next()
        .ok_or(DriverError::Unsupported("The server didn't say its pid."))?;
    let pid: i32 = get(&row, 0)?;
    Ok(SessionId::Number(i64::from(pid)))
}

/// A text literal, for values the server itself wrote (its timestamps), so the statement
/// shown before sending is the statement sent.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Seconds as the server measured them; a negative age (a clock adjusted backwards) is none.
fn duration(seconds: Option<f64>) -> Option<Duration> {
    seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

fn get<'a, T: FromSql<'a>>(row: &'a Row, column: usize) -> Result<T, DriverError> {
    row.try_get(column).map_err(connection_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> Login {
        Login {
            superuser: false,
            sees_everything: false,
            stops_others: false,
            statement_limit: 1023,
            has_leader_pid: true,
        }
    }

    fn listed(pid: i32, state: &str) -> Listed {
        Listed {
            pid,
            backend_start: Some("2026-11-25 09:12:03.501+00".to_owned()),
            user: Some("app".to_owned()),
            database: Some("shop".to_owned()),
            application: Some("orders-api".to_owned()),
            client: Some("10.0.3.7".to_owned()),
            state: Some(state.to_owned()),
            backend_type: Some(CLIENT_BACKEND.to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: Some("UPDATE orders SET status = 'paid' WHERE id = 3".to_owned()),
            query_start: Some("2026-11-25 09:15:00.25+00".to_owned()),
            in_state_seconds: Some(12.5),
            transaction_seconds: None,
            blocked_by: Vec::new(),
            leader: None,
            same_role: true,
            superuser_session: false,
        }
    }

    #[test]
    fn writes_stops_matched_by_the_session_and_statement_start() {
        let session = listed(4211, "active").into_session(&login(), None, 0);

        assert_eq!(
            session.stops,
            [
                SessionStop {
                    kind: StopKind::CancelQuery,
                    statement: "SELECT pg_cancel_backend(pid) FROM pg_stat_activity \
                                WHERE pid = 4211 \
                                AND backend_start = '2026-11-25 09:12:03.501+00' \
                                AND query_start = '2026-11-25 09:15:00.25+00'"
                        .to_owned(),
                },
                SessionStop {
                    kind: StopKind::EndSession,
                    statement: "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                                WHERE pid = 4211 \
                                AND backend_start = '2026-11-25 09:12:03.501+00'"
                        .to_owned(),
                },
            ]
        );
    }

    #[test]
    fn maps_states_and_offers_cancel_only_while_a_statement_runs() {
        let cases = [
            ("active", SessionState::Active, None, 2),
            ("idle", SessionState::Idle, None, 1),
            (
                "idle in transaction",
                SessionState::IdleInTransaction,
                None,
                1,
            ),
            (
                "idle in transaction (aborted)",
                SessionState::IdleInTransaction,
                Some("idle in transaction (aborted)"),
                1,
            ),
            (
                "fastpath function call",
                SessionState::Active,
                Some("fastpath function call"),
                2,
            ),
        ];
        for (word, state, engine_state, stops) in cases {
            let session = listed(1, word).into_session(&login(), None, 0);
            assert_eq!(session.state, state, "{word}");
            assert_eq!(session.engine_state.as_deref(), engine_state, "{word}");
            assert_eq!(session.stops.len(), stops, "{word}");
            assert_eq!(
                session.statement.map(|statement| statement.running),
                Some(stops == 2),
                "{word}"
            );
        }
    }

    #[test]
    fn a_session_waiting_on_a_lock_is_waiting_with_its_blockers() {
        let mut waiting = listed(7, "active");
        waiting.blocked_by = vec![4211, 4302];
        waiting.wait_event_type = Some("Lock".to_owned());
        waiting.wait_event = Some("transactionid".to_owned());

        let session = waiting.into_session(&login(), None, 0);

        assert_eq!(session.state, SessionState::Waiting);
        assert_eq!(session.wait.as_deref(), Some("Lock: transactionid"));
        assert_eq!(
            session.blocked_by,
            [SessionId::Number(4211), SessionId::Number(4302)]
        );
    }

    #[test]
    fn stops_are_offered_as_the_login_may_send_them() {
        let other = |superuser_session| Listed {
            same_role: false,
            superuser_session,
            ..listed(9, "active")
        };
        let signaller = Login {
            stops_others: true,
            ..login()
        };
        let superuser = Login {
            superuser: true,
            ..login()
        };

        assert!(
            other(false)
                .into_session(&login(), None, 0)
                .stops
                .is_empty()
        );
        assert_eq!(
            other(false).into_session(&signaller, None, 0).stops.len(),
            2
        );
        assert!(
            other(true)
                .into_session(&signaller, None, 0)
                .stops
                .is_empty()
        );
        assert_eq!(other(true).into_session(&superuser, None, 0).stops.len(), 2);

        let background = Listed {
            backend_type: Some("autovacuum worker".to_owned()),
            ..listed(3, "active")
        };
        let session = background.into_session(&superuser, None, 0);
        assert_eq!(session.state, SessionState::Background);
        assert!(session.stops.is_empty());
        assert_eq!(
            session.extra,
            [("Process".to_owned(), "autovacuum worker".to_owned())]
        );
    }

    #[test]
    fn hides_what_the_login_may_not_see_and_says_so() {
        let hidden = Listed {
            state: None,
            backend_type: None,
            query: Some(HIDDEN_QUERY.to_owned()),
            query_start: None,
            backend_start: None,
            same_role: false,
            ..listed(12, "active")
        };

        let read = activity(
            &login(),
            vec![hidden, listed(13, "idle")],
            &mut HashMap::new(),
        );

        let session = &read.sessions[0];
        assert_eq!(session.state, SessionState::Hidden);
        assert_eq!(session.statement, None);
        assert!(session.stops.is_empty());
        assert_eq!(
            read.restrictions,
            [Restriction {
                hidden: Hidden::OthersStatements,
                shown_by: SEES_EVERYTHING.to_owned(),
            }]
        );
        assert_eq!(read.others_stopped_by.as_deref(), Some(STOPS_OTHERS));
    }

    #[test]
    fn folds_parallel_workers_into_their_leader() {
        let worker = |pid, leader| Listed {
            backend_type: Some(PARALLEL_WORKER.to_owned()),
            leader: Some(leader),
            ..listed(pid, "active")
        };

        let read = activity(
            &login(),
            vec![
                listed(1, "active"),
                worker(2, 1),
                worker(3, 1),
                worker(4, 99),
            ],
            &mut HashMap::new(),
        );

        let ids: Vec<_> = read.sessions.iter().map(|session| &session.id).collect();
        assert_eq!(ids, [&SessionId::Number(1), &SessionId::Number(4)]);
        assert_eq!(
            read.sessions[0].extra,
            [("Parallel workers".to_owned(), "2".to_owned())]
        );
    }

    #[test]
    fn marks_a_statement_as_long_as_the_server_keeps_as_cut_short() {
        let mut long = listed(5, "active");
        long.query = Some("x".repeat(1023));

        let statement = long.into_session(&login(), None, 0).statement.unwrap();

        assert!(statement.cut_short);
    }

    #[test]
    fn quotes_literals() {
        assert_eq!(literal("it's"), "'it''s'");
    }
}
