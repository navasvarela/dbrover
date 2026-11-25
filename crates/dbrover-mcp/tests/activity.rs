//! Server activity over MCP (W15), driven as a client would, against the compose test server
//! for PostgreSQL when `DBROVER_TEST_POSTGRES_URL` is set (see `test-servers.env`).
//!
//! `server_activity` is listed only with the project's `activity` permission, and
//! `stop_session` only with `admin` too; a session holding a transaction open is listed as idle
//! in transaction, a kill of it ends it and is recorded in the history, and a read-only data
//! source refuses it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;

use dbrover_core::{ConnectionConfig, PostgresConfig, ProjectId, SafetyLevel, SessionId};
use dbrover_engine::{PasswordCommands, ProcessRunner, Tunnels, open_data_source};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects, TrustedCommands};
use serde_json::{Value, json};
use support::{FileSecrets, SLUG, Server, account, data_source, failure, ok, server_from_env};

fn main() {
    support::run("activity", lists_and_stops_sessions_as_the_project_allows);
}

/// Serves one project over `data_sources`, with `overrides`, from a new directory.
async fn serve(
    data_sources: &[dbrover_core::DataSource],
    project_id: ProjectId,
    overrides: McpOverrides,
    password: &dbrover_core::Password,
) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    project.mcp = overrides;
    let passwords: HashMap<String, dbrover_core::Password> = data_sources
        .iter()
        .map(|data_source| (account(data_source), password.clone()))
        .collect();
    support::write_files(
        dir.path(),
        data_sources,
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &passwords,
    );
    (Server::start(dir.path()).await, dir)
}

fn sessions(reply: &Value) -> &Vec<Value> {
    reply["sessions"].as_array().unwrap()
}

async fn lists_and_stops_sessions_as_the_project_allows() {
    let Some((server, password)) = server_from_env("DBROVER_TEST_POSTGRES_URL") else {
        println!("activity: skipped, DBROVER_TEST_POSTGRES_URL is not set");
        return;
    };
    let project_id = ProjectId::generate();
    let config = ConnectionConfig::Postgres(PostgresConfig { server });
    let production = data_source("production", project_id, config.clone());
    let mut archive = data_source("archive", project_id, config);
    archive.safety = SafetyLevel::ReadOnly;
    let both = [production.clone(), archive.clone()];

    // A session of the test's own, holding a transaction open.
    let secrets: std::sync::Arc<dyn dbrover_store::SecretStore> = std::sync::Arc::new(FileSecrets(
        HashMap::from([(account(&production), password.clone())]),
    ));
    let mut holder = open_data_source(
        &production,
        &secrets,
        &Tunnels::new("known_hosts".into()),
        &PasswordCommands::new(
            TrustedCommands::default(),
            std::sync::Arc::new(ProcessRunner),
        ),
    )
    .await
    .unwrap();
    for sql in ["BEGIN", "SELECT 1"] {
        holder.execute(sql).await.unwrap().finish().await.unwrap();
    }
    let Some(SessionId::Number(pid)) = holder.own_session().await.unwrap() else {
        panic!("PostgreSQL names its sessions by number");
    };
    let pid = pid.to_string();

    // Without the permission, neither tool is there.
    let (reader, _dir) = serve(&both, project_id, McpOverrides::default(), &password).await;
    let tools = reader.tools().await;
    assert!(
        !tools.iter().any(|tool| tool == "server_activity"),
        "{tools:?}"
    );
    assert!(
        !tools.iter().any(|tool| tool == "stop_session"),
        "{tools:?}"
    );
    reader.stop().await;

    // With activity alone, it lists sessions but can't stop them.
    let watching = McpOverrides {
        activity: Some(true),
        ..McpOverrides::default()
    };
    let (watcher, _dir) = serve(&both, project_id, watching, &password).await;
    let tools = watcher.tools().await;
    assert!(
        tools.iter().any(|tool| tool == "server_activity"),
        "{tools:?}"
    );
    assert!(
        !tools.iter().any(|tool| tool == "stop_session"),
        "{tools:?}"
    );
    let listed = ok(
        &watcher.client,
        "server_activity",
        json!({ "data_source": "production" }),
    )
    .await;
    let held = sessions(&listed)
        .iter()
        .find(|session| session["session"] == pid)
        .unwrap_or_else(|| panic!("the holder is listed: {listed}"));
    assert_eq!(held["state"], "idle in transaction", "{held}");
    assert_eq!(held["stops"], json!(["kill"]), "{held}");
    assert!(
        sessions(&listed)
            .iter()
            .any(|session| session["this_mcp_server"] == true),
        "{listed}"
    );
    watcher.stop().await;

    // With admin too, it stops them, except on a read-only data source.
    let stopping = McpOverrides {
        activity: Some(true),
        admin: Some(true),
        ..McpOverrides::default()
    };
    let (admin, dir) = serve(&both, project_id, stopping, &password).await;
    let refused = failure(
        &admin.client,
        "stop_session",
        json!({ "data_source": "archive", "session": pid, "stop": "kill" }),
    )
    .await;
    assert_eq!(refused["kind"], "refused", "{refused}");
    let killed = ok(
        &admin.client,
        "stop_session",
        json!({ "data_source": "production", "session": pid, "stop": "kill" }),
    )
    .await;
    assert_eq!(killed["outcome"], "stopped", "{killed}");
    let gone = failure(
        &admin.client,
        "stop_session",
        json!({ "data_source": "production", "session": pid, "stop": "kill" }),
    )
    .await;
    assert_eq!(gone["kind"], "invalid_arguments", "{gone}");
    admin.stop().await;

    let history = rusqlite::Connection::open(dir.path().join("history.sqlite")).unwrap();
    let recorded: i64 = history
        .query_row(
            "SELECT count(*) FROM history \
             WHERE source = 'mcp' AND sql LIKE 'SELECT pg_terminate_backend(pid)%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 1, "the kill is in the history");
    assert!(holder.ping().await.is_err(), "the holder's session ended");
}
