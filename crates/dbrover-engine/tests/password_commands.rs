//! Data sources whose password comes from a command (W9). The PostgreSQL ones skip unless
//! `DBROVER_TEST_POSTGRES_URL` is set, the SSH one unless the SSH server's variables are (see
//! test-servers.env); the rest run `dbrover-test-program` on every OS.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, closing_server, postgres_test_server, ssh_test_server};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, PasswordCommand, PostgresConfig, ServerConfig,
    TlsMode,
};
use dbrover_engine::{Command, Event, PasswordUpdate, SessionState};
use dbrover_store::{TrustedCommand, TrustedCommandsFile};

const PROGRAM: &str = env!("CARGO_BIN_EXE_dbrover-test-program");

fn command(items: &[&str]) -> PasswordCommand {
    let items: Vec<String> = items.iter().map(|&item| item.to_owned()).collect();
    PasswordCommand::parse(&items).unwrap()
}

fn with_command(server: ServerConfig, command: PasswordCommand) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "token".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                password_command: Some(command),
                ..server
            },
        }),
    }
}

/// A server that hangs up at once: connecting fails, but only after the password is found.
fn unreachable(command: PasswordCommand) -> DataSource {
    with_command(
        ServerConfig {
            host: "127.0.0.1".to_owned(),
            port: closing_server().into(),
            database: "app".to_owned(),
            user: "juan".to_owned(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        },
        command,
    )
}

/// An engine with `data_source` saved and nothing in the keychain, in `dir`.
fn start(dir: &Path, data_source: &DataSource) -> TestEngine {
    let mut engine = TestEngine::start(
        &dir.join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    engine
}

/// Connects the explorer and returns the state it settles in.
fn connect(engine: &mut TestEngine, id: DataSourceId) -> SessionState {
    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    engine.expect_session(id)
}

fn disconnect(engine: &mut TestEngine, id: DataSourceId) {
    engine.send(Command::Disconnect(id));
    assert_eq!(engine.expect_session(id), SessionState::Disconnected);
}

fn trust(engine: &mut TestEngine, data_source: &DataSource, command: &PasswordCommand) {
    engine.send(Command::TrustPasswordCommand {
        data_source: data_source.id,
        command: command.clone(),
    });
    let trusted = TrustedCommand::password(data_source.id, command);
    match engine.next_event() {
        Event::TrustedCommandsLoaded(all) => assert!(all.contains(&trusted), "{all:?}"),
        other => panic!("expected the command to be trusted, got {other:?}"),
    }
}

fn connected(state: &SessionState) -> bool {
    matches!(state, SessionState::Connected(_))
}

/// W9's exit: `["printf", "dbrover"]` connects after one trust prompt, and without it the
/// second time.
#[test]
fn connects_to_postgres_after_one_trust_prompt() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    assert_eq!(server.password.expose(), "dbrover", "test-servers.env");
    let dir = tempfile::tempdir().unwrap();
    let printf = command(&["printf", "dbrover"]);
    let data_source = with_command(server.config.server, printf.clone());
    let id = data_source.id;
    let mut engine = start(dir.path(), &data_source);

    let state = connect(&mut engine, id);
    assert_eq!(
        state,
        SessionState::Failed(
            "the password command isn't trusted to run yet; connecting to the data source in the \
             DbRover app asks"
                .to_owned()
        )
    );
    engine.expect(Event::PasswordCommandUntrusted {
        data_source: id,
        command: printf.clone(),
    });
    trust(&mut engine, &data_source, &printf);
    let state = connect(&mut engine, id);
    assert!(connected(&state), "{state:?}");

    disconnect(&mut engine, id);
    let state = connect(&mut engine, id);
    assert!(connected(&state), "asked only once: {state:?}");
    engine.shut_down();

    let trusted = TrustedCommandsFile::new(dir.path().join("trusted_commands.toml"))
        .load()
        .unwrap();
    assert!(trusted.contains(&TrustedCommand::password(id, &printf)));
}

/// Every connection runs the command again, so a token that has expired is never reused: the
/// password it prints the second time is the one that counts.
#[test]
fn runs_the_command_again_on_every_connection() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, server.password.expose()).unwrap();
    let print = command(&[PROGRAM, "print-file", token.to_str().unwrap()]);
    let data_source = with_command(server.config.server, print.clone());
    let id = data_source.id;
    let mut engine = start(dir.path(), &data_source);
    trust(&mut engine, &data_source, &print);

    let state = connect(&mut engine, id);
    assert!(connected(&state), "{state:?}");
    disconnect(&mut engine, id);

    std::fs::write(&token, "expired").unwrap();
    match connect(&mut engine, id) {
        SessionState::Failed(error) => assert!(error.contains("password"), "{error}"),
        other => panic!("expected the old password to be refused, got {other:?}"),
    }
    engine.shut_down();
}

/// A command that fails shows its exit status and what it wrote to stderr, instead of
/// connecting.
#[test]
fn a_failing_command_shows_its_exit_status_and_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let fail = command(&[PROGRAM, "fail"]);
    let data_source = unreachable(fail.clone());
    let id = data_source.id;
    let mut engine = start(dir.path(), &data_source);
    trust(&mut engine, &data_source, &fail);

    match connect(&mut engine, id) {
        SessionState::Failed(error) => {
            assert!(error.starts_with("could not get the password: "), "{error}");
            // `exit status: 3` on Unix, `exit code: 3` on Windows.
            assert!(error.contains(": 3): it went wrong"), "{error}");
        }
        other => panic!("expected the command to fail, got {other:?}"),
    }
    engine.shut_down();
}

/// The dialog's test runs the command the dialog shows without asking, and trusts nothing:
/// connecting still asks.
#[test]
fn the_dialogs_test_runs_the_command_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let fail = command(&[PROGRAM, "fail"]);
    let data_source = unreachable(fail.clone());
    let mut engine = start(dir.path(), &data_source);

    engine.send(Command::TestConnection {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    match engine.next_event() {
        Event::ConnectionTested {
            id,
            result: Err(error),
        } if id == data_source.id => assert!(error.ends_with("it went wrong"), "{error}"),
        other => panic!("expected the test to run the command, got {other:?}"),
    }

    assert!(matches!(
        connect(&mut engine, data_source.id),
        SessionState::Failed(_)
    ));
    engine.expect(Event::PasswordCommandUntrusted {
        data_source: data_source.id,
        command: fail,
    });
    engine.shut_down();
}

/// Through an SSH tunnel the command runs the same way, once the tunnel is open.
#[test]
fn connects_through_an_ssh_tunnel_with_the_commands_password() {
    let Some(server) = ssh_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("known_hosts"), server.trusted_line()).unwrap();
    let printf = command(&["printf", server.postgres.password.expose()]);
    let tunnelled = server.postgres_data_source(server.key_login());
    let ConnectionConfig::Postgres(config) = tunnelled.config else {
        panic!("{tunnelled:?}");
    };
    let data_source = DataSource {
        id: tunnelled.id,
        ..with_command(config.server, printf.clone())
    };
    let mut engine = start(dir.path(), &data_source);
    trust(&mut engine, &data_source, &printf);

    let state = connect(&mut engine, data_source.id);
    assert!(connected(&state), "{state:?}");
    engine.shut_down();
}
