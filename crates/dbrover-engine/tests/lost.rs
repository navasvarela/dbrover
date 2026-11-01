//! Noticing that a connection is gone: a session the server ended (as a restart or an
//! administrator would) shows as lost, the explorer reconnects on its next request, and a
//! console says so. Each test works in a database of its own, so ending its sessions never
//! touches another test's. Skipped unless `DBROVER_TEST_POSTGRES_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{MemorySecrets, ParsedUrl, TestEngine, postgres_test_server};
use dbrover_core::ChildrenQuery;
use dbrover_core::{
    CatalogPath, Connection, ConnectionConfig, DataSource, DataSourceId, PostgresConfig, Route,
    SafetyLevel,
};
use dbrover_engine::{
    Command, ConsoleId, Event, PasswordUpdate, RunId, RunOrigin, RunOutcome, RunPurpose,
    ScanApproval, SessionState, TransactionMode,
};

/// A database created for one test and dropped after it, with a connection to the server's
/// own `postgres` database to end its sessions from.
struct Scratch {
    runtime: tokio::runtime::Runtime,
    admin: Option<Box<dyn Connection>>,
    name: String,
    data_source: DataSource,
}

impl Scratch {
    fn create(server: ParsedUrl) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let name = format!(
            "dbrover_lost_{}",
            DataSourceId::generate().to_string().replace('-', "_")
        );
        let mut admin_config = server.config.clone();
        admin_config.server.database = "postgres".to_owned();
        let admin: Box<dyn Connection> = Box::new(
            runtime
                .block_on(dbrover_driver_postgres::connect(
                    &admin_config,
                    Some(&server.password),
                    Route::Direct,
                ))
                .unwrap(),
        );
        let mut scratch = Self {
            runtime,
            admin: Some(admin),
            name: name.clone(),
            data_source: DataSource {
                id: DataSourceId::generate(),
                name: "scratch".to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config: ConnectionConfig::Postgres(PostgresConfig {
                    server: dbrover_core::ServerConfig {
                        database: name.clone(),
                        ..server.config.server.clone()
                    },
                }),
            },
        };
        scratch.query(&format!("CREATE DATABASE {name}"));
        scratch
    }

    /// The first value of the first row of `sql`, run on the `postgres` database.
    fn query(&mut self, sql: &str) -> Option<String> {
        let admin = self.admin.as_mut().unwrap();
        self.runtime.block_on(async {
            let mut cursor = admin.execute(sql).await.unwrap();
            let rows = cursor.next_page(1).await.unwrap();
            cursor.finish().await.unwrap();
            rows.into_iter()
                .next()
                .and_then(|row| row.into_iter().next().flatten())
        })
    }

    /// Ends every session on the scratch database, as a restart would, and waits until the
    /// server says they are gone.
    fn end_its_sessions(&mut self) {
        let name = self.name.clone();
        self.query(&format!(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '{name}'"
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        let remaining = format!("SELECT count(*) FROM pg_stat_activity WHERE datname = '{name}'");
        while self.query(&remaining).as_deref() != Some("0") {
            assert!(Instant::now() < deadline, "the sessions never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let name = self.name.clone();
        self.query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
        if let Some(admin) = self.admin.take() {
            let _ = self.runtime.block_on(admin.close());
        }
    }
}

/// An engine with the scratch data source saved, pinging before every request or not.
fn engine(scratch: &Scratch, password: &dbrover_core::Password, ping: bool) -> TestEngine {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let secrets = Arc::new(MemorySecrets::default());
    let mut engine = if ping {
        TestEngine::start_pinging_before_every_request(&file, secrets)
    } else {
        TestEngine::start(&file, secrets)
    };
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: scratch.data_source.clone(),
        password: PasswordUpdate::Set(password.clone()),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(scratch.data_source.clone()));
    // The engine wrote its files before answering; the directory may go.
    drop(dir);
    engine
}

const CONSOLE: ConsoleId = ConsoleId(1);

fn run(engine: &TestEngine, scratch: &Scratch, run: u64) {
    engine.send(Command::Execute {
        console: CONSOLE,
        data_source: scratch.data_source.id,
        run: RunId(run),
        statements: vec!["SELECT 1".to_owned()],
        parameters: dbrover_sql::ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
        scans: ScanApproval::Ask,
    });
}

/// The events until the run finishes, and how it ended.
fn until_run_finished(engine: &mut TestEngine, run: u64) -> (Vec<Event>, RunOutcome) {
    let mut events = Vec::new();
    loop {
        match engine.next_event() {
            Event::RunFinished {
                run: finished,
                outcome,
                ..
            } if finished == RunId(run) => return (events, outcome),
            other => events.push(other),
        }
    }
}

/// The driver's error, which depends on when it noticed (the server's `FATAL` message, or a
/// closed connection), without "the connection was lost", which whoever shows it adds.
fn assert_is_a_reason(reason: &str) {
    assert!(!reason.is_empty(), "no reason");
    assert!(!reason.contains("the connection was lost"), "{reason}");
}

fn lost_reason(event: &Event) -> Option<&str> {
    match event {
        Event::ConsoleSessionChanged {
            console: CONSOLE,
            state: SessionState::Lost(reason),
        }
        | Event::SessionChanged {
            state: SessionState::Lost(reason),
            ..
        } => Some(reason),
        _ => None,
    }
}

fn failure(event: &Event) -> Option<&str> {
    match event {
        Event::StatementFinished {
            result: Err(failure),
            ..
        } => Some(&failure.message),
        _ => None,
    }
}

#[test]
fn a_lost_console_runs_nothing_until_it_reconnects() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let password = server.password.clone();
    let mut scratch = Scratch::create(server);
    let mut engine = engine(&scratch, &password, false);
    run(&engine, &scratch, 1);
    assert_eq!(until_run_finished(&mut engine, 1).1, RunOutcome::Completed);

    scratch.end_its_sessions();
    run(&engine, &scratch, 2);

    let (events, outcome) = until_run_finished(&mut engine, 2);
    assert_eq!(outcome, RunOutcome::Failed);
    assert!(
        events.iter().any(|event| failure(event).is_some()),
        "{events:?}"
    );
    match engine.next_event() {
        Event::ConsoleSessionChanged {
            console: CONSOLE,
            state: SessionState::Lost(reason),
        } => assert_is_a_reason(&reason),
        other => panic!("expected the console to be lost, got {other:?}"),
    }

    // It runs nothing until asked to reconnect, then runs in a new session.
    run(&engine, &scratch, 3);
    let (events, outcome) = until_run_finished(&mut engine, 3);
    assert_eq!(outcome, RunOutcome::Failed);
    assert!(
        events
            .iter()
            .filter_map(failure)
            .any(|message| message.contains("reconnect to go on")),
        "{events:?}"
    );
    engine.send(Command::ReconnectConsole(CONSOLE));
    for expected in ["Connecting", "Connected"] {
        match engine.next_event() {
            Event::ConsoleSessionChanged {
                console: CONSOLE,
                state,
            } => assert!(format!("{state:?}").starts_with(expected), "{state:?}"),
            other => panic!("expected the console {expected}, got {other:?}"),
        }
    }
    run(&engine, &scratch, 4);
    assert_eq!(until_run_finished(&mut engine, 4).1, RunOutcome::Completed);
    engine.shut_down();
}

#[test]
fn an_idle_console_pings_first_and_runs_nothing_on_a_lost_connection() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let password = server.password.clone();
    let mut scratch = Scratch::create(server);
    let mut engine = engine(&scratch, &password, true);
    run(&engine, &scratch, 1);
    assert_eq!(until_run_finished(&mut engine, 1).1, RunOutcome::Completed);

    scratch.end_its_sessions();
    run(&engine, &scratch, 2);

    let (events, outcome) = until_run_finished(&mut engine, 2);
    assert_eq!(outcome, RunOutcome::Failed);
    let lost_at = events.iter().position(|event| lost_reason(event).is_some());
    let failed_at = events.iter().position(|event| failure(event).is_some());
    assert!(lost_at < failed_at && lost_at.is_some(), "{events:?}");
    let message = failed_at
        .and_then(|at| failure(&events[at]))
        .unwrap_or_default();
    assert!(message.contains("reconnect"), "{message}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::StatementStarted { .. })),
        "nothing ran: {events:?}"
    );
    engine.shut_down();
}

