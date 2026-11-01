//! DuckDB served to an MCP client from a file opened read-only, driven as a client would. No
//! test server: the file is made here, so this runs on every OS.
//!
//! Checks W11 for MCP (`docs/well-rounded.md`): a DuckDB file is served, even in a project that
//! allows changes, and reads; DuckDB itself refuses a write hidden in a function, which
//! DbRover's check reads as a `SELECT`; `execute` is refused; the server holds the file only
//! while a call runs, so another program can write it between calls, and a call fails while
//! one does; and a database in memory, which DuckDB can't open read-only, isn't served.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::Path;

use dbrover_core::{Connection, ConnectionConfig, DuckDbConfig, DuckDbDatabase, ProjectId};
use dbrover_driver_duckdb::DuckDbConnection;
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{SLUG, Server, data_source, failure, ok};

fn main() {
    support::run("duckdb", reads_a_duckdb_file_read_only);
}

/// A connection of the test's own, outside the server, as another program's would be.
async fn open_for_writing(path: &Path) -> DuckDbConnection {
    let config = DuckDbConfig {
        database: DuckDbDatabase::File {
            path: path.to_owned(),
            read_only: false,
        },
    };
    dbrover_driver_duckdb::connect(&config).await.unwrap()
}

async fn run(connection: &mut DuckDbConnection, sql: &str) {
    let cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"));
    cursor.finish().await.unwrap();
}

async fn reads_a_duckdb_file_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lake.duckdb");
    dbrover_driver_duckdb::create_database(&path).await.unwrap();
    let mut writer = open_for_writing(&path).await;
    for sql in [
        "CREATE TABLE orders (id integer)",
        "INSERT INTO orders VALUES (1), (2)",
        "CREATE SEQUENCE orders_id",
    ] {
        run(&mut writer, sql).await;
    }
    Box::new(writer).close().await.unwrap();

    let project_id = ProjectId::generate();
    let lake = data_source(
        "lake",
        project_id,
        ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::File {
                path: path.clone(),
                read_only: false,
            },
        }),
    );
    let scratch = data_source(
        "scratch",
        project_id,
        ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::InMemory,
        }),
    );
    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    // Allowing changes would open other engines writable; DuckDB stays read-only.
    project.mcp = McpOverrides {
        write: Some(true),
        ..McpOverrides::default()
    };
    support::write_files(
        dir.path(),
        &[lake, scratch],
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &HashMap::new(),
    );

    let server = Server::start(dir.path()).await;
    let client = &server.client;
    let listed = ok(client, "list_data_sources", json!({})).await;
    let names: Vec<&str> = listed["data_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|data_source| data_source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lake"], "{listed}");

    let count = json!({ "data_source": "lake", "sql": "SELECT count(*) FROM orders" });
    let read = ok(client, "query", count.clone()).await;
    assert_eq!(read["rows"], json!([["2"]]), "{read}");

    let hidden = failure(
        client,
        "query",
        json!({ "data_source": "lake", "sql": "SELECT nextval('orders_id')" }),
    )
    .await;
    assert_eq!(hidden["kind"], "database", "{hidden}");
    assert!(
        hidden["message"]
            .as_str()
            .is_some_and(|message| message.contains("read-only mode")),
        "{hidden}"
    );
    let refused = failure(
        client,
        "execute",
        json!({ "data_source": "lake", "sql": "INSERT INTO orders VALUES (3)" }),
    )
    .await;
    assert_eq!(refused["kind"], "refused", "{refused}");

    // Between calls the server doesn't hold the file, so another program can write it; while
    // that one has it open, a call can't open it.
    let mut writer = open_for_writing(&path).await;
    run(&mut writer, "INSERT INTO orders VALUES (3)").await;
    let locked = failure(client, "query", count.clone()).await;
    assert_eq!(locked["kind"], "connection", "{locked}");
    Box::new(writer).close().await.unwrap();
    let read = ok(client, "query", count).await;
    assert_eq!(read["rows"], json!([["3"]]), "{read}");

    let log = server.stop().await;
    assert!(
        log.contains("scratch") && log.contains("can't open this database read-only"),
        "{log}"
    );
}
