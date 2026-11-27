//! The server's threads, the transactions and row or metadata locks they wait on, and the
//! statements that stop them, on MySQL and MariaDB (W15, `docs/server-activity.md`).

use std::collections::HashMap;
use std::time::Duration;

use dbrover_core::{
    DriverError, Hidden, LockWait, Restriction, ServerActivity, ServerSession, SessionId,
    SessionState, SessionStatement, SessionStop, StopKind, StopOutcome,
};
use mysql_async::prelude::{FromValue, Queryable};
use mysql_async::{Conn, Row};

use crate::errors::statement_error;
use crate::{Flavour, connection_error};

/// What lets a login see every thread, its transactions and the locks they wait on.
const SEES_EVERYTHING: &str = "PROCESS";
/// What lets a login stop other accounts' threads, on MySQL and on MariaDB.
const STOPS_OTHERS: [&str; 3] = ["SUPER", "CONNECTION_ADMIN", "CONNECTION ADMIN"];
/// `ER_NO_SUCH_THREAD`: the thread a `KILL` names has ended.
const NO_SUCH_THREAD: u16 = 1094;
/// `ER_SPECIFIC_ACCESS_DENIED_ERROR` and `ER_TABLEACCESS_DENIED_ERROR`: a view of the server's
/// state this login may not read.
const ACCESS_DENIED: [u16; 2] = [1227, 1142];
/// The threads of the server's own: replication, the event scheduler, InnoDB's purge.
const BACKGROUND_COMMANDS: [&str; 4] = ["Daemon", "Binlog Dump", "Binlog Dump GTID", "Connect"];
const BACKGROUND_USERS: [&str; 2] = ["event_scheduler", "system user"];
/// MySQL 8.0.22 added `performance_schema.processlist`, which doesn't hold the global mutex
/// `information_schema.PROCESSLIST` takes on every read (and which MySQL deprecates).
const PERFORMANCE_PROCESSLIST_SINCE: (u32, u32, u32) = (8, 0, 22);

/// What this login may see and do, asked with each read since a grant can change meanwhile.
struct Login {
    /// The account's user name, without its host: a login may always stop its own threads.
    user: String,
    sees_everything: bool,
    stops_others: bool,
}

/// One row of the process list, as read.
struct Thread {
    id: u64,
    user: Option<String>,
    host: Option<String>,
    database: Option<String>,
    command: Option<String>,
    seconds: Option<i64>,
    state: Option<String>,
    info: Option<String>,
    /// MariaDB's id of the running statement, which `KILL QUERY ID` stops and nothing later.
    query_id: Option<u64>,
}

pub(crate) async fn read(
    conn: &mut Conn,
    flavour: Flavour,
    version: &str,
) -> Result<ServerActivity, DriverError> {
    let login = login(conn).await?;
    let threads = threads(conn, flavour, version).await?;
    let mut restrictions = Vec::new();
    let (transactions, waits) = if login.sees_everything {
        let transactions = transactions(conn).await?;
        let waits = match lock_waits(conn, flavour).await {
            Ok(waits) => waits,
            Err(Denied) => {
                restrictions.push(Restriction {
                    hidden: Hidden::Blocking,
                    shown_by: "SELECT on the sys and performance_schema databases".to_owned(),
                });
                HashMap::new()
            }
        };
        (transactions, waits)
    } else {
        restrictions.push(Restriction {
            hidden: Hidden::OthersSessions,
            shown_by: SEES_EVERYTHING.to_owned(),
        });
        restrictions.push(Restriction {
            hidden: Hidden::Blocking,
            shown_by: SEES_EVERYTHING.to_owned(),
        });
        (HashMap::new(), HashMap::new())
    };
    let sessions = threads
        .into_iter()
        .map(|thread| {
            let transaction = transactions.get(&thread.id).copied();
            let wait = waits.get(&thread.id).cloned();
            session(thread, &login, flavour, transaction, wait)
        })
        .collect();
    Ok(ServerActivity {
        sessions,
        restrictions,
        others_stopped_by: (!login.stops_others).then(|| "CONNECTION_ADMIN or SUPER".to_owned()),
    })
}

