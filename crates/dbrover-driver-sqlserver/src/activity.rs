//! The server's sessions, from `sys.dm_exec_sessions` and the requests they run, who blocks
//! whom, and `KILL` (W15, `docs/server-activity.md`).
//!
//! SQL Server can't stop another session's statement and keep the session: that is an
//! attention signal only the session's own client sends. So only Kill is offered.

use std::collections::HashSet;
use std::time::Duration;

use dbrover_core::{
    DriverError, Hidden, LockWait, Restriction, ServerActivity, ServerSession, SessionId,
    SessionState, SessionStatement, SessionStop, StopKind, StopOutcome,
};
use tiberius::Row;

use crate::errors::statement_error;
use crate::{SqlClient, column, connection_error};

/// What lets a login see every session: `VIEW SERVER STATE` up to 2019, its narrower part
/// `VIEW SERVER PERFORMANCE STATE` from 2022 (which `HAS_PERMS_BY_NAME` doesn't know before).
const SEES_EVERYTHING: [&str; 2] = ["VIEW SERVER STATE", "VIEW SERVER PERFORMANCE STATE"];
/// What `KILL` needs, for any session, the login's own included.
const KILLS: &str = "ALTER ANY CONNECTION";
/// The prefix of the wait types of a request waiting on a lock (`LCK_M_U`, `LCK_M_X`).
const LOCK_WAIT: &str = "LCK_M_";

/// Every session, with its request if it is running one. A session holding locks with nothing
/// running (the usual head blocker) has no request, hence the left joins. The statement is the
/// request's own, cut from its batch by its offsets, or an idle session's last batch.
///
/// `{client}`, `{transaction}` and `{statement}` (with `{statement_sources}`) are the client's
/// address, the transaction's age and the statement, which come from views a login without
/// VIEW SERVER STATE may not read even for its own session; for it they are `NULL`
/// ([`sessions_query`]).
const SESSIONS: &str = "\
SELECT s.session_id,
       CONVERT(varchar(23), s.login_time, 126),
       s.login_name,
       DB_NAME(COALESCE(r.database_id, s.database_id)),
       s.program_name,
       {client},
       s.status,
       r.status,
       r.command,
       CAST(s.is_user_process AS int),
       s.open_transaction_count,
       DATEDIFF_BIG(SECOND, COALESCE(r.start_time, s.last_request_end_time), SYSDATETIME()),
       {transaction},
       r.blocking_session_id,
       r.wait_type,
       r.wait_resource,
       CAST(r.percent_complete AS float),
       {statement}
FROM sys.dm_exec_sessions s
LEFT JOIN sys.dm_exec_requests r ON r.session_id = s.session_id
{statement_sources}
ORDER BY s.session_id";

const CLIENT: &str = "(SELECT TOP (1) client_net_address FROM sys.dm_exec_connections \
                       WHERE session_id = s.session_id)";
const TRANSACTION_AGE: &str = "(SELECT DATEDIFF_BIG(SECOND, MIN(a.transaction_begin_time), \
                                SYSDATETIME()) \
                                FROM sys.dm_tran_session_transactions st \
                                JOIN sys.dm_tran_active_transactions a \
                                  ON a.transaction_id = st.transaction_id \
                                WHERE st.session_id = s.session_id)";

const STATEMENT: &str = "COALESCE(\
    SUBSTRING(t.text, r.statement_start_offset / 2 + 1, \
              (CASE r.statement_end_offset WHEN -1 THEN DATALENGTH(t.text) \
               ELSE r.statement_end_offset END - r.statement_start_offset) / 2 + 1), \
    ib.event_info)";
const STATEMENT_SOURCES: &str = "\
OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t
OUTER APPLY (SELECT event_info FROM sys.dm_exec_input_buffer(s.session_id, NULL)
              WHERE r.session_id IS NULL AND s.is_user_process = 1) ib";

fn sessions_query(login: &Login) -> String {
    let (client, transaction, statement, sources) = if login.sees_everything {
        (CLIENT, TRANSACTION_AGE, STATEMENT, STATEMENT_SOURCES)
    } else {
        ("NULL", "NULL", "NULL", "")
    };
    SESSIONS
        .replace("{client}", client)
        .replace("{transaction}", transaction)
        .replace("{statement_sources}", sources)
        .replace("{statement}", statement)
}

/// What this login may see and do.
struct Login {
    sees_everything: bool,
    kills: bool,
}

/// One row of [`SESSIONS`], as read.
#[derive(Debug, Default)]
struct Listed {
    id: i16,
    login_time: Option<String>,
    login: Option<String>,
    database: Option<String>,
    program: Option<String>,
    client: Option<String>,
    session_status: Option<String>,
    request_status: Option<String>,
    command: Option<String>,
    user_process: bool,
    open_transactions: i32,
    in_state_seconds: Option<i64>,
    transaction_seconds: Option<i64>,
    blocked_by: i16,
    wait_type: Option<String>,
    wait_resource: Option<String>,
    percent_complete: Option<f64>,
    statement: Option<String>,
}

