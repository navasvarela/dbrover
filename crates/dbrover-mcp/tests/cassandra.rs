//! Cassandra and ScyllaDB served to an MCP client through the MCP login alone, driven as a
//! client would, against the compose test server when `DBROVER_TEST_SCYLLA_URL` is set (see
//! `test-servers.env`), or `DBROVER_TEST_CASSANDRA_URL`, which the nightly job sets.
//!
//! Checks N4's exit for MCP (`docs/nosql.md`): a data source without an MCP login isn't served;
//! one whose MCP login is a role with only the `SELECT` permission is, and reads a table as that
//! role; an `INSERT` is refused by DbRover's check, through `query` and `execute`, and by the
//! server; and `list_objects` lists the keyspace's folders and tables.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;

use dbrover_core::{
    CassandraConfig, ConnectionConfig, McpLogin, Password, ProjectId, ServerConfig, TlsMode,
};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{
    Engine, SLUG, Server, account, data_source, failure, mcp_account, ok, refused_directly,
    run_directly, suffix,
};

/// The keyspace this test has to itself on the shared server.
const KEYSPACE: &str = "dbrover_mcp";

fn main() {
    support::run("cassandra", reads_cql_through_an_mcp_login);
}

/// The first server set, `cql://user:password@host:port`.
fn server_from_env() -> Option<(ServerConfig, Password)> {
    let url = ["DBROVER_TEST_SCYLLA_URL", "DBROVER_TEST_CASSANDRA_URL"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|url| !url.is_empty()))?;
    let rest = url.strip_prefix("cql://")?.trim_end_matches('/');
    let (credentials, address) = rest.split_once('@')?;
    let (user, password) = credentials.split_once(':')?;
    let (host, port) = address.split_once(':')?;
    Some((
        ServerConfig {
            host: host.to_owned(),
            port: port.parse::<u16>().ok()?.into(),
            database: String::new(),
            user: user.to_owned(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
        },
        Password::new(password),
    ))
}

async fn reads_cql_through_an_mcp_login() {
    let Some((server, password)) = server_from_env() else {
        println!(
            "cassandra: skipped, neither DBROVER_TEST_SCYLLA_URL nor DBROVER_TEST_CASSANDRA_URL is set"
        );
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let reader = format!("mcp_reader_{}", suffix());
    let reader_password = Password::new(format!("reader-{}", suffix()));
    let config = ConnectionConfig::Cassandra(CassandraConfig {
        server: server.clone(),
        local_datacenter: String::new(),
        statement_timeout: None,
    });

    let admin = Engine {
        data_source: data_source("unguarded", project_id, config.clone()),
        password: Some(password.clone()),
        table: String::new(),
        writing_function: None,
    };
    let mut guarded = data_source("guarded", project_id, config);
    guarded.mcp_login = Some(McpLogin {
        user: reader.clone(),
    });

    // The role the manual suggests: one that may log in, with `SELECT` alone.
    run_directly(
        &admin,
        &[
            format!("DROP KEYSPACE IF EXISTS {KEYSPACE}"),
            format!(
                "CREATE KEYSPACE {KEYSPACE} WITH replication = \
                 {{'class': 'NetworkTopologyStrategy', 'replication_factor': 1}}"
            ),
            format!("CREATE TABLE {KEYSPACE}.orders (id int PRIMARY KEY, status text)"),
            format!("INSERT INTO {KEYSPACE}.orders (id, status) VALUES (1, 'open')"),
            format!("INSERT INTO {KEYSPACE}.orders (id, status) VALUES (2, 'paid')"),
            format!(
                "CREATE ROLE {reader} WITH PASSWORD = '{}' AND LOGIN = true",
                reader_password.expose()
            ),
            format!("GRANT SELECT ON KEYSPACE {KEYSPACE} TO {reader}"),
        ],
    )
    .await;

    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    project.mcp = McpOverrides {
        write: Some(true),
        ..McpOverrides::default()
    };
    let passwords = HashMap::from([
        (account(&admin.data_source), password.clone()),
        (account(&guarded), password),
        (mcp_account(&guarded), reader_password.clone()),
    ]);
    support::write_files(
        dir.path(),
        &[admin.data_source.clone(), guarded.clone()],
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &passwords,
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
    assert_eq!(names, ["guarded"], "{listed}");

    let read = ok(
        client,
        "query",
        json!({
            "data_source": "guarded",
            "sql": format!("SELECT id, status FROM {KEYSPACE}.orders WHERE id = 1"),
        }),
    )
    .await;
    assert_eq!(read["rows"], json!([["1", "open"]]), "{read}");

    let insert = format!("INSERT INTO {KEYSPACE}.orders (id, status) VALUES (3, 'new')");
    for tool in ["query", "execute"] {
        let refused = failure(
            client,
            tool,
            json!({ "data_source": "guarded", "sql": insert }),
        )
        .await;
        assert_eq!(refused["kind"], "refused", "{tool}: {refused}");
    }
    let explained = failure(
        client,
        "explain",
        json!({ "data_source": "guarded", "sql": format!("SELECT * FROM {KEYSPACE}.orders") }),
    )
    .await;
    assert!(explained.to_string().contains("no EXPLAIN"), "{explained}");

    let keyspaces = ok(
        client,
        "list_objects",
        json!({ "data_source": "guarded", "filter": KEYSPACE }),
    )
    .await;
    let keyspace = keyspaces["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|object| object["name"] == KEYSPACE)
        .unwrap_or_else(|| panic!("no keyspace: {keyspaces}"))
        .clone();
    let folders = ok(
        client,
        "list_objects",
        json!({ "data_source": "guarded", "path": keyspace["path"] }),
    )
    .await;
    let tables = folders["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|object| object["name"] == "tables")
        .unwrap_or_else(|| panic!("no tables folder: {folders}"))
        .clone();
    let listed = ok(
        client,
        "list_objects",
        json!({ "data_source": "guarded", "path": tables["path"] }),
    )
    .await;
    assert_eq!(listed["objects"][0]["name"], "orders", "{listed}");
    assert_eq!(listed["objects"][0]["kind"], "table", "{listed}");

    let log = server.stop().await;
    assert!(
        log.contains("not served") && log.contains("only through an MCP login"),
        "{log}"
    );

    // The server refuses the reader's write too: its permissions, not DbRover's check.
    let mut as_reader = guarded.clone();
    as_reader.mcp_login = None;
    if let ConnectionConfig::Cassandra(config) = &mut as_reader.config {
        config.server.user.clone_from(&reader);
    }
    let reader_engine = Engine {
        data_source: as_reader,
        password: Some(reader_password),
        table: String::new(),
        writing_function: None,
    };
    let refusal = refused_directly(&reader_engine, &insert).await;
    assert!(refusal.to_lowercase().contains("permission"), "{refusal}");

    run_directly(
        &admin,
        &[
            format!("DROP ROLE {reader}"),
            format!("DROP KEYSPACE {KEYSPACE}"),
        ],
    )
    .await;
}