async fn login(conn: &mut Conn) -> Result<Login, DriverError> {
    // USER_PRIVILEGES lists the account's own global privileges, by its quoted name; one that
    // comes only through a role isn't there, so such a login is told less than it may see.
    let account = "CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''', \
                   SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''')";
    let privileges: Vec<String> = conn
        .query(format!(
            "SELECT PRIVILEGE_TYPE FROM information_schema.USER_PRIVILEGES \
             WHERE GRANTEE = {account}"
        ))
        .await
        .map_err(connection_error)?;
    let user: Option<String> = conn
        .query_first("SELECT SUBSTRING_INDEX(CURRENT_USER(), '@', 1)")
        .await
        .map_err(connection_error)?;
    let has = |wanted: &str| privileges.iter().any(|privilege| privilege == wanted);
    Ok(Login {
        user: user.unwrap_or_default(),
        sees_everything: has(SEES_EVERYTHING) || has("SUPER"),
        stops_others: STOPS_OTHERS.iter().any(|privilege| has(privilege)),
    })
}

async fn threads(
    conn: &mut Conn,
    flavour: Flavour,
    version: &str,
) -> Result<Vec<Thread>, DriverError> {
    let sql = match flavour {
        Flavour::MariaDb => {
            "SELECT ID, USER, HOST, DB, COMMAND, TIME, STATE, INFO, QUERY_ID \
             FROM information_schema.PROCESSLIST"
        }
        Flavour::Mysql if version_at_least(version, PERFORMANCE_PROCESSLIST_SINCE) => {
            "SELECT ID, USER, HOST, DB, COMMAND, TIME, STATE, INFO, NULL \
             FROM performance_schema.processlist"
        }
        Flavour::Mysql => {
            "SELECT ID, USER, HOST, DB, COMMAND, TIME, STATE, INFO, NULL \
             FROM information_schema.PROCESSLIST"
        }
    };
    let rows: Vec<Row> = conn.query(sql).await.map_err(connection_error)?;
    rows.iter()
        .map(|row| {
            Ok(Thread {
                id: get::<Option<u64>>(row, 0)?.unwrap_or_default(),
                user: get(row, 1)?,
                host: get(row, 2)?,
                database: get(row, 3)?,
                command: get(row, 4)?,
                seconds: get(row, 5)?,
                state: get(row, 6)?,
                info: get(row, 7)?,
                query_id: get(row, 8)?,
            })
        })
        .collect()
}

/// How long each thread's open transaction has been open, in seconds.
async fn transactions(conn: &mut Conn) -> Result<HashMap<u64, i64>, DriverError> {
    let rows: Vec<(u64, Option<i64>)> = conn
        .query(
            "SELECT trx_mysql_thread_id, TIMESTAMPDIFF(SECOND, trx_started, NOW()) \
             FROM information_schema.INNODB_TRX",
        )
        .await
        .map_err(connection_error)?;
    Ok(rows
        .into_iter()
        .map(|(thread, seconds)| (thread, seconds.unwrap_or_default()))
        .collect())
}

/// The login may read the process list but not the views of who waits on whom.
struct Denied;

/// What each waiting thread waits for, and the threads holding it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Wait {
    blockers: Vec<u64>,
    lock: LockWait,
}

