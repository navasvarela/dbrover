//! The report tools, driven as a client would, with a stand-in for the app's runner: which
//! reports a project sees, and `run_report` only where the project allows running them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;

use dbrover_core::{ConnectionConfig, ProjectId, SqliteConfig};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{SLUG, Server, data_source, failure, ok};

fn main() {
    support::run("reports", lists_and_runs_the_projects_reports);
}

async fn lists_and_runs_the_projects_reports() {
    for allowed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let project_id = ProjectId::generate();
        let path = dir.path().join("lake.db");
        rusqlite::Connection::open(&path).unwrap();
        let lake = data_source(
            "lake",
            project_id,
            ConnectionConfig::Sqlite(SqliteConfig {
                path,
                read_only: false,
            }),
        );
        let mut project = Project::new(
            project_id,
            "Test".to_owned(),
            ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
        );
        project.mcp_enabled = true;
        project.mcp = McpOverrides {
            reports: Some(allowed),
            ..McpOverrides::default()
        };
        support::write_files(
            dir.path(),
            &[lake],
            Projects {
                projects: vec![project],
                ..Projects::default()
            },
            &HashMap::new(),
        );

        let server = Server::start(dir.path()).await;
        let client = &server.client;
        let listed = ok(client, "list_reports", json!({})).await;
        let names: Vec<&str> = listed["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|report| report["name"].as_str().unwrap())
            .collect();
        // `elsewhere` reads a data source of no project's here.
        assert_eq!(names, ["sales", "broken"], "{listed}");
        assert_eq!(listed["may_run"], allowed, "{listed}");
        assert_eq!(listed["reports"][1]["can_run"], false, "{listed}");

        let tools = server.tools().await;
        assert_eq!(
            tools.contains(&"run_report".to_owned()),
            allowed,
            "{tools:?}"
        );
        if allowed {
            let ran = ok(
                client,
                "run_report",
                json!({ "report": "sales", "params": { "since": "2026-10-01" } }),
            )
            .await;
            assert_eq!(ran["outcome"], "finished", "{ran}");
            let written = dir.path().join("sales-2026-10-01.csv");
            assert_eq!(ran["files"][0]["path"], written.display().to_string());
            assert!(written.exists());
            let refused = failure(
                client,
                "run_report",
                json!({ "report": "elsewhere", "params": { "since": "2026-10-01" } }),
            )
            .await;
            assert!(
                refused["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("has no report \"elsewhere\"")),
                "{refused}"
            );
            let broken = failure(client, "run_report", json!({ "report": "broken" })).await;
            assert_eq!(broken["kind"], "invalid_arguments", "{broken}");
        }
        server.stop().await;
    }
}
