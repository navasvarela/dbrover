//! The `chart` tool driven as a client would, over standard input and output, on the example
//! reports' SQLite shop: CH4's exit (`docs/charts-as-code.md`). A chart call returns a PNG
//! image a client shows; a data source the project doesn't serve to MCP clients is refused;
//! rows past the row limit are drawn cut, and the picture and the reply say so.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dbrover_core::{
    ConnectionConfig, ProjectId, ServerConfig, SqlServerConfig, SqliteConfig, TlsMode,
};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{Value, json};
use support::{Client, SLUG, Server, data_source, failure, ok};

/// Rows a call returns at most: fewer than the shop's eight products.
const MAX_ROWS: u32 = 5;

/// Net sales by category, four rows.
const BY_CATEGORY: &str = "SELECT p.category, SUM(l.amount) AS net \
    FROM order_lines l JOIN products p ON p.id = l.product_id \
    GROUP BY p.category ORDER BY net DESC";

fn main() {
    support::run("chart", charts_a_query_over_stdio);
}

/// The call's content: its picture, and what was charted.
async fn chart(client: &Client, arguments: Value) -> (ContentBlock, Value) {
    let Value::Object(arguments) = arguments else {
        panic!("arguments must be an object");
    };
    let result = client
        .call_tool(CallToolRequestParams::new("chart").with_arguments(arguments))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let picture = result.content.first().cloned().unwrap();
    (picture, result.structured_content.unwrap())
}

async fn charts_a_query_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let path = dir.path().join("shop.db");
    let script = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports/shop.sql"),
    )
    .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    let shop = data_source(
        "shop",
        project_id,
        ConnectionConfig::Sqlite(SqliteConfig {
            path,
            read_only: false,
        }),
    );
    // SQL Server has no read-only mode: without an MCP login of its own, MCP clients don't
    // get it.
    let erp = data_source(
        "erp",
        project_id,
        ConnectionConfig::SqlServer(SqlServerConfig {
            server: ServerConfig {
                host: "erp.internal".to_owned(),
                port: SqlServerConfig::DEFAULT_PORT.into(),
                database: "erp".to_owned(),
                user: "reader".to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        }),
    );
    let mut project = Project::new(
        project_id,
        "Test".to_owned(),
        ProjectSlug::try_from(SLUG.to_owned()).unwrap(),
    );
    project.mcp_enabled = true;
    project.mcp = McpOverrides {
        max_rows: std::num::NonZeroU32::new(MAX_ROWS),
        ..McpOverrides::default()
    };
    support::write_files(
        dir.path(),
        &[shop, erp],
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &std::collections::HashMap::new(),
    );

    let server = Server::start(dir.path()).await;
    assert!(server.tools().await.contains(&"chart".to_owned()));
    let client = &server.client;

    let (picture, charted) = chart(
        client,
        json!({
            "data_source": "shop",
            "sql": BY_CATEGORY,
            "chart": { "kind": "bars", "x": "category", "y": "net", "title": "Net sales by category" },
        }),
    )
    .await;
    let ContentBlock::Image(image) = picture else {
        panic!("a PNG image: {picture:?}");
    };
    assert_eq!(image.mime_type, "image/png");
    let png = STANDARD.decode(&image.data).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    // 800 by 450 points at 2 pixels a point, from the PNG's header.
    assert_eq!(png[16..24], [0, 0, 6, 64, 0, 0, 3, 132]);
    assert_eq!(charted["format"], "png");
    assert_eq!(charted["points"], 4, "{charted}");
    assert_eq!(charted["truncated"], false, "{charted}");

    let (picture, svg) = chart(
        client,
        json!({ "data_source": "shop", "sql": BY_CATEGORY, "format": "svg" }),
    )
    .await;
    let ContentBlock::Text(text) = picture else {
        panic!("the SVG as text: {picture:?}");
    };
    assert!(text.text.starts_with("<svg "), "{}", text.text);
    assert_eq!(svg["kind"], "bars");

    let (picture, cut) = chart(
        client,
        json!({ "data_source": "shop", "sql": "SELECT name, price FROM products ORDER BY id" }),
    )
    .await;
    let reason = "only the first 5 rows were read, the project's row limit";
    assert_eq!(cut["truncated"], true, "{cut}");
    assert_eq!(cut["truncated_reason"], reason, "{cut}");
    assert_eq!(cut["points"], 5, "{cut}");
    assert!(cut["note"].as_str().unwrap().ends_with(reason), "{cut}");
    assert!(matches!(picture, ContentBlock::Image(_)));

    let refused = failure(
        client,
        "chart",
        json!({ "data_source": "erp", "sql": "SELECT 1 AS a, 2 AS b" }),
    )
    .await;
    assert_eq!(refused["kind"], "invalid_arguments", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("the project has no data source \"erp\"; it has: shop"),
        "{refused}"
    );

    let changes = failure(
        client,
        "chart",
        json!({ "data_source": "shop", "sql": "DELETE FROM products" }),
    )
    .await;
    assert_eq!(changes["kind"], "refused", "{changes}");
    let misspelt = failure(
        client,
        "chart",
        json!({ "data_source": "shop", "sql": BY_CATEGORY, "chart": { "kind": "pie" } }),
    )
    .await;
    assert_eq!(
        misspelt["message"],
        "chart.kind: `kind` is bars, lines or scatter, not \"pie\""
    );
    let listed = ok(client, "list_data_sources", json!({})).await;
    assert_eq!(listed["data_sources"].as_array().unwrap().len(), 1);

    let log = server.stop().await;
    assert!(log.contains("not served"), "{log}");
    assert!(
        log.contains("tool=\"chart\"") || log.contains("tool=chart"),
        "{log}"
    );
    let history = rusqlite::Connection::open(dir.path().join("history.sqlite")).unwrap();
    let recorded: i64 = history
        .query_row(
            "SELECT count(*) FROM history WHERE source = 'mcp' AND sql LIKE 'SELECT%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 3, "each chart's statement in the history");
}
