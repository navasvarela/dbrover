//! The explorer's tree: the region's tables, then each table's key attributes and secondary
//! indexes.
//!
//! ```text
//! orders                    ≈ 1,204 items · 2.3 MB
//! ├── customer              partition key · S
//! ├── placed                sort key · S
//! └── indexes
//!     └── by-status         global · status, placed
//! ```
//!
//! Counts and sizes are `DescribeTable`'s, which AWS updates about every six hours; nothing is
//! counted, and listing and describing tables is free.

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, KeySchemaElement, Projection, TableDescription,
};
use dbrover_core::{
    CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, DriverError, FolderKind, NodeKind,
    PageToken,
};
use dbrover_document::Value;
use futures_util::future::try_join_all;

use crate::errors::{self, Refused};
use crate::keys::{KeyCache, Keys};

/// The most names `ListTables` gives at a time, and so the most tables a page of the explorer
/// describes at once.
const TABLES_PER_PAGE: usize = 100;

pub(crate) async fn children(
    client: &Client,
    keys: &KeyCache,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    match path.segments() {
        [] => {
            let mut nodes = Vec::new();
            let mut query = ChildrenQuery::default();
            loop {
                let page = tables_page(client, keys, path, &query).await?;
                nodes.extend(page.nodes);
                match page.next {
                    Some(next) => query = query.after(next),
                    None => return Ok(nodes),
                }
            }
        }
        [table] if table.kind == NodeKind::Table => {
            let description = described(client, keys, path, &table.name).await?;
            Ok(table_children(path, &description))
        }
        [table, folder]
            if table.kind == NodeKind::Table
                && folder.kind == NodeKind::Folder(FolderKind::Indexes) =>
        {
            let description = described(client, keys, path, &table.name).await?;
            Ok(index_nodes(path, &description))
        }
        _ => Err(DriverError::InvalidPath(path.clone())),
    }
}

/// `table`'s description, for the node at `path`: a table dropped since it was listed is a
/// node that disappeared.
async fn described(
    client: &Client,
    keys: &KeyCache,
    path: &CatalogPath,
    table: &str,
) -> Result<TableDescription, DriverError> {
    keys.describe(client, table)
        .await?
        .ok_or_else(|| DriverError::InvalidPath(path.clone()))
}

/// The tables a page of the root holds: those whose name contains the filter, ignoring case,
/// after the token's name, at most [`TABLES_PER_PAGE`] of them unless the whole level is asked
/// for. `ListTables` has no filter, and
/// lists names for free, so the names are listed whole and filtered here; only the page's own
/// tables are described.
pub(crate) async fn children_page(
    client: &Client,
    keys: &KeyCache,
    path: &CatalogPath,
    query: &ChildrenQuery,
) -> Result<ChildrenPage, DriverError> {
    if !path.is_root() {
        return Ok(ChildrenPage::whole(
            children(client, keys, path).await?,
            query,
        ));
    }
    tables_page(client, keys, path, query).await
}

async fn tables_page(
    client: &Client,
    keys: &KeyCache,
    path: &CatalogPath,
    query: &ChildrenQuery,
) -> Result<ChildrenPage, DriverError> {
    let filter = query.filter.as_deref().map(str::to_lowercase);
    let after = query.after.as_ref().map(PageToken::as_str);
    let limit = query
        .limit
        .map_or(usize::MAX, |limit| limit.min(TABLES_PER_PAGE));
    let mut names = table_names(client).await?.into_iter().filter(|name| {
        after.is_none_or(|after| name.as_str() > after)
            && filter
                .as_deref()
                .is_none_or(|filter| name.to_lowercase().contains(filter))
    });
    let page: Vec<String> = names.by_ref().take(limit).collect();
    let next = match (names.next(), page.last()) {
        (Some(_), Some(last)) => Some(PageToken::new(last.clone())),
        _ => None,
    };
    let descriptions = try_join_all(page.iter().map(|name| keys.describe(client, name))).await?;
    // A table dropped between listing and describing is left out.
    let nodes = page
        .into_iter()
        .zip(&descriptions)
        .filter_map(|(name, description)| Some(table_node(path, name, description.as_ref()?)))
        .collect();
    Ok(ChildrenPage { nodes, next })
}

/// Every table's name, in the order DynamoDB lists them: by name.
async fn table_names(client: &Client) -> Result<Vec<String>, DriverError> {
    let mut names = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let answer = client
            .list_tables()
            .limit(TABLES_PER_PAGE as i32)
            .set_exclusive_start_table_name(after.take())
            .send()
            .await
            .map_err(|error| errors::connection(Refused::from(error)))?;
        names.extend(answer.table_names().iter().cloned());
        match answer.last_evaluated_table_name() {
            Some(last) => after = Some(last.to_owned()),
            None => return Ok(names),
        }
    }
}