async fn lock_waits(conn: &mut Conn, flavour: Flavour) -> Result<HashMap<u64, Wait>, Denied> {
    let queries: &[&str] = match flavour {
        // Row locks, then metadata locks (an ALTER TABLE waiting for a table), both from the
        // sys schema's views over performance_schema.
        Flavour::Mysql => &[
            "SELECT waiting_pid, blocking_pid, locked_table, waiting_lock_mode \
             FROM sys.innodb_lock_waits",
            "SELECT waiting_pid, blocking_pid, \
                    CONCAT('`', object_schema, '`.`', object_name, '`'), waiting_lock_type \
             FROM sys.schema_table_lock_waits",
        ],
        // Metadata locks need the METADATA_LOCK_INFO plugin, which isn't installed by
        // default; a thread waiting on one is shown waiting, with no blocker.
        Flavour::MariaDb => &[
            "SELECT r.trx_mysql_thread_id, b.trx_mysql_thread_id, l.lock_table, l.lock_mode \
             FROM information_schema.INNODB_LOCK_WAITS w \
             JOIN information_schema.INNODB_TRX r ON r.trx_id = w.requesting_trx_id \
             JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id \
             LEFT JOIN information_schema.INNODB_LOCKS l ON l.lock_id = w.requested_lock_id",
        ],
    };
    let mut waits: HashMap<u64, Wait> = HashMap::new();
    for sql in queries {
        let rows: Vec<(u64, u64, Option<String>, Option<String>)> = match conn.query(*sql).await {
            Ok(rows) => rows,
            Err(mysql_async::Error::Server(error)) if ACCESS_DENIED.contains(&error.code) => {
                return Err(Denied);
            }
            // Anything else (the sys schema dropped, a view the server doesn't have)
            // leaves the blocking unknown rather than failing the whole read.
            Err(error) => {
                tracing::debug!("could not read lock waits: {error}");
                continue;
            }
        };
        add_waits(&mut waits, rows);
    }
    Ok(waits)
}

fn add_waits(
    waits: &mut HashMap<u64, Wait>,
    rows: Vec<(u64, u64, Option<String>, Option<String>)>,
) {
    for (waiting, blocking, table, mode) in rows {
        let wait = waits.entry(waiting).or_insert_with(|| Wait {
            blockers: Vec::new(),
            lock: LockWait {
                object: table,
                mode,
            },
        });
        if !wait.blockers.contains(&blocking) {
            wait.blockers.push(blocking);
        }
    }
}

fn session(
    thread: Thread,
    login: &Login,
    flavour: Flavour,
    transaction: Option<i64>,
    wait: Option<Wait>,
) -> ServerSession {
    let state = state(&thread, transaction.is_some(), wait.is_some());
    let stops = stops(&thread, login, flavour, state);
    let engine_state = engine_state(&thread);
    let running = matches!(state, SessionState::Active | SessionState::Waiting);
    let (blocked_by, lock) = match wait {
        Some(wait) => (
            wait.blockers
                .into_iter()
                .map(|id| SessionId::Number(id_number(id)))
                .collect(),
            Some(wait.lock),
        ),
        None => (Vec::new(), None),
    };
    ServerSession {
        id: SessionId::Number(id_number(thread.id)),
        statement: thread
            .info
            .filter(|text| !text.is_empty())
            .map(|text| SessionStatement {
                text,
                running,
                cut_short: false,
            }),
        user: thread.user,
        database: thread.database,
        application: None,
        client: thread.host,
        state,
        engine_state,
        in_state_for: seconds(thread.seconds),
        transaction_for: seconds(transaction),
        wait: None,
        lock,
        blocked_by,
        extra: Vec::new(),
        stops,
    }
}

fn state(thread: &Thread, in_transaction: bool, waits: bool) -> SessionState {
    let command = thread.command.as_deref().unwrap_or_default();
    let user = thread.user.as_deref().unwrap_or_default();
    if BACKGROUND_COMMANDS.contains(&command) || BACKGROUND_USERS.contains(&user) {
        return SessionState::Background;
    }
    let waiting_for_a_lock = thread
        .state
        .as_deref()
        .is_some_and(|state| state.starts_with("Waiting for") && state.contains("lock"));
    match command {
        "Sleep" if in_transaction => SessionState::IdleInTransaction,
        "Sleep" => SessionState::Idle,
        _ if waits || waiting_for_a_lock => SessionState::Waiting,
        _ => SessionState::Active,
    }
}

