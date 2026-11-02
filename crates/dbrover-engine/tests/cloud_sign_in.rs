//! Data sources whose password is a cloud account's token (C0d). Google Cloud's sign-in is
//! tried against `dbrover-cloud`'s stand-in token endpoint, which hands out the test server's
//! password as the token: no cloud account is used. The PostgreSQL test skips unless
//! `DBROVER_TEST_POSTGRES_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, closing_server, postgres_test_server};
use dbrover_cloud::stand_in::{self, TokenEndpoint};
use dbrover_core::{
    CloudSignIn, ConnectionConfig, DataSource, DataSourceId, PasswordCommand, PostgresConfig,
    ServerConfig, TlsMode,
};
use dbrover_engine::{Command, Event, PasswordUpdate, SessionState};

fn data_source(server: ServerConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "cloud".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig { server }),
    }
}

/// A user's Application Default Credentials whose token endpoint is `endpoint`.
fn credentials_file(dir: &Path, endpoint: &TokenEndpoint) -> CloudSignIn {
    let path = dir.join("application_default_credentials.json");
    let file = serde_json::json!({
        "type": "authorized_user",
        "client_id": "client",
        "client_secret": "client-secret",
        "refresh_token": "refresh-token",
        "token_uri": endpoint.address,
    });
    std::fs::write(&path, file.to_string()).unwrap();
    CloudSignIn::Google {
        credentials_file: Some(path),
    }
}

/// The stand-in, answering every request with `token`, which expires within the cache's
/// margin so that every connection asks again.
fn endpoint(runtime: &tokio::runtime::Runtime, token: &str) -> TokenEndpoint {
    let body = serde_json::json!({ "access_token": token, "expires_in": 60 }).to_string();
    runtime
        .block_on(stand_in::token_endpoint(200, &body))
        .unwrap()
}

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

fn connect(engine: &mut TestEngine, id: DataSourceId) -> SessionState {
    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    engine.expect_session(id)
}

fn disconnect(engine: &mut TestEngine, id: DataSourceId) {
    engine.send(Command::Disconnect(id));
    assert_eq!(engine.expect_session(id), SessionState::Disconnected);
}

/// C0d's exit: the token the sign-in hands out is the password, and the next connection asks
/// for a new one. Nothing is asked of the keychain.
#[test]
fn connects_to_postgres_with_a_google_token_fetched_on_every_connection() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut endpoint = endpoint(&runtime, server.password.expose());
    let dir = tempfile::tempdir().unwrap();
    let data_source = data_source(ServerConfig {
        cloud_sign_in: Some(credentials_file(dir.path(), &endpoint)),
        ..server.config.server
    });
    let id = data_source.id;
    let mut engine = start(dir.path(), &data_source);

    for connection in 1..=2 {
        let state = connect(&mut engine, id);
        assert!(
            matches!(state, SessionState::Connected(_)),
            "connection {connection}: {state:?}"
        );
        let asked = endpoint.received.try_recv().unwrap();
        assert_eq!(asked.field("refresh_token"), Some("refresh-token"));
        disconnect(&mut engine, id);
    }
    assert!(endpoint.received.try_recv().is_err());
    engine.shut_down();
}

/// A server that hangs up at once, so connecting gets as far as the password.
fn unreachable(cloud_sign_in: CloudSignIn) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_owned(),
        port: closing_server().into(),
        database: "app".to_owned(),
        user: "reader@project.iam".to_owned(),
        tls: TlsMode::Disable,
        ssh: None,
        password_command: None,
        cloud_sign_in: Some(cloud_sign_in),
    }
}

fn failure(state: SessionState) -> String {
    match state {
        SessionState::Failed(message) => message,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_refused_sign_in_says_why_before_connecting() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let body =
        r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
    let endpoint = runtime
        .block_on(stand_in::token_endpoint(400, body))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data_source = data_source(unreachable(credentials_file(dir.path(), &endpoint)));
    let mut engine = start(dir.path(), &data_source);

    let message = failure(connect(&mut engine, data_source.id));
    assert!(message.contains("cloud account"), "{message}");
    assert!(
        message.contains("gcloud auth application-default login"),
        "{message}"
    );
    engine.shut_down();
}

#[test]
fn a_password_command_beside_a_cloud_sign_in_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let command = PasswordCommand::parse(&["printf".to_owned(), "x".to_owned()]).unwrap();
    let data_source = data_source(ServerConfig {
        password_command: Some(command),
        ..unreachable(CloudSignIn::Google {
            credentials_file: None,
        })
    });
    let mut engine = start(dir.path(), &data_source);

    let message = failure(connect(&mut engine, data_source.id));
    assert!(
        message.contains("both a password command and a cloud sign-in"),
        "{message}"
    );
    engine.shut_down();
}
