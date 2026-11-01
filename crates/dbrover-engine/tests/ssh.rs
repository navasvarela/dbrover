//! Connecting through the `ssh` compose service to its `postgres` service. Skipped unless
//! `DBROVER_TEST_SSH_URL` and `DBROVER_TEST_SSH_POSTGRES_URL` are set (see test-servers.env).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{MemorySecrets, SshTestServer, TestEngine, ssh_test_server};
use dbrover_core::ChildrenQuery;
use dbrover_core::{CatalogPath, DataSource, DataSourceId, SshLogin};
use dbrover_engine::{
    Command, ConsoleId, Event, PasswordKept, PasswordNeeded, PasswordUpdate, RunId, RunOrigin,
    RunOutcome, RunPurpose, ScanApproval, SessionState, TransactionMode,
};
use dbrover_store::Secret;

/// An engine whose `known_hosts` holds `known_hosts`, with `data_source` saved.
struct Machine {
    engine: TestEngine,
    secrets: Arc<MemorySecrets>,
    _dir: tempfile::TempDir,
}

fn machine(known_hosts: &str, data_source: &DataSource) -> Machine {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("known_hosts"), known_hosts).unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::clone(&secrets));
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Set(server_password()),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    Machine {
        engine,
        secrets,
        _dir: dir,
    }
}

fn server_password() -> dbrover_core::Password {
    ssh_test_server()
        .map(|server| server.postgres.password)
        .unwrap_or_else(|| dbrover_core::Password::new(""))
}

impl Machine {
    /// Connects the explorer and returns the state it settles in.
    fn connect(&mut self, id: DataSourceId) -> SessionState {
        self.engine.send(Command::Connect(id));
        assert_eq!(self.engine.expect_session(id), SessionState::Connecting);
        self.engine.expect_session(id)
    }

    /// Runs `SELECT 42` in a new console and returns how the run ended.
    fn run_in_console(&mut self, id: DataSourceId) -> RunOutcome {
        let console = ConsoleId(1);
        let run = RunId(1);
        self.engine.send(Command::Execute {
            console,
            data_source: id,
            run,
            statements: vec!["SELECT 42".to_owned()],
            parameters: dbrover_sql::ParameterValues::default(),
            transactions: TransactionMode::Auto,
            purpose: RunPurpose::Run,
            origin: RunOrigin::Console,
            scans: ScanApproval::Ask,
        });
        loop {
            if let Event::RunFinished {
                run: finished,
                outcome,
                ..
            } = self.engine.next_event()
                && finished == run
            {
                return outcome;
            }
        }
    }
}

fn connected(state: &SessionState) -> bool {
    matches!(state, SessionState::Connected(_))
}

#[test]
fn browses_and_queries_postgres_through_the_tunnel_with_a_key() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let data_source = server.postgres_data_source(server.key_login());
    let id = data_source.id;
    let mut machine = machine(&server.trusted_line(), &data_source);

    let state = machine.connect(id);
    assert!(connected(&state), "{state:?}");
    machine.engine.send(Command::LoadChildren {
        id,
        path: CatalogPath::root(),
        query: ChildrenQuery::default(),
    });
    match machine.engine.next_event() {
        Event::ChildrenLoaded { result, .. } => {
            let databases = result.unwrap().nodes;
            assert!(
                databases
                    .iter()
                    .any(|node| node.label == server.postgres.config.server.database),
                "{databases:?}"
            );
        }
        other => panic!("expected the databases, got {other:?}"),
    }

    assert_eq!(machine.run_in_console(id), RunOutcome::Completed);
    machine.engine.shut_down();
}