fn table_node(parent: &CatalogPath, name: String, description: &TableDescription) -> CatalogNode {
    let items = description.item_count().map(group_thousands);
    let size = description.table_size_bytes().map(megabytes);
    let detail = match (items, size) {
        (Some(items), Some(size)) => Some(format!("\u{2248} {items} items \u{b7} {size}")),
        _ => None,
    };
    CatalogNode {
        path: parent.child(NodeKind::Table, name.clone()),
        label: name,
        detail,
        has_children: true,
        has_definition: true,
        system: false,
    }
}

/// A table's key attributes, partition first, then a folder of its indexes when it has any.
fn table_children(path: &CatalogPath, description: &TableDescription) -> Vec<CatalogNode> {
    let definitions = description.attribute_definitions();
    let mut nodes: Vec<CatalogNode> = description
        .key_schema()
        .iter()
        .map(|key| key_node(path, key, definitions))
        .collect();
    let has_indexes = !description.global_secondary_indexes().is_empty()
        || !description.local_secondary_indexes().is_empty();
    if has_indexes {
        nodes.push(CatalogNode::folder(path, FolderKind::Indexes));
    }
    nodes
}

fn key_node(
    table: &CatalogPath,
    key: &KeySchemaElement,
    definitions: &[AttributeDefinition],
) -> CatalogNode {
    let name = key.attribute_name();
    let role = match key.key_type().as_str() {
        "HASH" => "partition key",
        _ => "sort key",
    };
    let scalar = definitions
        .iter()
        .find(|definition| definition.attribute_name() == name)
        .map(|definition| definition.attribute_type().as_str());
    CatalogNode {
        path: table.child(NodeKind::Column, name),
        label: name.to_owned(),
        detail: Some(match scalar {
            Some(scalar) => format!("{role} \u{b7} {scalar}"),
            None => role.to_owned(),
        }),
        has_children: false,
        has_definition: false,
        system: false,
    }
}

