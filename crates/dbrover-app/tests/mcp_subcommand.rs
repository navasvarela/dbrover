//! `dbrover mcp` as an MCP client starts it: the real binary, with no window, finding the
//! app's files under its home folder. The server's own behaviour is tested in dbrover-mcp;
//! this checks the command line around it.
//!
//! Unix only: the home folder is moved by setting `HOME`, which Windows ignores when it
//! looks up the user's folders.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, ProjectId, SqliteConfig};
use dbrover_store::{DataSourceFile, Project, ProjectFile, ProjectSlug, Projects};
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

/// Where DbRover keeps its settings under `home`, with the XDG variables unset.
fn config_dir(home: &Path) -> PathBuf {
    home.join(".config/dbrover")
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(BINARY);
    command
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("DBROVER_CONFIG_DIR");
    command
}

/// A home folder with a SQLite data source in an MCP-enabled project `shop`.
fn home_with_a_project() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    save_a_project(home.path(), &config_dir(home.path()));
    home
}

/// The project `shop` in the settings folder `config`, its database in `home`.
fn save_a_project(home: &Path, config: &Path) {
    let database = home.join("shop.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY); INSERT INTO orders VALUES (7);",
        )
        .unwrap();
    let mut project = Project::new(
        ProjectId::generate(),
        "Shop".to_owned(),
        ProjectSlug::from_name("Shop"),
    );
    project.mcp_enabled = true;
    DataSourceFile::new(config.join("data_sources.toml"))
        .save(&[DataSource {
            id: DataSourceId::generate(),
            name: "orders db".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: Some(project.id),
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: database,
                read_only: false,
            }),
        }])
        .unwrap();
    ProjectFile::new(config.join("projects.toml"))
        .save(&Projects {
            projects: vec![project],
            ..Projects::default()
        })
        .unwrap();
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn refuses_bad_arguments_and_unknown_projects_without_touching_stdout() {
    let home = home_with_a_project();
    let cases: [(&[&str], &str); 3] = [
        (&["mcp"], "dbrover mcp needs --project <slug>"),
        (&["mcp", "--projcet", "shop"], "unknown argument"),
        (&["mcp", "--project", "nope"], "the projects are: shop"),
    ];
    for (args, expected) in cases {
        let output = run(home.path(), args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?} wrote to stdout");
    }
}

/// Asking for a project that doesn't exist lists the ones that do, which tells which settings
/// folder the server read.
#[test]
fn finds_the_settings_folder_from_the_flag_the_variable_or_the_choice() {
    let home = tempfile::tempdir().unwrap();
    let profile = home.path().join("profile");
    save_a_project(home.path(), &profile);
    let chosen_file = home
        .path()
        .join(".local/share/dbrover/settings_folder.toml");
    let profile_flag = format!("--config-dir={}", profile.display());

    let found = |output: Output| {
        assert!(output.stdout.is_empty());
        String::from_utf8_lossy(&output.stderr).contains("the projects are: shop")
    };
    assert!(!found(run(home.path(), &["mcp", "--project", "nope"])));
    assert!(found(run(
        home.path(),
        &["mcp", "--project", "nope", &profile_flag]
    )));
    assert!(found(
        command(home.path())
            .args(["mcp", "--project", "nope"])
            .env("DBROVER_CONFIG_DIR", "~/profile")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    ));

    std::fs::create_dir_all(chosen_file.parent().unwrap()).unwrap();
    std::fs::write(&chosen_file, format!("folder = {:?}\n", profile)).unwrap();
    assert!(found(run(home.path(), &["mcp", "--project", "nope"])));
}

#[test]
fn stops_rather_than_serve_another_folder_than_the_one_chosen() {
    let home = home_with_a_project();
    let chosen_file = home
        .path()
        .join(".local/share/dbrover/settings_folder.toml");
    std::fs::create_dir_all(chosen_file.parent().unwrap()).unwrap();
    std::fs::write(&chosen_file, "folder = \"/Volumes/Backup/dbrover\"\n").unwrap();
    let cases: [(&[&str], &str); 2] = [
        (
            &["mcp", "--project", "shop"],
            "/Volumes/Backup/dbrover, isn't there",
        ),
        (
            &["mcp", "--project", "shop", "--config-dir", "profile"],
            "--config-dir must be an absolute path or start with ~",
        ),
    ];
    for (args, expected) in cases {
        let output = run(home.path(), args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?} wrote to stdout");
    }
}

#[tokio::test]
async fn serves_a_project_over_stdio_with_no_window() {
    let home = home_with_a_project();
    let mut command = tokio::process::Command::new(BINARY);
    command
        .args(["mcp", "--project", "shop"])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    let (transport, _) = TokioChildProcess::builder(command)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let client = ().serve(transport).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "describe_table",
            "explain",
            "get_definition",
            "list_data_sources",
            "list_objects",
            "query"
        ]
    );
    assert!(tools.iter().all(|tool| {
        tool.annotations
            .as_ref()
            .is_some_and(|annotations| annotations.read_only_hint == Some(true))
    }));

    let Value::Object(arguments) =
        json!({ "data_source": "orders db", "sql": "SELECT id FROM orders" })
    else {
        unreachable!("a JSON object literal");
    };
    let result = client
        .call_tool(CallToolRequestParams::new("query").with_arguments(arguments))
        .await
        .unwrap();
    let rows = &result.structured_content.unwrap()["rows"];
    assert_eq!(rows, &json!([["7"]]));
    client.cancel().await.unwrap();
}
