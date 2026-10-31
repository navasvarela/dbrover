//! The explorer's tree: folders of indices, aliases, data streams and index templates, and
//! under an index its fields, from its mapping.
//!
//! ```text
//! data source
//! ├─ indices
//! │  └─ orders               with its number of documents
//! │     └─ address.city      a field, by its path, with its type
//! ├─ aliases
//! ├─ data streams
//! └─ index templates
//! ```

use dbrover_core::{
    CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, DriverError, FolderKind, NodeKind,
    PageToken,
};
use dbrover_document::Value;

use crate::Flavor;
use crate::errors::{self, Refused};
use crate::http::{Call, Client};
use crate::json::{at, field, object, string, text_of};

const FOLDERS: [FolderKind; 4] = [
    FolderKind::Indices,
    FolderKind::Aliases,
    FolderKind::DataStreams,
    FolderKind::IndexTemplates,
];

/// What `_cat/indices` lists of each index: its name, document count and whether it is open,
/// hidden ones included, sorted by name.
const INDEX_COLUMNS: &str = "format=json&h=index,docs.count,status&s=index&expand_wildcards=all";

/// Settings `GET /<index>` reports that the server sets itself, which a `PUT` of a new index
/// refuses, left out of an index's definition so it runs again as written.
const SERVER_SETTINGS: &[&str] = &[
    "creation_date",
    "provided_name",
    "resize",
    "uuid",
    "version",
];

/// A template's properties the server sets itself, which a `PUT` refuses.
const SERVER_TEMPLATE_PROPERTIES: &[&str] = &[
    "created_date",
    "created_date_millis",
    "modified_date",
    "modified_date_millis",
];

/// A level of the tree, by the path's shape.
enum Level<'a> {
    Root,
    Folder(FolderKind),
    Index(&'a str),
}

fn level(path: &CatalogPath) -> Option<Level<'_>> {
    match path.segments() {
        [] => Some(Level::Root),
        [folder] => match folder.kind {
            NodeKind::Folder(kind) if FOLDERS.contains(&kind) => Some(Level::Folder(kind)),
            _ => None,
        },
        [folder, index]
            if folder.kind == NodeKind::Folder(FolderKind::Indices)
                && index.kind == NodeKind::SearchIndex =>
        {
            Some(Level::Index(&index.name))
        }
        _ => None,
    }
}

pub(crate) async fn children(
    client: &Client,
    flavor: Flavor,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    match level(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))? {
        Level::Root => Ok(FOLDERS
            .iter()
            .map(|folder| CatalogNode::folder(path, *folder))
            .collect()),
        Level::Folder(FolderKind::Indices) => indices(client, path, None).await,
        Level::Folder(FolderKind::Aliases) => aliases(client, path).await,
        Level::Folder(FolderKind::DataStreams) => data_streams(client, flavor, path).await,
        Level::Folder(FolderKind::IndexTemplates) => templates(client, path).await,
        Level::Folder(_) => Err(DriverError::InvalidPath(path.clone())),
        Level::Index(index) => fields(client, path, index).await,
    }
}

/// The indices a page lists, by name, with the filter as a wildcard pattern the server
/// applies; every other level is listed whole and filtered here.
pub(crate) async fn children_page(
    client: &Client,
    flavor: Flavor,
    path: &CatalogPath,
    query: &ChildrenQuery,
) -> Result<ChildrenPage, DriverError> {
    if !matches!(level(path), Some(Level::Folder(FolderKind::Indices))) {
        return Ok(ChildrenPage::whole(
            children(client, flavor, path).await?,
            query,
        ));
    }
    let nodes = indices(client, path, query.filter.as_deref()).await?;
    let after = query.after.as_ref().map(PageToken::as_str);
    let mut nodes: Vec<CatalogNode> = nodes
        .into_iter()
        .filter(|node| after.is_none_or(|after| node.label.as_str() > after))
        .collect();
    let next = match query.limit {
        Some(limit) if nodes.len() > limit => {
            nodes.truncate(limit);
            nodes.last().map(|node| PageToken::new(node.label.clone()))
        }
        _ => None,
    };
    Ok(ChildrenPage { nodes, next })
}

/// The indices, those whose name contains `filter` when there is one. Index names are lower
/// case, so the filter is too; one holding a character no name may have matches none.
async fn indices(
    client: &Client,
    path: &CatalogPath,
    filter: Option<&str>,
) -> Result<Vec<CatalogNode>, DriverError> {
    let target = match filter.map(str::to_lowercase) {
        None => format!("/_cat/indices?{INDEX_COLUMNS}"),
        Some(filter) if filter.contains(INVALID_NAME_CHARACTERS) => return Ok(Vec::new()),
        Some(filter) => format!("/_cat/indices/*{filter}*?{INDEX_COLUMNS}"),
    };
    let Value::Array(indices) = get(client, &target).await? else {
        return Ok(Vec::new());
    };
    Ok(indices
        .iter()
        .filter_map(|index| {
            let name = text_of(index, "index")?;
            Some(CatalogNode {
                path: path.child(NodeKind::SearchIndex, &name),
                detail: index_detail(index),
                has_children: true,
                has_definition: true,
                system: is_system(&name),
                label: name,
            })
        })
        .collect())
}