#[test]
fn asks_for_the_ssh_password_then_connects_with_it() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let data_source = server.postgres_data_source(SshLogin::Password);
    let id = data_source.id;
    let mut machine = machine(&server.trusted_line(), &data_source);

    let state = machine.connect(id);
    let SessionState::Failed(error) = &state else {
        panic!("expected a failure for want of the SSH password, got {state:?}");
    };
    assert!(error.contains("SSH password"), "{error}");
    machine.engine.expect(Event::PasswordNeeded {
        data_source: id,
        secret: Secret::Ssh,
        reason: PasswordNeeded::NoneSaved,
    });
    machine.engine.send(Command::ProvidePassword {
        data_source: id,
        secret: Secret::Ssh,
        password: server.password.clone(),
        save: true,
    });
    machine.engine.expect(Event::PasswordProvided {
        data_source: id,
        secret: Secret::Ssh,
        kept: PasswordKept::Saved,
    });

    let state = machine.connect(id);
    assert!(connected(&state), "{state:?}");
    assert_eq!(
        machine.secrets.saved(id, Secret::Ssh),
        Some(server.password.clone())
    );
    assert_eq!(
        machine.secrets.database(id),
        Some(server.postgres.password.clone())
    );
    machine.engine.shut_down();
}

#[test]
fn tests_a_connection_with_the_ssh_password_typed_in_the_dialog() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let data_source = server.postgres_data_source(SshLogin::Password);
    let mut machine = machine(&server.trusted_line(), &data_source);

    machine.engine.send(Command::TestConnection {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Set(server.password.clone()),
    });

    match machine.engine.next_event() {
        Event::ConnectionTested { id, result } => {
            assert_eq!(id, data_source.id);
            assert!(result.is_ok(), "{result:?}");
        }
        other => panic!("expected the test's outcome, got {other:?}"),
    }
    assert_eq!(machine.secrets.saved(data_source.id, Secret::Ssh), None);
    machine.engine.shut_down();
}

/// A server whose host key isn't trusted yet, or no longer is, isn't connected to.
fn refused(server: &SshTestServer, known_hosts: &str) -> String {
    let data_source = server.postgres_data_source(server.key_login());
    let id = data_source.id;
    let mut machine = machine(known_hosts, &data_source);
    let state = machine.connect(id);
    machine.engine.shut_down();
    match state {
        SessionState::Failed(error) => error,
        other => panic!("expected the host key to be refused, got {other:?}"),
    }
}

#[test]
fn refuses_an_unknown_or_changed_host_key() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let another_key = std::fs::read_to_string(common::docker_ssh("authorized_keys")).unwrap();
    let another_key = another_key.lines().next().unwrap();

    let unknown = refused(&server, "");
    let changed = refused(
        &server,
        &format!("[{}]:{} {another_key}\n", server.host, server.port),
    );

    assert!(unknown.contains("is not known yet"), "{unknown}");
    assert!(unknown.contains("SHA256:"), "{unknown}");
    assert!(changed.contains("has changed"), "{changed}");
    assert!(changed.contains("refusing to connect"), "{changed}");
}

#[test]
fn trusts_an_unknown_host_key_when_asked_then_connects() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let data_source = server.postgres_data_source(server.key_login());
    let id = data_source.id;
    let mut machine = machine("", &data_source);

    let state = machine.connect(id);
    assert!(matches!(state, SessionState::Failed(_)), "{state:?}");
    let key = match machine.engine.next_event() {
        Event::HostKeyUnknown { data_source, key } if data_source == id => key,
        other => panic!("expected the unknown host key, got {other:?}"),
    };
    assert!(key.fingerprint().starts_with("SHA256:"), "{key:?}");
    machine.engine.send(Command::TrustHostKey {
        data_source: id,
        key,
    });
    machine.engine.expect(Event::HostKeyTrusted {
        data_source: id,
        result: Ok(()),
    });

    let state = machine.connect(id);
    assert!(connected(&state), "{state:?}");
    let known_hosts = std::fs::read_to_string(machine._dir.path().join("known_hosts")).unwrap();
    assert_eq!(known_hosts, server.trusted_line());
    machine.engine.shut_down();
}
