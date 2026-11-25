//! What a server is doing: its sessions, who waits on whose locks, and the statements that
//! stop them (W15, `docs/server-activity.md`).
//!
//! A driver reads a [`ServerActivity`] with [`Connection::server_activity`]. Each session it
//! lists carries the statements that would stop it, written by the driver at that moment with
//! what identifies the session (its start time, where ids are reused), so the confirmation
//! shows exactly what will be sent and nothing but a listed session is ever stopped.
//!
//! [`Connection::server_activity`]: crate::Connection::server_activity

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// How the server names a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SessionId {
    /// PostgreSQL's pid, MySQL's and MariaDB's thread id, SQL Server's session id.
    Number(i64),
    /// ClickHouse's query id: over HTTP each query is a request of its own, with no session
    /// around it.
    Query(String),
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::Query(id) => f.write_str(id),
        }
    }
}

/// What a session is doing, in DbRover's words, mapped from each engine's own so the same word
/// means the same thing on every engine. The engine's word is kept in
/// [`ServerSession::engine_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum SessionState {
    /// Running a statement.
    Active,
    /// Running a statement that waits on another session's lock.
    Waiting,
    /// Nothing running, with a transaction open: the usual blocker.
    IdleInTransaction,
    #[default]
    Idle,
    /// The server's own work: autovacuum, replication, the event scheduler, system sessions.
    Background,
    /// The login may not see what the session is doing (another role's session on
    /// PostgreSQL without `pg_read_all_stats`).
    Hidden,
}

impl SessionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::IdleInTransaction => "idle in transaction",
            Self::Idle => "idle",
            Self::Background => "background",
            Self::Hidden => "hidden",
        }
    }

    /// Whether the session is doing nothing at all: no statement and no open transaction.
    pub fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

/// The statement a session is running, or the last one it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatement {
    pub text: String,
    /// `false` for the last statement of a session that is running nothing now.
    pub running: bool,
    /// The server keeps only the start of a long statement (PostgreSQL's
    /// `track_activity_query_size`), and this one is as long as that.
    pub cut_short: bool,
}

/// The lock a waiting session waits for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LockWait {
    /// What is locked, as the server names it: a table (`public.orders`), or the transaction a
    /// row lock waits on (`transaction 1234`).
    pub object: Option<String>,
    /// The mode asked for (`ShareLock`, `X`, `RowExclusiveLock`).
    pub mode: Option<String>,
}

impl fmt::Display for LockWait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.mode, &self.object) {
            (Some(mode), Some(object)) => write!(f, "{mode} on {object}"),
            (Some(mode), None) => f.write_str(mode),
            (None, Some(object)) => f.write_str(object),
            (None, None) => f.write_str("a lock"),
        }
    }
}

/// How a session is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopKind {
    /// Stops the running statement and keeps the session.
    CancelQuery,
    /// Ends the session: the server rolls back its open transaction and releases its locks.
    EndSession,
}

impl StopKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::CancelQuery => "Cancel query",
            Self::EndSession => "Kill",
        }
    }
}

/// A statement that stops one listed session, written by the driver when it read the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStop {
    pub kind: StopKind,
    /// Sent as it is. It names the session with what identifies it (its start time, where the
    /// server reuses ids), so a session that has ended since is never confused with one that
    /// took its id.
    pub statement: String,
}

/// What sending a [`SessionStop`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The server took the request. Ending a session can take a moment, or a long rollback
    /// on SQL Server, so the session may still be listed for a while.
    Stopped,
    /// The session, or the statement Cancel was for, had already ended; nothing was stopped.
    AlreadyEnded,
}

/// One session on the server, as the login may see it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerSession {
    pub id: SessionId,
    pub user: Option<String>,
    pub database: Option<String>,
    pub application: Option<String>,
    /// The client's address, as the server reports it.
    pub client: Option<String>,
    pub state: SessionState,
    /// The engine's own word for what the session is doing (`idle in transaction (aborted)`,
    /// `Sleep`, `suspended`), where it says more than [`ServerSession::state`].
    pub engine_state: Option<String>,
    /// How long the session has been in its state: for an active one, how long its statement
    /// has run.
    pub in_state_for: Option<Duration>,
    /// How long its open transaction has been open.
    pub transaction_for: Option<Duration>,
    pub statement: Option<SessionStatement>,
    /// What the session waits on, as the engine names it (`Lock: transactionid`, `LCK_M_U`).
    pub wait: Option<String>,
    /// The lock it waits for, when it waits on another session's.
    pub lock: Option<LockWait>,
    /// The sessions holding what it waits for, the first being the one it is drawn under.
    pub blocked_by: Vec<SessionId>,
    /// Facts only this engine has (rows read, memory, percent complete), as label and value.
    pub extra: Vec<(String, String)>,
    /// What stops the session, as this login may: empty where it may not stop it, or where
    /// there is nothing to stop (no Cancel for a session running nothing).
    pub stops: Vec<SessionStop>,
}

impl ServerSession {
    pub fn stop(&self, kind: StopKind) -> Option<&SessionStop> {
        self.stops.iter().find(|stop| stop.kind == kind)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::Number(0)
    }
}