pub(crate) async fn read(client: &mut SqlClient) -> Result<ServerActivity, DriverError> {
    let login = login(client).await?;
    let rows = client
        .simple_query(sessions_query(&login))
        .await
        .map_err(connection_error)?
        .into_first_result()
        .await
        .map_err(connection_error)?;
    // A session running several requests (MARS) is listed once, with its first.
    let mut seen = HashSet::new();
    let sessions = rows
        .iter()
        .map(listed)
        .filter(|listed| seen.insert(listed.id))
        .map(|listed| session(listed, &login))
        .collect();
    let mut restrictions = Vec::new();
    if !login.sees_everything {
        for hidden in [Hidden::OthersSessions, Hidden::OthersStatements] {
            restrictions.push(Restriction {
                hidden,
                shown_by: SEES_EVERYTHING[0].to_owned(),
            });
        }
    }
    Ok(ServerActivity {
        sessions,
        restrictions,
        others_stopped_by: (!login.kills).then(|| KILLS.to_owned()),
    })
}

async fn login(client: &mut SqlClient) -> Result<Login, DriverError> {
    let row = client
        .simple_query(format!(
            "SELECT COALESCE(HAS_PERMS_BY_NAME(NULL, NULL, '{}'), 0), \
                    COALESCE(HAS_PERMS_BY_NAME(NULL, NULL, '{}'), 0), \
                    COALESCE(HAS_PERMS_BY_NAME(NULL, NULL, '{KILLS}'), 0)",
            SEES_EVERYTHING[0], SEES_EVERYTHING[1]
        ))
        .await
        .map_err(connection_error)?
        .into_row()
        .await
        .map_err(connection_error)?;
    let has = |index| {
        row.as_ref()
            .and_then(|row| column::<i32>(row, index))
            .is_some_and(|value| value == 1)
    };
    Ok(Login {
        sees_everything: has(0) || has(1),
        kills: has(2),
    })
}

fn listed(row: &Row) -> Listed {
    let text = |index| column::<&str>(row, index).map(str::to_owned);
    Listed {
        id: column(row, 0).unwrap_or_default(),
        login_time: text(1),
        login: text(2),
        database: text(3),
        program: text(4).filter(|program| !program.is_empty()),
        client: text(5),
        session_status: text(6),
        request_status: text(7),
        command: text(8),
        user_process: column::<i32>(row, 9) == Some(1),
        open_transactions: column(row, 10).unwrap_or_default(),
        in_state_seconds: column(row, 11),
        transaction_seconds: column(row, 12),
        blocked_by: column(row, 13).unwrap_or_default(),
        wait_type: text(14),
        wait_resource: text(15).filter(|resource| !resource.is_empty()),
        percent_complete: column(row, 16),
        statement: text(17).filter(|statement| !statement.trim().is_empty()),
    }
}

impl Listed {
    fn state(&self) -> SessionState {
        if !self.user_process || self.request_status.as_deref() == Some("background") {
            return SessionState::Background;
        }
        match self.request_status {
            Some(_) if self.blocked_by > 0 => SessionState::Waiting,
            Some(_) => SessionState::Active,
            None if self.open_transactions > 0 => SessionState::IdleInTransaction,
            None => SessionState::Idle,
        }
    }

    /// The server's words: the request's status and command, or the session's status.
    fn engine_state(&self) -> Option<String> {
        match (&self.request_status, &self.command) {
            (Some(status), Some(command)) => Some(format!("{status}: {command}")),
            (Some(status), None) => Some(status.clone()),
            (None, _) => self.session_status.clone(),
        }
    }

    /// `KILL` takes no variables, so the session's login time is checked first, in the same
    /// batch: a session id reused since the read (SQL Server reuses them within seconds) no
    /// longer matches, and nothing is killed. The batch answers 1 or 0 to say which.
    fn stops(&self, login: &Login, state: SessionState) -> Vec<SessionStop> {
        let Some(login_time) = &self.login_time else {
            return Vec::new();
        };
        if !login.kills || state == SessionState::Background {
            return Vec::new();
        }
        let id = self.id;
        let login_time = login_time.replace('\'', "''");
        vec![SessionStop {
            kind: StopKind::EndSession,
            statement: format!(
                "IF EXISTS (SELECT 1 FROM sys.dm_exec_sessions WHERE session_id = {id} \
                 AND login_time = CONVERT(datetime, '{login_time}', 126)) \
                 BEGIN KILL {id}; SELECT 1; END ELSE SELECT 0;"
            ),
        }]
    }

    fn extra(&self) -> Vec<(String, String)> {
        let mut extra = Vec::new();
        if let Some(percent) = self.percent_complete.filter(|percent| *percent > 0.0) {
            extra.push(("Percent complete".to_owned(), format!("{percent:.0} %")));
        }
        extra
    }
}

