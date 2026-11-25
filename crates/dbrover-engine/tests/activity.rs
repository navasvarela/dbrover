//! Server activity (W15): a tab's reads on a connection of its own, DbRover's own sessions
//! named, Kill sent and recorded in the history, and the refusals: a read-only data source,
//! the tab's own session, a session the last read didn't list. Skipped unless
//! `DBROVER_TEST_POSTGRES_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{MemorySecrets, ParsedUrl, TestEngine, postgres_test_server};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, PostgresConfig, Route, SafetyLevel,
    SessionId, SessionState, StopKind, StopOutcome,
};
use dbrover_engine::{
    Activity, Command, ConsoleId, CostApproval, Event, OwnSession, PasswordUpdate, RunId,
    RunOrigin, RunOutcome, RunPurpose, TransactionMode,
};
use dbrover_store::{HistoryQuery, HistorySource, HistorySourceKind};

const CONSOLE: ConsoleId = ConsoleId(1);

fn data_source(server: &ParsedUrl, safety: SafetyLevel) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "production".to_owned(),
        color: None,
        safety,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: server.config.server.clone(),
        }),
    }
}

/// An engine with `data_source` saved, and the directory its files are in.
fn engine(server: &ParsedUrl, data_source: &DataSource) -> (TestEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(
        &dir.path().join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Set(server.password.clone()),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    (engine, dir)
}

/// The next event `pick` takes, skipping the others (sessions connecting, statements
/// starting).
fn until<T>(engine: &mut TestEngine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(engine.next_event()) {
            return found;
        }
    }
}

fn read(engine: &mut TestEngine, data_source: DataSourceId) -> Activity {
    engine.send(Command::ReadActivity(data_source));
    until(engine, |event| match event {
        Event::ActivityRead {
            data_source: read,
            result,
        } if read == data_source => Some(result.unwrap()),
        _ => None,
    })
}