/// What a login without a privilege doesn't see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hidden {
    /// Other users' sessions aren't listed at all.
    OthersSessions,
    /// Other users' sessions are listed, but not what they run.
    OthersStatements,
    /// Who blocks whom can't be read.
    Blocking,
}

/// Part of the server the login can't see, and what would show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    pub hidden: Hidden,
    /// The privilege, role or grant that shows it (`pg_read_all_stats`, `PROCESS`).
    pub shown_by: String,
}

/// What the server was doing when it was read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerActivity {
    pub sessions: Vec<ServerSession>,
    /// What this login can't see. Empty when nothing was hidden from it.
    pub restrictions: Vec<Restriction>,
    /// What would let this login stop other users' sessions, when it may stop only its own.
    pub others_stopped_by: Option<String>,
}

/// One row of the activity list, in the order [`blocking_order`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityRow {
    /// The session's index in [`ServerActivity::sessions`].
    pub session: usize,
    /// 0 for a session drawn at the left; one more for each session it is drawn under.
    pub depth: usize,
    /// The session is drawn from a cycle of sessions waiting on each other (a deadlock the
    /// server hasn't broken yet): it is in the cycle, or waits on a session that is.
    pub in_cycle: bool,
}

/// Orders sessions for the list: each blocking chain from its root, the sessions waiting on
/// a session drawn under it, then every other session that `shown` keeps.
///
/// A session in a chain is always kept, whatever `shown` says, so an idle blocker still shows
/// when idle sessions are hidden. Chains come first, the root with the oldest transaction
/// first; other sessions follow by state (active first, idle last) and then by how long they
/// have been in it, longest first.
///
/// A session blocked by several is drawn under the first of them listed. A cycle has no root,
/// so it is drawn from the session in it with the oldest transaction, and every session drawn
/// from it is marked.
pub fn blocking_order(
    sessions: &[ServerSession],
    shown: impl Fn(&ServerSession) -> bool,
) -> Vec<ActivityRow> {
    let index_of: HashMap<&SessionId, usize> = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (&session.id, index))
        .collect();
    let parent: Vec<Option<usize>> = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            session
                .blocked_by
                .iter()
                .filter_map(|blocker| index_of.get(blocker).copied())
                .find(|&blocker| blocker != index)
        })
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); sessions.len()];
    for (index, parent) in parent.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(index);
        }
    }
    for list in &mut children {
        list.sort_by(|&a, &b| by_longest_in_state(&sessions[a], &sessions[b]));
    }

    let mut rows = Vec::with_capacity(sessions.len());
    let mut placed = vec![false; sessions.len()];
    let mut roots: Vec<usize> = (0..sessions.len())
        .filter(|&index| parent[index].is_none() && !children[index].is_empty())
        .collect();
    roots.sort_by(|&a, &b| by_oldest_transaction(&sessions[a], &sessions[b]));
    for root in roots {
        place_chain(root, false, &children, &mut placed, &mut rows);
    }
    // Whatever has a parent but wasn't reached from a root waits, through others, on itself.
    loop {
        let mut cyclic: Vec<usize> = (0..sessions.len())
            .filter(|&index| !placed[index] && parent[index].is_some())
            .collect();
        cyclic.sort_by(|&a, &b| by_oldest_transaction(&sessions[a], &sessions[b]));
        let Some(&start) = cyclic.first() else {
            break;
        };
        place_chain(start, true, &children, &mut placed, &mut rows);
    }

    let mut others: Vec<usize> = (0..sessions.len())
        .filter(|&index| !placed[index] && shown(&sessions[index]))
        .collect();
    others.sort_by(|&a, &b| {
        sessions[a]
            .state
            .cmp(&sessions[b].state)
            .then_with(|| by_longest_in_state(&sessions[a], &sessions[b]))
    });
    rows.extend(others.into_iter().map(|session| ActivityRow {
        session,
        depth: 0,
        in_cycle: false,
    }));
    rows
}

/// Places `start` and everything under it, depth first. `placed` stops a cycle from being
/// walked twice: the walk ends where it comes back to a session already placed.
fn place_chain(
    start: usize,
    in_cycle: bool,
    children: &[Vec<usize>],
    placed: &mut [bool],
    rows: &mut Vec<ActivityRow>,
) {
    let mut stack = vec![(start, 0)];
    while let Some((session, depth)) = stack.pop() {
        if placed[session] {
            continue;
        }
        placed[session] = true;
        rows.push(ActivityRow {
            session,
            depth,
            in_cycle,
        });
        // Pushed in reverse, so the first child is drawn first.
        for &child in children[session].iter().rev() {
            stack.push((child, depth + 1));
        }
    }
}

fn by_oldest_transaction(a: &ServerSession, b: &ServerSession) -> std::cmp::Ordering {
    b.transaction_for
        .cmp(&a.transaction_for)
        .then_with(|| by_longest_in_state(a, b))
}