/// The table's global and local secondary indexes, by name, each with its keys.
fn index_nodes(folder: &CatalogPath, description: &TableDescription) -> Vec<CatalogNode> {
    let global = description
        .global_secondary_indexes()
        .iter()
        .map(|index| ("global", index.index_name(), index.key_schema()));
    let local = description
        .local_secondary_indexes()
        .iter()
        .map(|index| ("local", index.index_name(), index.key_schema()));
    let mut nodes: Vec<CatalogNode> = global
        .chain(local)
        .filter_map(|(scope, name, schema)| {
            let name = name?;
            let keys = Keys::of(schema)?;
            let key_names: Vec<&str> = keys.names().map(String::as_str).collect();
            Some(CatalogNode {
                path: folder.child(NodeKind::Index, name),
                label: name.to_owned(),
                detail: Some(format!("{scope} \u{b7} {}", key_names.join(", "))),
                has_children: false,
                has_definition: false,
                system: false,
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.label.cmp(&b.label));
    nodes
}

/// The table's `CreateTable` input as JSON, the form `aws dynamodb create-table
/// --cli-input-json` takes: its keys, attribute definitions, billing, indexes, stream and
/// deletion protection, without what AWS sets (its ARN, counts, dates and status).
pub(crate) async fn definition(
    client: &Client,
    keys: &KeyCache,
    path: &CatalogPath,
) -> Result<String, DriverError> {
    let [table] = path.segments() else {
        return Err(DriverError::InvalidPath(path.clone()));
    };
    if table.kind != NodeKind::Table {
        return Err(DriverError::InvalidPath(path.clone()));
    }
    let description = described(client, keys, path, &table.name).await?;
    Ok(create_table_input(&table.name, &description).to_pretty_json())
}

fn create_table_input(name: &str, description: &TableDescription) -> Value {
    let provisioned = is_provisioned(description);
    let mut fields = vec![
        field("TableName", string(name)),
        field("KeySchema", key_schema(description.key_schema())),
        field(
            "AttributeDefinitions",
            Value::Array(
                description
                    .attribute_definitions()
                    .iter()
                    .map(|definition| {
                        object(vec![
                            field("AttributeName", string(definition.attribute_name())),
                            field(
                                "AttributeType",
                                string(definition.attribute_type().as_str()),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ];
    if provisioned {
        fields.push(field(
            "BillingMode",
            string(BillingMode::Provisioned.as_str()),
        ));
        if let Some(throughput) = description.provisioned_throughput() {
            fields.push(field(
                "ProvisionedThroughput",
                throughput_value(
                    throughput.read_capacity_units(),
                    throughput.write_capacity_units(),
                ),
            ));
        }
    } else {
        fields.push(field(
            "BillingMode",
            string(BillingMode::PayPerRequest.as_str()),
        ));
    }
    let global: Vec<Value> = description
        .global_secondary_indexes()
        .iter()
        .map(|index| {
            let mut index_fields = vec![
                field("IndexName", string(index.index_name().unwrap_or_default())),
                field("KeySchema", key_schema(index.key_schema())),
                field("Projection", projection(index.projection())),
            ];
            if provisioned && let Some(throughput) = index.provisioned_throughput() {
                index_fields.push(field(
                    "ProvisionedThroughput",
                    throughput_value(
                        throughput.read_capacity_units(),
                        throughput.write_capacity_units(),
                    ),
                ));
            }
            object(index_fields)
        })
        .collect();
    if !global.is_empty() {
        fields.push(field("GlobalSecondaryIndexes", Value::Array(global)));
    }
    let local: Vec<Value> = description
        .local_secondary_indexes()
        .iter()
        .map(|index| {
            object(vec![
                field("IndexName", string(index.index_name().unwrap_or_default())),
                field("KeySchema", key_schema(index.key_schema())),
                field("Projection", projection(index.projection())),
            ])
        })
        .collect();
    if !local.is_empty() {
        fields.push(field("LocalSecondaryIndexes", Value::Array(local)));
    }
    if let Some(stream) = description.stream_specification()
        && stream.stream_enabled()
    {
        let mut stream_fields = vec![field("StreamEnabled", Value::Bool(true))];
        if let Some(view) = stream.stream_view_type() {
            stream_fields.push(field("StreamViewType", string(view.as_str())));
        }
        fields.push(field("StreamSpecification", object(stream_fields)));
    }
    if description.deletion_protection_enabled() == Some(true) {
        fields.push(field("DeletionProtectionEnabled", Value::Bool(true)));
    }
    object(fields)
}

/// Whether the table is billed for capacity set ahead, rather than per request: AWS writes no
/// billing summary for a table that has always been provisioned.
fn is_provisioned(description: &TableDescription) -> bool {
    match description
        .billing_mode_summary()
        .and_then(|summary| summary.billing_mode())
    {
        Some(mode) => *mode == BillingMode::Provisioned,
        None => description
            .provisioned_throughput()
            .and_then(|throughput| throughput.read_capacity_units())
            .is_some_and(|units| units > 0),
    }
}

fn key_schema(schema: &[KeySchemaElement]) -> Value {
    Value::Array(
        schema
            .iter()
            .map(|key| {
                object(vec![
                    field("AttributeName", string(key.attribute_name())),
                    field("KeyType", string(key.key_type().as_str())),
                ])
            })
            .collect(),
    )
}

fn projection(projection: Option<&Projection>) -> Value {
    let mut fields = Vec::new();
    if let Some(kind) = projection.and_then(Projection::projection_type) {
        fields.push(field("ProjectionType", string(kind.as_str())));
    }
    let attributes = projection.map_or(&[][..], Projection::non_key_attributes);
    if !attributes.is_empty() {
        fields.push(field(
            "NonKeyAttributes",
            Value::Array(attributes.iter().map(|name| string(name)).collect()),
        ));
    }
    object(fields)
}

fn throughput_value(read: Option<i64>, write: Option<i64>) -> Value {
    object(vec![
        field("ReadCapacityUnits", number(read.unwrap_or_default())),
        field("WriteCapacityUnits", number(write.unwrap_or_default())),
    ])
}

fn field(name: &str, value: Value) -> (String, Value) {
    (name.to_owned(), value)
}

fn object(fields: Vec<(String, Value)>) -> Value {
    Value::Object(fields)
}

fn string(text: &str) -> Value {
    Value::String(text.to_owned())
}

fn number(number: i64) -> Value {
    Value::Number(number.to_string())
}

/// `1234567` as `1,234,567`.
fn group_thousands(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match number < 0 {
        true => format!("-{grouped}"),
        false => grouped,
    }
}

/// A size in bytes in megabytes, to one decimal place, or in kilobytes below one.
fn megabytes(bytes: i64) -> String {
    const KILOBYTE: f64 = 1_000.0;
    const MEGABYTE: f64 = 1_000_000.0;
    let bytes = bytes as f64;
    match bytes < MEGABYTE {
        true => format!("{:.1} kB", bytes / KILOBYTE),
        false => format!("{:.1} MB", bytes / MEGABYTE),
    }
}
