//! `dbrover mcp` running a report for an MCP client: the real binary, spoken to in the MCP's
//! JSON-RPC over standard input and output, on a settings folder with the examples' SQLite shop
//! in a project that allows running reports. The tools themselves are tested in dbrover-mcp
//! with a stand-in runner; this checks the app's runner: the run in the worker, its files and
//! its run kept in the history as MCP's.
//!
//! Unix only, as the worker's tests: the home folder is moved by setting `HOME`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, ProjectId, SafetyLevel, SqliteConfig,
};
use dbrover_store::{
    DataSourceFile, HistoryFile, McpOverrides, Project, ProjectFile, ProjectSlug, Projects,
    ReportName, RunOrigin, RunOutcome,
};
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
}

/// A home folder whose settings folder serves the project `shop`, with the shop's SQLite
/// database and `sales-by-category.toml`.
fn settings(home: &Path) -> PathBuf {
    let config = home.join("settings");
    let database = home.join("shop.db");
    let script = std::fs::read_to_string(examples().join("shop.sql")).unwrap();
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    let project = ProjectId::generate();
    DataSourceFile::new(config.join("data_sources.toml"))
        .save(&[DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: Some(project),
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: database,
                read_only: false,
            }),
        }])
        .unwrap();
    let mut shop = Project::new(
        project,
        "Shop".to_owned(),
        ProjectSlug::try_from("shop".to_owned()).unwrap(),
    );
    shop.mcp_enabled = true;
    shop.mcp = McpOverrides {
        reports: Some(true),
        ..McpOverrides::default()
    };
    ProjectFile::new(config.join("projects.toml"))
        .save(&Projects {
            projects: vec![shop],
            ..Projects::default()
        })
        .unwrap();
    let reports = config.join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::copy(
        examples().join("sales-by-category.toml"),
        reports.join("sales-by-category.toml"),
    )
    .unwrap();
    config
}

/// Sends `message`, and for a request, returns the answer with its id.
fn send(stdin: &mut impl Write, stdout: &mut impl BufRead, message: &Value) -> Option<Value> {
    writeln!(stdin, "{message}").unwrap();
    stdin.flush().unwrap();
    let id = message.get("id")?;
    loop {
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0, "the server ended");
        let answer: Value = serde_json::from_str(&line).unwrap();
        if answer.get("id") == Some(id) {
            return Some(answer);
        }
    }
}

#[test]
fn runs_a_report_for_an_mcp_client() {
    let home = tempfile::tempdir().unwrap();
    let config = settings(home.path());
    let mut server = Command::new(BINARY)
        .args(["mcp", "--project", "shop", "--config-dir"])
        .arg(&config)
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("DBROVER_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1" },
        },
    });
    send(&mut stdin, &mut stdout, &initialize).unwrap();
    send(
        &mut stdin,
        &mut stdout,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    let run = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "run_report",
            "arguments": {
                "report": "sales-by-category",
                "params": { "since": "2026-10-01", "until": "2026-11-01" },
            },
        },
    });
    let answer = send(&mut stdin, &mut stdout, &run).unwrap();
    let result = &answer["result"]["structuredContent"];
    assert_eq!(result["outcome"], "finished", "{answer}");
    let files: Vec<PathBuf> = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| PathBuf::from(file["path"].as_str().unwrap()))
        .collect();
    assert_eq!(files.len(), 2, "{answer}");
    for file in &files {
        assert!(file.exists(), "{}", file.display());
        assert!(file.starts_with(home.path()), "{}", file.display());
    }
    drop(stdin);
    server.wait().unwrap();

    let runs = HistoryFile::new(config.join("history.sqlite"))
        .open()
        .unwrap()
        .runs(&ReportName::new("sales-by-category").unwrap(), 5)
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Finished);
    assert_eq!(runs[0].origin, RunOrigin::Mcp);
}