/// The server's own words: the command, and what the thread is doing within it.
fn engine_state(thread: &Thread) -> Option<String> {
    let state = thread.state.as_deref().filter(|state| !state.is_empty());
    match (thread.command.as_deref(), state) {
        (Some(command), Some(state)) => Some(format!("{command}: {state}")),
        (Some(command), None) => Some(command.to_owned()),
        (None, state) => state.map(str::to_owned),
    }
}

/// `KILL QUERY` and `KILL CONNECTION`. Thread ids only grow until the server restarts, so a
/// thread's id never names another. MariaDB's `KILL QUERY ID` stops the statement that was
/// listed and not one the thread started since; MySQL has no such guard.
fn stops(
    thread: &Thread,
    login: &Login,
    flavour: Flavour,
    state: SessionState,
) -> Vec<SessionStop> {
    let own = thread.user.as_deref() == Some(login.user.as_str());
    if state == SessionState::Background || !(own || login.stops_others) {
        return Vec::new();
    }
    let mut stops = Vec::new();
    if matches!(state, SessionState::Active | SessionState::Waiting) {
        let statement = match (flavour, thread.query_id) {
            (Flavour::MariaDb, Some(query)) => format!("KILL QUERY ID {query}"),
            _ => format!("KILL QUERY {}", thread.id),
        };
        stops.push(SessionStop {
            kind: StopKind::CancelQuery,
            statement,
        });
    }
    stops.push(SessionStop {
        kind: StopKind::EndSession,
        statement: format!("KILL CONNECTION {}", thread.id),
    });
    stops
}

pub(crate) async fn stop(conn: &mut Conn, stop: &SessionStop) -> Result<StopOutcome, DriverError> {
    match conn.query_drop(&stop.statement).await {
        Ok(()) => Ok(StopOutcome::Stopped),
        Err(mysql_async::Error::Server(error)) if error.code == NO_SUCH_THREAD => {
            Ok(StopOutcome::AlreadyEnded)
        }
        Err(error) => Err(statement_error(error, &stop.statement)),
    }
}

pub(crate) async fn own_session(conn: &mut Conn) -> Result<SessionId, DriverError> {
    let id: Option<u64> = conn
        .query_first("SELECT CONNECTION_ID()")
        .await
        .map_err(connection_error)?;
    Ok(SessionId::Number(id_number(id.unwrap_or_default())))
}

/// Thread ids are 32 bits on MySQL and 64 on MariaDB, far below where `i64` ends.
fn id_number(id: u64) -> i64 {
    i64::try_from(id).unwrap_or(i64::MAX)
}

fn seconds(seconds: Option<i64>) -> Option<Duration> {
    seconds
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map(Duration::from_secs)
}

/// Whether `version`, as `8.4.3` or `11.4.4`, is at least `wanted`.
pub(crate) fn version_at_least(version: &str, wanted: (u32, u32, u32)) -> bool {
    let mut parts = version
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().unwrap_or_default());
    let found = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    found >= wanted
}