/// Connects the explorer and lists the databases, returning whether that worked.
fn browse(engine: &mut TestEngine, id: DataSourceId) -> Vec<Event> {
    engine.send(Command::LoadChildren {
        id,
        path: CatalogPath::root(),
        query: ChildrenQuery::default(),
    });
    let mut events = Vec::new();
    loop {
        let event = engine.next_event();
        let done = matches!(event, Event::ChildrenLoaded { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

fn loaded(events: &[Event]) -> bool {
    matches!(
        events.last(),
        Some(Event::ChildrenLoaded { result: Ok(_), .. })
    )
}

#[test]
fn an_idle_explorer_finds_its_connection_lost_and_reconnects_for_the_request() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let password = server.password.clone();
    let mut scratch = Scratch::create(server);
    let mut engine = engine(&scratch, &password, true);
    let id = scratch.data_source.id;
    assert!(loaded(&browse(&mut engine, id)));

    scratch.end_its_sessions();
    let events = browse(&mut engine, id);

    assert!(loaded(&events), "{events:?}");
    let states: Vec<&SessionState> = events
        .iter()
        .filter_map(|event| match event {
            Event::SessionChanged { state, .. } => Some(state),
            _ => None,
        })
        .collect();
    assert!(
        matches!(
            states.as_slice(),
            [
                SessionState::Lost(_),
                SessionState::Connecting,
                SessionState::Connected(_)
            ]
        ),
        "{states:?}"
    );
    engine.shut_down();
}

#[test]
fn an_explorer_request_that_finds_the_connection_lost_fails_then_the_next_reconnects() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let password = server.password.clone();
    let mut scratch = Scratch::create(server);
    let mut engine = engine(&scratch, &password, false);
    let id = scratch.data_source.id;
    assert!(loaded(&browse(&mut engine, id)));

    scratch.end_its_sessions();
    let failed = browse(&mut engine, id);
    assert!(
        matches!(
            failed.last(),
            Some(Event::ChildrenLoaded { result: Err(_), .. })
        ),
        "{failed:?}"
    );
    match engine.expect_session(id) {
        SessionState::Lost(reason) => assert_is_a_reason(&reason),
        other => panic!("expected the explorer to be lost, got {other:?}"),
    }

    let again = browse(&mut engine, id);
    assert!(loaded(&again), "{again:?}");
    engine.shut_down();
}