/// What an index's name can't hold, besides upper case: a filter with one matches nothing,
/// and some would change the request's path.
const INVALID_NAME_CHARACTERS: &[char] = &[
    '\\', '/', '*', '?', '"', '<', '>', '|', ' ', ',', '#', '&', '%',
];

/// A name starting with `.` is a system or hidden index, alias or data stream by convention.
fn is_system(name: &str) -> bool {
    name.starts_with('.')
}

/// An index's document count, or that it is closed.
fn index_detail(index: &Value) -> Option<String> {
    if text_of(index, "status").as_deref() == Some("close") {
        return Some("closed".to_owned());
    }
    let count = text_of(index, "docs.count")?;
    Some(match count.as_str() {
        "1" => "1 document".to_owned(),
        _ => format!("{count} documents"),
    })
}

/// The aliases, each with the indices it names.
async fn aliases(client: &Client, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let rows = get(
        client,
        "/_cat/aliases?format=json&h=alias,index&s=alias,index&expand_wildcards=all",
    )
    .await?;
    let Value::Array(rows) = rows else {
        return Ok(Vec::new());
    };
    let mut aliases: Vec<(String, Vec<String>)> = Vec::new();
    for row in &rows {
        let (Some(alias), Some(index)) = (text_of(row, "alias"), text_of(row, "index")) else {
            continue;
        };
        match aliases.last_mut() {
            Some((last, indices)) if *last == alias => indices.push(index),
            _ => aliases.push((alias, vec![index])),
        }
    }
    Ok(aliases
        .into_iter()
        .map(|(alias, indices)| CatalogNode {
            path: path.child(NodeKind::SearchIndex, &alias),
            detail: Some(format!("→ {}", indices.join(", "))),
            has_children: false,
            has_definition: true,
            system: is_system(&alias),
            label: alias,
        })
        .collect())
}

