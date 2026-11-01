//! The explorer's tree: the region's tables.

use aws_sdk_dynamodb::Client;
use dbrover_core::{CatalogNode, CatalogPath, DriverError, NodeKind};

use crate::errors::{self, Refused};

/// The most names `ListTables` gives at a time.
const TABLES_PER_LIST: i32 = 100;

pub(crate) async fn children(
    client: &Client,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    if !path.is_root() {
        return Err(DriverError::InvalidPath(path.clone()));
    }
    let names = table_names(client).await?;
    Ok(names
        .into_iter()
        .map(|name| table_node(path, name))
        .collect())
}

/// Every table's name, in the order DynamoDB lists them: by name.
async fn table_names(client: &Client) -> Result<Vec<String>, DriverError> {
    let mut names = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let answer = client
            .list_tables()
            .limit(TABLES_PER_LIST)
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

fn table_node(parent: &CatalogPath, name: String) -> CatalogNode {
    CatalogNode {
        path: parent.child(NodeKind::Table, name.clone()),
        label: name,
        detail: None,
        has_children: false,
        has_definition: false,
        system: false,
    }
}