/// Reads until `done` holds, as the tab's timer would.
fn read_until(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    what: &str,
    done: impl Fn(&Activity) -> bool,
) -> Activity {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let activity = read(engine, data_source);
        if done(&activity) {
            return activity;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {activity:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    session: SessionId,
    kind: StopKind,
) -> Result<StopOutcome, String> {
    engine.send(Command::StopSession {
        data_source,
        session: session.clone(),
        kind,
    });
    until(engine, |event| match event {
        Event::SessionStopped {
            session: stopped,
            result,
            ..
        } if stopped == session => Some(result),
        _ => None,
    })
}

fn run(engine: &mut TestEngine, data_source: DataSourceId, run: u64, sql: &str) -> RunOutcome {
    engine.send(Command::Execute {
        console: CONSOLE,
        data_source,
        run: RunId(run),
        statements: vec![sql.to_owned()],
        parameters: dbrover_sql::ParameterValues::default(),
        transactions: TransactionMode::Manual,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
        cost: CostApproval::Ask,
    });
    until(engine, |event| match event {
        Event::RunFinished {
            run: finished,
            outcome,
            ..
        } if finished == RunId(run) => Some(outcome),
        _ => None,
    })
}

fn own_id(activity: &Activity, own: OwnSession) -> SessionId {
    activity
        .own
        .iter()
        .find(|(_, which)| *which == own)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| panic!("{own:?} isn't named: {:?}", activity.own))
}

/// A connection of the test's own, outside the engine.
fn connect(runtime: &tokio::runtime::Runtime, server: &ParsedUrl) -> Box<dyn Connection> {
    Box::new(
        runtime
            .block_on(dbrover_driver_postgres::connect(
                &server.config,
                Some(&server.password),
                Route::Direct,
            ))
            .unwrap(),
    )
}

fn execute(runtime: &tokio::runtime::Runtime, connection: &mut dyn Connection, sql: &str) {
    runtime.block_on(async {
        let cursor = connection.execute(sql).await.unwrap();
        cursor.finish().await.unwrap();
    });
}

#[test]
fn names_a_console_holding_a_lock_kills_it_and_records_the_kill() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let table = format!(
        "w15_engine_{}",
        DataSourceId::generate().to_string().replace('-', "_")
    );
    let mut setup = connect(&runtime, &server);
    execute(
        &runtime,
        setup.as_mut(),
        &format!("CREATE TABLE {table} (id int PRIMARY KEY, n int NOT NULL)"),
    );
    execute(
        &runtime,
        setup.as_mut(),
        &format!("INSERT INTO {table} VALUES (1, 0)"),
    );
    let production = data_source(&server, SafetyLevel::Careful);
    let (mut engine, _dir) = engine(&server, &production);
    let id = production.id;

    // The console holds the row in its open transaction; another connection waits on it.
    assert_eq!(
        run(
            &mut engine,
            id,
            1,
            &format!("UPDATE {table} SET n = 1 WHERE id = 1")
        ),
        RunOutcome::Completed
    );
    let mut waiter = connect(&runtime, &server);
    let waiting_sql = format!("UPDATE {table} SET n = 2 WHERE id = 1");
    let waiting = runtime.spawn(async move {
        let result = async {
            waiter
                .execute("SET lock_timeout = '20s'")
                .await?
                .finish()
                .await?;
            waiter.execute(&waiting_sql).await?.finish().await
        }
        .await;
        (waiter, result)
    });

    let activity = read_until(
        &mut engine,
        id,
        "a session waiting on the console",
        |read| {
            read.own
                .iter()
                .find(|(_, own)| *own == OwnSession::Console(CONSOLE))
                .is_some_and(|(console, _)| {
                    read.server.sessions.iter().any(|session| {
                        session.state == SessionState::Waiting
                            && session.blocked_by.contains(console)
                    })
                })
        },
    );
    let console = own_id(&activity, OwnSession::Console(CONSOLE));
    let this_tab = own_id(&activity, OwnSession::ThisTab);
    assert_ne!(console, this_tab);
    assert_eq!(
        stop(&mut engine, id, this_tab, StopKind::EndSession),
        Err("This is the Server activity tab's own session.".to_owned())
    );
    assert!(
        stop(&mut engine, id, SessionId::Number(-1), StopKind::EndSession)
            .unwrap_err()
            .contains("wasn't in the last read"),
    );

    assert_eq!(
        stop(&mut engine, id, console.clone(), StopKind::EndSession),
        Ok(StopOutcome::Stopped)
    );
    let (_waiter, result) = runtime.block_on(waiting).unwrap();
    result.expect("the waiter goes on once the console's session has ended");

    engine.send(Command::SearchHistory(HistoryQuery {
        kind: Some(HistorySourceKind::Activity),
        ..HistoryQuery::all(id)
    }));
    let entries = until(&mut engine, |event| match event {
        Event::HistoryFound { result, .. } => Some(result.unwrap()),
        _ => None,
    });
    let [entry] = entries.as_slice() else {
        panic!("one kill recorded: {entries:#?}");
    };
    assert!(
        entry.sql.starts_with(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE pid = {console}"
        )),
        "{entry:#?}"
    );
    let HistorySource::Activity { stopped } = &entry.source else {
        panic!("recorded as Activity: {entry:#?}");
    };
    assert!(
        stopped.starts_with(&format!("Ended session {console} (")),
        "{stopped}"
    );
    assert!(stopped.contains("idle in transaction"), "{stopped}");

    engine.send(Command::CloseActivity(id));
    engine.shut_down();
    execute(&runtime, setup.as_mut(), &format!("DROP TABLE {table}"));
}

#[test]
fn a_read_only_data_source_shows_activity_but_stops_nothing() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut other = connect(&runtime, &server);
    let other_id = runtime.block_on(other.own_session()).unwrap().unwrap();
    let production = data_source(&server, SafetyLevel::ReadOnly);
    let (mut engine, _dir) = engine(&server, &production);
    let id = production.id;

    let activity = read(&mut engine, id);
    let listed = activity
        .server
        .sessions
        .iter()
        .find(|session| session.id == other_id)
        .expect("the other connection is listed");
    assert!(listed.stop(StopKind::EndSession).is_some());

    let refusal = stop(&mut engine, id, other_id, StopKind::EndSession).unwrap_err();
    assert_eq!(
        refusal,
        "Refused: \u{201c}production\u{201d} is read-only, so its sessions aren't stopped."
    );
    execute(&runtime, other.as_mut(), "SELECT 1");
    engine.shut_down();
}