/// The data streams, hidden ones among the system ones on Elasticsearch; OpenSearch lists no
/// hidden data streams, and refuses to be asked for them.
async fn data_streams(
    client: &Client,
    flavor: Flavor,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let target = match flavor {
        Flavor::Elasticsearch => "/_data_stream?expand_wildcards=all",
        Flavor::OpenSearch => "/_data_stream",
    };
    let answer = get(client, target).await?;
    let Some(Value::Array(streams)) = field(&answer, "data_streams") else {
        return Ok(Vec::new());
    };
    let mut nodes: Vec<CatalogNode> = streams
        .iter()
        .filter_map(|stream| {
            let name = text_of(stream, "name")?;
            let backing = match field(stream, "indices") {
                Some(Value::Array(indices)) => indices.len(),
                _ => 0,
            };
            let flagged = |flag| matches!(field(stream, flag), Some(Value::Bool(true)));
            Some(CatalogNode {
                path: path.child(NodeKind::SearchIndex, &name),
                detail: Some(match backing {
                    1 => "1 backing index".to_owned(),
                    count => format!("{count} backing indices"),
                }),
                has_children: false,
                has_definition: false,
                system: is_system(&name) || flagged("hidden") || flagged("system"),
                label: name,
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(nodes)
}

/// The composable index templates. Those the server manages itself (Elasticsearch's `logs`,
/// `metrics` and the like, marked `managed` in their `_meta`) are system ones.
async fn templates(client: &Client, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let answer = get(client, "/_index_template").await?;
    let Some(Value::Array(templates)) = field(&answer, "index_templates") else {
        return Ok(Vec::new());
    };
    let mut nodes: Vec<CatalogNode> = templates
        .iter()
        .filter_map(|template| {
            let name = text_of(template, "name")?;
            let body = field(template, "index_template");
            let patterns = match body.and_then(|body| field(body, "index_patterns")) {
                Some(Value::Array(patterns)) => patterns
                    .iter()
                    .filter_map(|pattern| match pattern {
                        Value::String(pattern) => Some(pattern.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            let managed = body
                .and_then(|body| at(body, &["_meta", "managed"]))
                .is_some_and(|managed| *managed == Value::Bool(true));
            Some(CatalogNode {
                path: path.child(NodeKind::IndexTemplate, &name),
                detail: (!patterns.is_empty()).then_some(patterns),
                has_children: false,
                has_definition: true,
                system: is_system(&name) || managed,
                label: name,
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(nodes)
}

/// An index's fields, from its mapping: each by its path, an object's fields under it
/// (`address.city`), a field's other ways of being indexed after it (`status.keyword`), each
/// with its type.
async fn fields(
    client: &Client,
    path: &CatalogPath,
    index: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let answer = get(client, &format!("/{index}/_mapping")).await?;
    let mut found = Vec::new();
    if let Value::Object(indices) = &answer {
        // A name may be an alias of the index that answers.
        if let Some((_, mapping)) = indices.first()
            && let Some(Value::Object(properties)) = at(mapping, &["mappings", "properties"])
        {
            mapped_fields("", properties, &mut found);
        }
    }
    Ok(found
        .into_iter()
        .map(|(name, kind)| CatalogNode {
            path: path.child(NodeKind::Column, &name),
            label: name,
            detail: Some(kind),
            has_children: false,
            has_definition: false,
            system: false,
        })
        .collect())
}

fn mapped_fields(prefix: &str, properties: &[(String, Value)], found: &mut Vec<(String, String)>) {
    for (name, mapping) in properties {
        let path = format!("{prefix}{name}");
        let kind = text_of(mapping, "type");
        if let Some(Value::Object(children)) = field(mapping, "properties") {
            if let Some(kind) = kind {
                // A `nested` field holds its own documents; it shows before its fields.
                found.push((path.clone(), kind));
            }
            mapped_fields(&format!("{path}."), children, found);
            continue;
        }
        found.push((path.clone(), kind.unwrap_or_else(|| "object".to_owned())));
        if let Some(Value::Object(variants)) = field(mapping, "fields") {
            mapped_fields(&format!("{path}."), variants, found);
        }
    }
}

/// The request that makes the object at `path` again: an index with its aliases, mappings and
/// settings, the actions that make an alias, or a template.
pub(crate) async fn definition(client: &Client, path: &CatalogPath) -> Result<String, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let [folder, object] = path.segments() else {
        return Err(invalid());
    };
    match (folder.kind, object.kind) {
        (NodeKind::Folder(FolderKind::Indices), NodeKind::SearchIndex) => {
            index_definition(client, &object.name).await
        }
        (NodeKind::Folder(FolderKind::Aliases), NodeKind::SearchIndex) => {
            alias_definition(client, &object.name).await
        }
        (NodeKind::Folder(FolderKind::IndexTemplates), NodeKind::IndexTemplate) => {
            template_definition(client, &object.name).await
        }
        _ => Err(invalid()),
    }
}

async fn index_definition(client: &Client, index: &str) -> Result<String, DriverError> {
    let answer = get(client, &format!("/{index}")).await?;
    let Some(description) = field(&answer, index) else {
        return Err(errors::connection(format!(
            "no index {index} in the answer"
        )));
    };
    let mut body = Vec::new();
    for part in ["aliases", "mappings"] {
        if let Some(value) = field(description, part).filter(|value| !is_empty(value)) {
            body.push((part.to_owned(), value.clone()));
        }
    }
    if let Some(Value::Object(settings)) = at(description, &["settings", "index"]) {
        let settable: Vec<(String, Value)> = settings
            .iter()
            .filter(|(name, _)| !SERVER_SETTINGS.contains(&name.as_str()))
            .cloned()
            .collect();
        body.push((
            "settings".to_owned(),
            object([("index", Value::Object(settable))]),
        ));
    }
    Ok(format!(
        "PUT /{index}\n{}\n",
        Value::Object(body).to_pretty_json()
    ))
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

async fn alias_definition(client: &Client, alias: &str) -> Result<String, DriverError> {
    let answer = get(client, &format!("/_alias/{alias}")).await?;
    let Value::Object(indices) = &answer else {
        return Err(errors::connection(format!(
            "no alias {alias} in the answer"
        )));
    };
    let actions: Vec<Value> = indices
        .iter()
        .filter_map(|(index, description)| {
            let options = at(description, &["aliases", alias])?;
            let mut add = vec![
                ("index".to_owned(), string(index)),
                ("alias".to_owned(), string(alias)),
            ];
            if let Value::Object(options) = options {
                add.extend(options.iter().cloned());
            }
            Some(object([("add", Value::Object(add))]))
        })
        .collect();
    let body = object([("actions", Value::Array(actions))]);
    Ok(format!("POST /_aliases\n{}\n", body.to_pretty_json()))
}

async fn template_definition(client: &Client, name: &str) -> Result<String, DriverError> {
    let answer = get(client, &format!("/_index_template/{name}")).await?;
    let template = match field(&answer, "index_templates") {
        Some(Value::Array(templates)) => templates
            .iter()
            .find(|template| text_of(template, "name").as_deref() == Some(name))
            .and_then(|template| field(template, "index_template")),
        _ => None,
    };
    let Some(Value::Object(template)) = template else {
        return Err(errors::connection(format!(
            "no template {name} in the answer"
        )));
    };
    let settable: Vec<(String, Value)> = template
        .iter()
        .filter(|(property, _)| !SERVER_TEMPLATE_PROPERTIES.contains(&property.as_str()))
        .cloned()
        .collect();
    Ok(format!(
        "PUT /_index_template/{name}\n{}\n",
        Value::Object(settable).to_pretty_json()
    ))
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
