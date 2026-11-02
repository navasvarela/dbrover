//! Asking for a password on connect when none is saved, or the keychain can't be read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{MemorySecrets, TestEngine, closing_server, postgres_test_server};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, Password, PostgresConfig, ServerConfig, TlsMode,
};
use dbrover_engine::{Command, Event, PasswordKept, PasswordNeeded, PasswordUpdate, SessionState};
use dbrover_store::Secret;

/// A server that hangs up at once, so connecting fails as soon as the password is found.
fn unreachable_postgres() -> DataSource {
    postgres(ServerConfig {
        host: "127.0.0.1".to_owned(),
        port: closing_server().into(),
        database: "app".to_owned(),
        user: "juan".to_owned(),
        tls: TlsMode::Disable,
        ssh: None,
        password_command: None,
        cloud_sign_in: None,
    })
}

fn postgres(server: ServerConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "shared".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig { server }),
    }
}

fn start_with(secrets: Arc<MemorySecrets>, data_source: &DataSource) -> TestEngine {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), secrets);
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    // The engine wrote its files before answering; the directory may go.
    drop(dir);
    engine
}

/// Connects and expects the attempt to stop for want of a password.
fn expect_password_needed(engine: &mut TestEngine, id: DataSourceId, reason: &PasswordNeeded) {
    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert_eq!(
        engine.expect_session(id),
        SessionState::Failed(format!("a password is needed: {reason}"))
    );
    engine.expect(Event::PasswordNeeded {
        data_source: id,
        reason: reason.clone(),
        secret: Secret::Database,
    });
}

/// Connects with the password given, which this server refuses only by being unreachable.
fn expect_connection_refused(engine: &mut TestEngine, id: DataSourceId) {
    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    match engine.expect_session(id) {
        SessionState::Failed(error) => assert!(error.starts_with("could not connect"), "{error}"),
        other => panic!("expected a refused connection, got {other:?}"),
    }
}

#[test]
fn asks_for_a_missing_password_and_saves_it_to_the_keychain() {
    let secrets = Arc::new(MemorySecrets::default());
    let data_source = unreachable_postgres();
    let id = data_source.id;
    let mut engine = start_with(Arc::clone(&secrets), &data_source);

    expect_password_needed(&mut engine, id, &PasswordNeeded::NoneSaved);
    engine.send(Command::ProvidePassword {
        data_source: id,
        password: Password::new("typed once"),
        save: true,
        secret: Secret::Database,
    });
    engine.expect(Event::PasswordProvided {
        data_source: id,
        kept: PasswordKept::Saved,
        secret: Secret::Database,
    });

    assert_eq!(secrets.database(id), Some(Password::new("typed once")));
    expect_connection_refused(&mut engine, id);
}

#[test]
fn keeps_an_unsaved_password_for_the_session_only() {
    let secrets = Arc::new(MemorySecrets::default());
    let data_source = unreachable_postgres();
    let id = data_source.id;
    let mut engine = start_with(Arc::clone(&secrets), &data_source);

    expect_password_needed(&mut engine, id, &PasswordNeeded::NoneSaved);
    engine.send(Command::ProvidePassword {
        data_source: id,
        password: Password::new("not saved"),
        save: false,
        secret: Secret::Database,
    });
    engine.expect(Event::PasswordProvided {
        data_source: id,
        kept: PasswordKept::ForThisSession,
        secret: Secret::Database,
    });

    assert_eq!(secrets.database(id), None);
    expect_connection_refused(&mut engine, id);
}

#[test]
fn asks_when_the_keychain_is_locked_and_keeps_the_password_it_refuses() {
    let data_source = unreachable_postgres();
    let id = data_source.id;
    let mut engine = start_with(Arc::new(MemorySecrets::locked()), &data_source);

    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert!(matches!(engine.expect_session(id), SessionState::Failed(_)));
    match engine.next_event() {
        Event::PasswordNeeded {
            data_source,
            secret: Secret::Database,
            reason: PasswordNeeded::KeychainUnavailable(error),
        } if data_source == id => assert!(error.contains("the keychain is locked"), "{error}"),
        other => panic!("expected a password to be needed, got {other:?}"),
    }
    engine.send(Command::ProvidePassword {
        data_source: id,
        password: Password::new("typed once"),
        save: true,
        secret: Secret::Database,
    });
    match engine.next_event() {
        Event::PasswordProvided {
            data_source,
            secret: Secret::Database,
            kept: PasswordKept::SaveFailed(_),
        } if data_source == id => {}
        other => panic!("expected the keychain to refuse the password, got {other:?}"),
    }

    expect_connection_refused(&mut engine, id);
}

#[test]
fn the_dialogs_connection_test_never_asks() {
    let data_source = unreachable_postgres();
    let mut engine = start_with(Arc::default(), &data_source);

    engine.send(Command::TestConnection {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });

    match engine.next_event() {
        Event::ConnectionTested {
            id,
            result: Err(error),
        } if id == data_source.id => assert!(error.starts_with("could not connect"), "{error}"),
        other => panic!("expected a failed connection test, got {other:?}"),
    }
}

/// The exit test of W1: a data source imported without its password asks once, then connects.
#[test]
fn connects_to_postgres_with_the_password_given_at_the_prompt() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let secrets = Arc::new(MemorySecrets::default());
    let data_source = postgres(server.config.server);
    let id = data_source.id;
    let mut engine = start_with(Arc::clone(&secrets), &data_source);

    expect_password_needed(&mut engine, id, &PasswordNeeded::NoneSaved);
    engine.send(Command::ProvidePassword {
        data_source: id,
        password: server.password.clone(),
        save: true,
        secret: Secret::Database,
    });
    engine.expect(Event::PasswordProvided {
        data_source: id,
        kept: PasswordKept::Saved,
        secret: Secret::Database,
    });
    engine.send(Command::Connect(id));

    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert!(matches!(
        engine.expect_session(id),
        SessionState::Connected(_)
    ));
    assert_eq!(secrets.database(id), Some(server.password));
}