fn session(listed: Listed, login: &Login) -> ServerSession {
    let state = listed.state();
    let running = matches!(state, SessionState::Active | SessionState::Waiting);
    let lock = listed
        .wait_type
        .as_deref()
        .and_then(|wait| wait.strip_prefix(LOCK_WAIT))
        .map(|mode| LockWait {
            object: listed.wait_resource.clone(),
            mode: Some(mode.to_owned()),
        });
    ServerSession {
        id: SessionId::Number(i64::from(listed.id)),
        stops: listed.stops(login, state),
        engine_state: listed.engine_state(),
        extra: listed.extra(),
        user: listed.login,
        database: listed.database,
        application: listed.program,
        client: listed.client,
        state,
        in_state_for: seconds(listed.in_state_seconds),
        transaction_for: seconds(listed.transaction_seconds),
        statement: listed.statement.map(|text| SessionStatement {
            text,
            running,
            cut_short: false,
        }),
        wait: listed.wait_type.filter(|_| running),
        lock,
        blocked_by: (listed.blocked_by > 0)
            .then(|| SessionId::Number(i64::from(listed.blocked_by)))
            .into_iter()
            .collect(),
    }
}

/// Sends the kill [`read`] wrote, and reads its 1 or 0.
pub(crate) async fn stop(
    client: &mut SqlClient,
    stop: &SessionStop,
) -> Result<StopOutcome, DriverError> {
    let row = client
        .simple_query(&stop.statement)
        .await
        .map_err(|error| statement_error(error, &stop.statement))?
        .into_row()
        .await
        .map_err(|error| statement_error(error, &stop.statement))?;
    let killed = row.as_ref().and_then(|row| column::<i32>(row, 0)) == Some(1);
    Ok(if killed {
        StopOutcome::Stopped
    } else {
        StopOutcome::AlreadyEnded
    })
}

pub(crate) async fn own_session(client: &mut SqlClient) -> Result<SessionId, DriverError> {
    let row = client
        .simple_query("SELECT CAST(@@SPID AS int)")
        .await
        .map_err(connection_error)?
        .into_row()
        .await
        .map_err(connection_error)?;
    let id = row
        .as_ref()
        .and_then(|row| column::<i32>(row, 0))
        .ok_or(DriverError::Unsupported(
            "The server didn't say its session id.",
        ))?;
    Ok(SessionId::Number(i64::from(id)))
}

fn seconds(seconds: Option<i64>) -> Option<Duration> {
    seconds
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_session(request: Option<&str>) -> Listed {
        Listed {
            id: 52,
            login_time: Some("2026-11-25T09:12:03.483".to_owned()),
            login: Some("app".to_owned()),
            session_status: Some("sleeping".to_owned()),
            request_status: request.map(str::to_owned),
            command: request.map(|_| "UPDATE".to_owned()),
            user_process: true,
            ..Listed::default()
        }
    }

    const SYSADMIN: Login = Login {
        sees_everything: true,
        kills: true,
    };

    #[test]
    fn maps_sessions_and_requests_to_dbrovers_words() {
        let blocked = Listed {
            blocked_by: 51,
            ..user_session(Some("suspended"))
        };
        let holding = Listed {
            open_transactions: 1,
            ..user_session(None)
        };
        let system = Listed {
            user_process: false,
            ..user_session(Some("background"))
        };
        let cases = [
            (user_session(Some("running")), SessionState::Active),
            (blocked, SessionState::Waiting),
            (holding, SessionState::IdleInTransaction),
            (user_session(None), SessionState::Idle),
            (system, SessionState::Background),
        ];
        for (listed, expected) in cases {
            assert_eq!(listed.state(), expected, "{listed:?}");
        }
    }

    #[test]
    fn kills_only_the_session_that_was_listed_and_never_cancels() {
        let stops = user_session(Some("running")).stops(&SYSADMIN, SessionState::Active);

        assert_eq!(
            stops,
            [SessionStop {
                kind: StopKind::EndSession,
                statement: "IF EXISTS (SELECT 1 FROM sys.dm_exec_sessions WHERE session_id = 52 \
                            AND login_time = CONVERT(datetime, '2026-11-25T09:12:03.483', 126)) \
                            BEGIN KILL 52; SELECT 1; END ELSE SELECT 0;"
                    .to_owned(),
            }]
        );
        let without = Login {
            sees_everything: true,
            kills: false,
        };
        assert!(
            user_session(None)
                .stops(&without, SessionState::Idle)
                .is_empty()
        );
    }

    #[test]
    fn a_lock_wait_names_its_mode_and_resource_and_its_blocker() {
        let waiting = Listed {
            blocked_by: 51,
            wait_type: Some("LCK_M_U".to_owned()),
            wait_resource: Some("KEY: 5:72057594043236352 (8194443284a0)".to_owned()),
            percent_complete: Some(0.0),
            ..user_session(Some("suspended"))
        };

        let session = session(waiting, &SYSADMIN);

        assert_eq!(session.blocked_by, [SessionId::Number(51)]);
        assert_eq!(
            session.lock.map(|lock| lock.to_string()).as_deref(),
            Some("U on KEY: 5:72057594043236352 (8194443284a0)")
        );
        assert_eq!(session.engine_state.as_deref(), Some("suspended: UPDATE"));
        assert!(session.extra.is_empty());
    }
}
