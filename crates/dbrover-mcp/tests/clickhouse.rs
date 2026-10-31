//! ClickHouse served to an MCP client through its own read-only setting, driven as a client
//! would, against the compose test server when `DBROVER_TEST_CLICKHOUSE_URL` is set (see
//! `test-servers.env`).
//!
//! Checks W11 for MCP (`docs/well-rounded.md`): a ClickHouse data source is served without an
//! MCP login, even in a project that allows changes, and reads; ClickHouse itself refuses a
//! query that lifts `readonly`, which DbRover's check reads as a `SELECT`; and `execute` is
//! refused, since ClickHouse has no transaction to run changes in.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;

use dbrover_core::{ClickHouseConfig, ConnectionConfig, ProjectId};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{SLUG, Server, account, data_source, failure, ok, server_from_env};

fn main() {
    support::run("clickhouse", reads_clickhouse_in_its_read_only_mode);
}

async fn reads_clickhouse_in_its_read_only_mode() {
    let Some((server, password)) = server_from_env("DBROVER_TEST_CLICKHOUSE_URL") else {
        println!("clickhouse: skipped, DBROVER_TEST_CLICKHOUSE_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let events = data_source(
        "events",
        project_id,
        ConnectionConfig::ClickHouse(ClickHouseConfig {
            server,
            read_only: false,
        }),
    );
    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    // Allowing changes would open other engines writable; ClickHouse stays read-only.
    project.mcp = McpOverrides {
        write: Some(true),
        ..McpOverrides::default()
    };
    support::write_files(
        dir.path(),
        std::slice::from_ref(&events),
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &HashMap::from([(account(&events), password)]),
    );

    let server = Server::start(dir.path()).await;
    let client = &server.client;
    let listed = ok(client, "list_data_sources", json!({})).await;
    assert_eq!(listed["data_sources"][0]["name"], "events", "{listed}");

    let read = ok(
        client,
        "query",
        json!({ "data_source": "events", "sql": "SELECT number FROM numbers(3)" }),
    )
    .await;
    assert_eq!(read["rows"], json!([["0"], ["1"], ["2"]]), "{read}");

    let lifted = failure(
        client,
        "query",
        json!({
            "data_source": "events",
            "sql": "SELECT number FROM numbers(1) SETTINGS readonly = 0",
        }),
    )
    .await;
    assert_eq!(lifted["kind"], "database", "{lifted}");
    assert!(
        lifted["message"]
            .as_str()
            .is_some_and(|message| message.contains("in readonly mode")),
        "{lifted}"
    );

    let refused = failure(
        client,
        "execute",
        json!({ "data_source": "events", "sql": "INSERT INTO mcp_events VALUES (1)" }),
    )
    .await;
    assert_eq!(refused["kind"], "refused", "{refused}");

    let log = server.stop().await;
    assert!(!log.contains("not served"), "{log}");
}