fn by_longest_in_state(a: &ServerSession, b: &ServerSession) -> std::cmp::Ordering {
    b.in_state_for
        .cmp(&a.in_state_for)
        .then_with(|| a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: i64, state: SessionState, seconds: u64) -> ServerSession {
        ServerSession {
            id: SessionId::Number(id),
            state,
            in_state_for: Some(Duration::from_secs(seconds)),
            ..ServerSession::default()
        }
    }

    fn blocked(mut session: ServerSession, by: &[i64]) -> ServerSession {
        session.blocked_by = by.iter().copied().map(SessionId::Number).collect();
        session
    }

    fn with_transaction(mut session: ServerSession, seconds: u64) -> ServerSession {
        session.transaction_for = Some(Duration::from_secs(seconds));
        session
    }

    /// The rows as (id, depth, in a cycle), to compare at a glance.
    fn order(
        sessions: &[ServerSession],
        shown: impl Fn(&ServerSession) -> bool,
    ) -> Vec<(i64, usize, bool)> {
        blocking_order(sessions, shown)
            .into_iter()
            .map(|row| match &sessions[row.session].id {
                SessionId::Number(id) => (*id, row.depth, row.in_cycle),
                SessionId::Query(_) => panic!("only numbered sessions here"),
            })
            .collect()
    }

    fn not_idle(session: &ServerSession) -> bool {
        !session.state.is_idle()
    }

    #[test]
    fn draws_waiting_sessions_under_their_blocker_and_keeps_an_idle_blocker() {
        let sessions = [
            session(1, SessionState::Active, 30),
            blocked(session(2, SessionState::Waiting, 5), &[4]),
            session(3, SessionState::Idle, 900),
            with_transaction(session(4, SessionState::Idle, 600), 2400),
            blocked(session(5, SessionState::Waiting, 50), &[4]),
            blocked(session(6, SessionState::Waiting, 1), &[5]),
        ];

        assert_eq!(
            order(&sessions, not_idle),
            [
                (4, 0, false),
                (5, 1, false),
                (6, 2, false),
                (2, 1, false),
                (1, 0, false),
            ]
        );
    }

    #[test]
    fn puts_older_transactions_first_and_others_by_state_then_time() {
        let sessions = [
            session(1, SessionState::Idle, 9000),
            session(2, SessionState::Active, 3),
            session(3, SessionState::IdleInTransaction, 60),
            session(4, SessionState::Active, 300),
            with_transaction(session(5, SessionState::IdleInTransaction, 10), 100),
            blocked(session(6, SessionState::Waiting, 2), &[5]),
            with_transaction(session(7, SessionState::Active, 10), 500),
            blocked(session(8, SessionState::Waiting, 2), &[7]),
        ];

        assert_eq!(
            order(&sessions, |_| true),
            [
                (7, 0, false),
                (8, 1, false),
                (5, 0, false),
                (6, 1, false),
                (4, 0, false),
                (2, 0, false),
                (3, 0, false),
                (1, 0, false),
            ]
        );
    }

    #[test]
    fn draws_a_cycle_once_from_its_oldest_transaction_and_marks_it() {
        let sessions = [
            with_transaction(blocked(session(1, SessionState::Waiting, 5), &[2]), 10),
            with_transaction(blocked(session(2, SessionState::Waiting, 5), &[3]), 90),
            with_transaction(blocked(session(3, SessionState::Waiting, 5), &[1]), 40),
            blocked(session(4, SessionState::Waiting, 1), &[3]),
        ];

        assert_eq!(
            order(&sessions, |_| true),
            [(2, 0, true), (1, 1, true), (3, 2, true), (4, 3, true)]
        );
    }

    #[test]
    fn ignores_blockers_it_cannot_see_and_a_session_blocking_itself() {
        // A blocker hidden by a filter on the server, or one that ended between reads, isn't
        // in the list: the waiting session is drawn on its own. PostgreSQL never names a
        // session its own blocker, but a list read mid-change mustn't loop if it does.
        let sessions = [
            blocked(session(1, SessionState::Waiting, 5), &[99]),
            blocked(session(2, SessionState::Waiting, 5), &[2]),
        ];

        assert_eq!(order(&sessions, |_| true), [(1, 0, false), (2, 0, false)]);
    }

    #[test]
    fn under_several_blockers_a_session_is_drawn_under_the_first_listed() {
        let sessions = [
            session(1, SessionState::Active, 5),
            session(2, SessionState::Active, 5),
            blocked(session(3, SessionState::Waiting, 5), &[99, 2, 1]),
        ];

        assert_eq!(
            order(&sessions, |_| true),
            [(2, 0, false), (3, 1, false), (1, 0, false)]
        );
    }

    #[test]
    fn lock_waits_read_as_mode_on_object() {
        let lock = |mode: Option<&str>, object: Option<&str>| {
            LockWait {
                mode: mode.map(str::to_owned),
                object: object.map(str::to_owned),
            }
            .to_string()
        };

        assert_eq!(
            lock(Some("ShareLock"), Some("transaction 812")),
            "ShareLock on transaction 812"
        );
        assert_eq!(lock(Some("X"), None), "X");
        assert_eq!(lock(None, Some("public.orders")), "public.orders");
        assert_eq!(lock(None, None), "a lock");
    }
}
