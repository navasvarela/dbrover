//! DynamoDB served to an MCP client through its MCP profile alone, driven as a client would,
//! against DynamoDB Local when `DBROVER_TEST_DYNAMODB_URL` is set (see `test-servers.env`).
//! Both data sources sign in as a local emulator, so no AWS account or file is read; DynamoDB
//! Local doesn't check IAM, so the MCP profile's policy refusing a write is tried by hand.
//!
//! Checks N5's exit for MCP (`docs/nosql.md`): a data source without an MCP profile isn't
//! served; one with one is, and reads a table by key; a Scan is refused by the cost guard,
//! with nobody to ask; an `UPDATE` is refused by DbRover's check; and `list_objects` lists the
//! table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;

use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
};
use dbrover_cloud::aws::{AwsCredentials, AwsSignIn};
use dbrover_core::{
    ConnectionConfig, CostGuard, DynamoDbConfig, DynamoDbSignIn, McpLogin, ProjectId,
};
use dbrover_store::{McpOverrides, Project, ProjectSlug, Projects};
use serde_json::json;
use support::{SLUG, Server, data_source, failure, ok, suffix};

fn main() {
    support::run("dynamodb", reads_dynamodb_through_an_mcp_profile);
}

/// A table keyed by `pk`, with one item, made with AWS's SDK, since PartiQL can't make one.
async fn create_table(endpoint: &str, table: &str) -> aws_sdk_dynamodb::Client {
    let sign_in = AwsSignIn {
        credentials: AwsCredentials::LocalEmulator,
        region: None,
        endpoint: Some(endpoint.to_owned()),
    };
    let client = aws_sdk_dynamodb::Client::new(&sign_in.sdk_config().await.unwrap());
    client
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    client
        .execute_statement()
        .statement(format!(
            "INSERT INTO \"{table}\" VALUE {{'pk': 'a', 'status': 'open'}}"
        ))
        .send()
        .await
        .unwrap();
    client
}

async fn reads_dynamodb_through_an_mcp_profile() {
    let Ok(endpoint) = std::env::var("DBROVER_TEST_DYNAMODB_URL") else {
        println!("dynamodb: skipped, DBROVER_TEST_DYNAMODB_URL is not set");
        return;
    };
    let table = format!("mcp-{}", suffix());
    let client = create_table(&endpoint, &table).await;
    let dir = tempfile::tempdir().unwrap();
    let project_id = ProjectId::generate();
    let config = ConnectionConfig::DynamoDb(DynamoDbConfig {
        sign_in: DynamoDbSignIn::LocalEmulator,
        profile: String::new(),
        region: String::new(),
        endpoint,
        cost_guard: CostGuard::AskBeforeScans,
    });
    let unguarded = data_source("unguarded", project_id, config.clone());
    let mut guarded = data_source("guarded", project_id, config);
    guarded.mcp_login = Some(McpLogin {
        user: "dbrover-mcp-reader".to_owned(),
    });

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
    support::write_files(
        dir.path(),
        &[unguarded, guarded],
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        &HashMap::new(),
    );

    let server = Server::start(dir.path()).await;
    let client_mcp = &server.client;
    let listed = ok(client_mcp, "list_data_sources", json!({})).await;
    let names: Vec<&str> = listed["data_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|data_source| data_source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["guarded"], "{listed}");

    let read = ok(
        client_mcp,
        "query",
        json!({
            "data_source": "guarded",
            "sql": format!("SELECT * FROM \"{table}\" WHERE pk = 'a'"),
        }),
    )
    .await;
    let rows = read["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{read}");
    assert_eq!(rows[0][0], r#"{"pk":"a","status":"open"}"#, "{read}");

    let scan = failure(
        client_mcp,
        "query",
        json!({
            "data_source": "guarded",
            "sql": format!("SELECT * FROM \"{table}\" WHERE status = 'open'"),
        }),
    )
    .await;
    assert_eq!(scan["kind"], "refused", "{scan}");
    assert!(
        scan["message"]
            .as_str()
            .is_some_and(|message| message.contains("can't be asked")),
        "{scan}"
    );

    let update = format!("UPDATE \"{table}\" SET status = 'paid' WHERE pk = 'a'");
    for tool in ["query", "execute"] {
        let refused = failure(
            client_mcp,
            tool,
            json!({ "data_source": "guarded", "sql": update }),
        )
        .await;
        assert_eq!(refused["kind"], "refused", "{tool}: {refused}");
    }

    let objects = ok(
        client_mcp,
        "list_objects",
        json!({ "data_source": "guarded", "filter": table }),
    )
    .await;
    assert_eq!(objects["objects"][0]["name"], table, "{objects}");
    assert_eq!(objects["objects"][0]["kind"], "table", "{objects}");

    let log = server.stop().await;
    assert!(
        log.contains("not served") && log.contains("only through an MCP login"),
        "{log}"
    );
    client
        .delete_table()
        .table_name(&table)
        .send()
        .await
        .unwrap();
}
