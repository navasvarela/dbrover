//! `search_data` over MCP (W16), driven as a client would, on a SQLite file: a value found in
//! two tables with their keys and rows, a table with no column that can hold it, a whole value,
//! the project's row limit across tables, and the search recorded once in the history.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::num::NonZeroU32;

use dbrover_core::{ConnectionConfig, ProjectId, SqliteConfig};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::{Value, json};
use support::{SLUG, Server, data_source, ok};

fn main() {
    support::run("search", finds_a_value_across_tables);
}

const SHOP: &str = "
    CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
    INSERT INTO customers VALUES (1, 'Kate O''Brien', 'kate@example.com'),
                                 (2, 'Liam Hale', 'liam@example.org');
    CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
    INSERT INTO notes VALUES (88, 'called O''Brien about the refund'),
                             (89, 'O''Brien again');
    CREATE TABLE amounts (id INTEGER PRIMARY KEY, total INTEGER);
    INSERT INTO amounts VALUES (1, 300);
";

async fn serve(
    shop: &dbrover_core::DataSource,
    project: ProjectId,
    overrides: McpOverrides,
) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut served = Project::new(
        project,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    served.mcp_enabled = true;
    served.mcp = overrides;
    support::write_files(
        dir.path(),
        std::slice::from_ref(shop),
        Projects {
            projects: vec![served],
            ..Projects::default()
        },
        &HashMap::new(),
    );
    (Server::start(dir.path()).await, dir)
}

fn tables(reply: &Value, key: &str) -> Vec<String> {
    reply[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|table| {
            table
                .get("table")
                .unwrap_or(table)
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

async fn finds_a_value_across_tables() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("shop.sqlite");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(SHOP)
        .unwrap();
    let project = ProjectId::generate();
    let shop = data_source(
        "shop",
        project,
        ConnectionConfig::Sqlite(SqliteConfig {
            path,
            read_only: false,
        }),
    );

    let (server, dir) = serve(&shop, project, McpOverrides::default()).await;
    assert!(
        server
            .tools()
            .await
            .iter()
            .any(|tool| tool == "search_data")
    );
    let found = ok(
        &server.client,
        "search_data",
        json!({ "data_source": "shop", "value": "o'brien" }),
    )
    .await;
    assert_eq!(
        tables(&found, "matched"),
        ["main.customers", "main.notes"],
        "{found}"
    );
    assert_eq!(
        tables(&found, "no_column_can_hold_the_value"),
        ["main.amounts"],
        "{found}"
    );
    let customers = &found["matched"][0];
    assert_eq!(customers["key"], json!(["id"]), "{found}");
    assert_eq!(
        customers["columns"],
        json!(["id", "name", "email"]),
        "{found}"
    );
    assert_eq!(
        customers["rows"],
        json!([["1", "Kate O'Brien", "kate@example.com"]]),
        "{found}"
    );

    let whole = ok(
        &server.client,
        "search_data",
        json!({ "data_source": "shop", "value": "o'brien again", "match": "whole" }),
    )
    .await;
    assert_eq!(tables(&whole, "matched"), ["main.notes"], "{whole}");
    assert_eq!(tables(&whole, "no_match"), ["main.customers"], "{whole}");
    server.stop().await;

    let history = rusqlite::Connection::open(dir.path().join("history.sqlite")).unwrap();
    let recorded: i64 = history
        .query_row(
            "SELECT count(*) FROM history WHERE source = 'mcp' AND sql LIKE 'SELECT %'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 2, "each search recorded once");

    // A row limit of 2 is spent by the first table's row and half the second's.
    let two_rows = McpOverrides {
        max_rows: NonZeroU32::new(2),
        ..McpOverrides::default()
    };
    let (server, _dir) = serve(&shop, project, two_rows).await;
    let capped = ok(
        &server.client,
        "search_data",
        json!({ "data_source": "shop", "value": "o'brien", "tables": ["main.notes", "main.customers"] }),
    )
    .await;
    assert_eq!(capped["row_limit_reached"], true, "{capped}");
    let rows: usize = capped["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|table| table["rows"].as_array().unwrap().len())
        .sum();
    assert_eq!(rows, 2, "{capped}");
    server.stop().await;
}
