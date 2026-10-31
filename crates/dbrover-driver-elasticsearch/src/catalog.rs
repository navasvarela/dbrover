//! The explorer's tree: the cluster's indices.

use dbrover_core::{CatalogNode, CatalogPath, DriverError, NodeKind};
use dbrover_document::Value;

use crate::errors::{self, Refused};
use crate::http::{Call, Client};
use crate::json::text_of;

/// Every index, hidden ones included, sorted by name, with the columns the explorer shows.
const INDICES: &str =
    "/_cat/indices?format=json&h=index,docs.count,status&s=index&expand_wildcards=all";

pub(crate) async fn children(
    client: &Client,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    if !path.is_root() {
        return Err(DriverError::InvalidPath(path.clone()));
    }
    let indices = get(client, INDICES).await?;
    let Value::Array(indices) = indices else {
        return Ok(Vec::new());
    };
    Ok(indices
        .iter()
        .filter_map(|index| {
            let name = text_of(index, "index")?;
            Some(CatalogNode {
                path: path.child(NodeKind::SearchIndex, &name),
                detail: detail(index),
                has_children: false,
                has_definition: false,
                // A name starting with `.` is a system or hidden index by convention.
                system: name.starts_with('.'),
                label: name,
            })
        })
        .collect())
}

/// An index's document count, or that it is closed.
fn detail(index: &Value) -> Option<String> {
    if text_of(index, "status").as_deref() == Some("close") {
        return Some("closed".to_owned());
    }
    let count = text_of(index, "docs.count")?;
    Some(match count.as_str() {
        "1" => "1 document".to_owned(),
        _ => format!("{count} documents"),
    })
}

/// The JSON answer of `GET target`, or the server's refusal as the connection's error.
pub(crate) async fn get(client: &Client, target: &str) -> Result<Value, DriverError> {
    let answer = client
        .send(Call::get(target))
        .await
        .map_err(errors::connection)?;
    if !answer.status.is_success() {
        return Err(errors::connection(Refused::of(&answer)));
    }
    answer.json().map_err(errors::connection)
}