fn get<T: FromValue>(row: &Row, column: usize) -> Result<T, DriverError> {
    match row.get_opt::<T, usize>(column) {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(DriverError::Connection(Box::new(error))),
        None => Err(DriverError::Unsupported(
            "The process list had fewer columns than asked for.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(command: &str, state: Option<&str>) -> Thread {
        Thread {
            id: 42,
            user: Some("app".to_owned()),
            host: Some("10.0.3.7:51234".to_owned()),
            database: Some("shop".to_owned()),
            command: Some(command.to_owned()),
            seconds: Some(12),
            state: state.map(str::to_owned),
            info: Some("UPDATE orders SET status = 'paid' WHERE id = 3".to_owned()),
            query_id: Some(9001),
        }
    }

    fn login(user: &str, stops_others: bool) -> Login {
        Login {
            user: user.to_owned(),
            sees_everything: true,
            stops_others,
        }
    }

    #[test]
    fn maps_commands_and_states_to_dbrovers_words() {
        let cases = [
            (thread("Sleep", None), false, false, SessionState::Idle),
            (
                thread("Sleep", None),
                true,
                false,
                SessionState::IdleInTransaction,
            ),
            (
                thread("Query", Some("updating")),
                true,
                false,
                SessionState::Active,
            ),
            (
                thread("Query", Some("updating")),
                true,
                true,
                SessionState::Waiting,
            ),
            (
                thread("Query", Some("Waiting for table metadata lock")),
                false,
                false,
                SessionState::Waiting,
            ),
            (
                thread("Daemon", None),
                false,
                false,
                SessionState::Background,
            ),
            (
                thread("Binlog Dump GTID", None),
                false,
                false,
                SessionState::Background,
            ),
        ];
        for (thread, in_transaction, waits, expected) in cases {
            assert_eq!(
                state(&thread, in_transaction, waits),
                expected,
                "{:?} {:?}",
                thread.command,
                thread.state
            );
        }
    }

    #[test]
    fn writes_kills_and_guards_mariadbs_cancel_by_its_query_id() {
        let running = thread("Query", Some("updating"));
        let statements = |flavour| -> Vec<String> {
            stops(
                &running,
                &login("app", false),
                flavour,
                SessionState::Active,
            )
            .into_iter()
            .map(|stop| stop.statement)
            .collect()
        };
        assert_eq!(
            statements(Flavour::Mysql),
            ["KILL QUERY 42", "KILL CONNECTION 42"]
        );
        assert_eq!(
            statements(Flavour::MariaDb),
            ["KILL QUERY ID 9001", "KILL CONNECTION 42"]
        );
        let idle = stops(
            &thread("Sleep", None),
            &login("app", false),
            Flavour::Mysql,
            SessionState::Idle,
        );
        assert_eq!(idle.len(), 1, "only Kill for a thread running nothing");
    }

    #[test]
    fn offers_stops_for_the_logins_own_threads_unless_it_may_stop_others() {
        let running = thread("Query", None);
        let offered = |login: &Login| {
            !stops(&running, login, Flavour::Mysql, SessionState::Active).is_empty()
        };
        assert!(offered(&login("app", false)));
        assert!(!offered(&login("report", false)));
        assert!(offered(&login("report", true)));
        assert!(
            stops(
                &thread("Daemon", None),
                &login("root", true),
                Flavour::Mysql,
                SessionState::Background
            )
            .is_empty()
        );
    }

    #[test]
    fn keeps_every_blocker_of_a_waiting_thread_once() {
        let mut waits = HashMap::new();
        let row = |blocking| {
            (
                7,
                blocking,
                Some("`shop`.`orders`".to_owned()),
                Some("X,REC_NOT_GAP".to_owned()),
            )
        };
        add_waits(&mut waits, vec![row(3), row(5), row(3)]);

        let wait = &waits[&7];
        assert_eq!(wait.blockers, [3, 5]);
        assert_eq!(wait.lock.to_string(), "X,REC_NOT_GAP on `shop`.`orders`");
    }

    #[test]
    fn says_the_servers_words() {
        assert_eq!(
            engine_state(&thread("Query", Some("updating"))).as_deref(),
            Some("Query: updating")
        );
        assert_eq!(
            engine_state(&thread("Sleep", Some(""))).as_deref(),
            Some("Sleep")
        );
    }

    #[test]
    fn compares_versions() {
        assert!(version_at_least("8.4.3", (8, 0, 22)));
        assert!(version_at_least("8.0.22", (8, 0, 22)));
        assert!(!version_at_least("8.0.21", (8, 0, 22)));
        assert!(!version_at_least("5.7.44-log", (8, 0, 22)));
    }
}
