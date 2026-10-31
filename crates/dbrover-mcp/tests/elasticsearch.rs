//! Elasticsearch served to an MCP client through its MCP login alone, driven as a client would,
//! against the compose test server when `DBROVER_TEST_ELASTICSEARCH_URL` is set (see
//! `test-servers.env`). OpenSearch's test server runs without its security plugin, so it has no
//! users to sign in as.
//!
//! Checks N3's exit for MCP (`docs/nosql.md`): a data source without an MCP login isn't
//! served; one whose MCP login is a user with a read-only role is, and searches an index as that
//! user; a `_delete_by_query` is refused by DbRover's check and by the server; and
//! `list_objects` lists the folders and the indices.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::Path;

use dbrover_core::{
    ConnectionConfig, ElasticsearchAuth, ElasticsearchConfig, McpLogin, Password, ProjectId,
    ServerConfig, TlsMode,
};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{
    Engine, SLUG, Server, account, data_source, failure, mcp_account, ok, refused_directly,
    run_directly, suffix,
};

/// The index this test has to itself on the shared server.
const INDEX: &str = "dbrover_mcp";

fn main() {
    support::run("elasticsearch", reads_elasticsearch_through_an_mcp_login);
}

/// The server in `DBROVER_TEST_ELASTICSEARCH_URL` (`https://user:password@host:port`), its
/// certificate checked against the test authority.
fn server_from_env() -> Option<(ServerConfig, Password)> {
    let url = std::env::var("DBROVER_TEST_ELASTICSEARCH_URL").ok()?;
    let rest = url.strip_prefix("https://")?.trim_end_matches('/');
    let (credentials, address) = rest.split_once('@')?;
    let (user, password) = credentials.split_once(':')?;
    let (host, port) = address.split_once(':')?;
    let ca_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt");
    Some((
        ServerConfig {
            host: host.to_owned(),
            port: port.parse::<u16>().ok()?.into(),
            database: String::new(),
            user: user.to_owned(),
            tls: TlsMode::VerifyFull {
                ca_file: Some(ca_file),
            },
            ssh: None,
            password_command: None,
        },
        Password::new(password),
    ))
}

async fn reads_elasticsearch_through_an_mcp_login() {
    let Some((server, password)) = server_from_env() else {
        println!("elasticsearch: skipped, DBROVER_TEST_ELASTICSEARCH_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let suffix = suffix();
    let reader = format!("mcp_reader_{suffix}");
    let role = format!("mcp_reader_role_{suffix}");
    let reader_password = Password::new(format!("reader-{suffix}"));
    let config = ConnectionConfig::Elasticsearch(ElasticsearchConfig {
        server: server.clone(),
        auth: ElasticsearchAuth::Password,
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

    // The role the manual suggests: the cluster's read-only `monitor`, and reading the indices.
    run_directly(
        &admin,
        &[
            format!("DELETE /{INDEX}?ignore_unavailable=true"),
            format!("PUT /{INDEX}/_doc/1?refresh=true\n{{ \"status\": \"open\" }}"),
            format!("PUT /{INDEX}/_doc/2?refresh=true\n{{ \"status\": \"paid\" }}"),
            format!(
                "PUT /_security/role/{role}\n{{ \"cluster\": [\"monitor\"], \"indices\": [{{ \"names\": [\"{INDEX}*\"], \"privileges\": [\"read\", \"view_index_metadata\", \"monitor\"] }}] }}"
            ),
            format!(
                "PUT /_security/user/{reader}\n{{ \"password\": \"{}\", \"roles\": [\"{role}\"] }}",
                reader_password.expose()
            ),
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
            "sql": format!("GET /{INDEX}/_search\n{{ \"query\": {{ \"term\": {{ \"status\": \"open\" }} }} }}"),
        }),
    )
    .await;
    let rows = read["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{read}");
    let hit = rows[0][0].as_str().unwrap();
    assert!(
        hit.starts_with(&format!(r#"{{"_index":"{INDEX}","_id":"1","#))
            && hit.ends_with(r#""status":"open"}"#),
        "{read}"
    );

    let delete =
        format!("POST /{INDEX}/_delete_by_query\n{{ \"query\": {{ \"match_all\": {{}} }} }}");
    for tool in ["query", "execute"] {
        let refused = failure(
            client,
            tool,
            json!({ "data_source": "guarded", "sql": delete }),
        )
        .await;
        assert_eq!(refused["kind"], "refused", "{tool}: {refused}");
    }

    let folders = ok(client, "list_objects", json!({ "data_source": "guarded" })).await;
    let indices = folders["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|object| object["name"] == "indices")
        .unwrap_or_else(|| panic!("no indices folder: {folders}"))
        .clone();
    let listed = ok(
        client,
        "list_objects",
        json!({ "data_source": "guarded", "path": indices["path"], "filter": INDEX }),
    )
    .await;
    assert_eq!(listed["objects"][0]["name"], INDEX, "{listed}");
    assert_eq!(listed["objects"][0]["kind"], "search_index", "{listed}");

    let log = server.stop().await;
    assert!(
        log.contains("not served") && log.contains("only through an MCP login"),
        "{log}"
    );

    // The server refuses the reader's write too: its role, not DbRover's check.
    let mut as_reader = guarded.clone();
    as_reader.mcp_login = None;
    if let ConnectionConfig::Elasticsearch(config) = &mut as_reader.config {
        config.server.user.clone_from(&reader);
    }
    let reader_engine = Engine {
        data_source: as_reader,
        password: Some(reader_password),
        table: String::new(),
        writing_function: None,
    };
    let refusal = refused_directly(&reader_engine, &delete).await;
    assert!(refusal.contains("unauthorized"), "{refusal}");

    run_directly(
        &admin,
        &[
            format!("DELETE /_security/user/{reader}"),
            format!("DELETE /_security/role/{role}"),
            format!("DELETE /{INDEX}"),
        ],
    )
    .await;
}
