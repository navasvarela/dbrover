//! The tables a data search reads, found by walking a driver's own catalog, for the drivers
//! whose catalogs are read through an API rather than queries a search could join (W16g: Trino,
//! Athena, BigQuery, Snowflake, Databricks). Those engines are searched a schema or a table at a
//! time, so the walk is short: a schema's folders, their tables, and each table's columns.
//!
//! A column node's detail starts with its type as the engine writes it; BigQuery's and Athena's
//! add a role after a comma (`STRING, partition key`), which is cut off.

use crate::{
    CatalogNode, CatalogPath, Connection, DriverError, FolderKind, NodeKind, RelationKind,
    Relations, SearchColumn, SearchScope, SearchableTable, TableName, TablesToSearch,
};

/// How deep under the scope's node the walk looks for tables: a database, a schema, a folder.
const LEVELS: usize = 3;

/// The relations under each of the scope's nodes, with their columns, as the explorer lists
/// them: what [`Connection::searchable_tables`] answers on the drivers named above. The rows
/// of these engines aren't keyed, and the catalogs keep no estimate.
pub async fn searchable_tables_from_catalog(
    connection: &mut dyn Connection,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let starts = match &tables.scope {
        SearchScope::Under(path) => vec![path.clone()],
        SearchScope::Tables(paths) => paths.clone(),
    };
    let mut found: Vec<CatalogPath> = Vec::new();
    for start in starts {
        if relation_kind(start.last().map(|segment| segment.kind)).is_some() {
            found.push(start);
            continue;
        }
        let mut level = vec![start];
        for _ in 0..LEVELS {
            let mut next = Vec::new();
            for path in level {
                for node in connection.children(&path).await? {
                    match node.kind() {
                        Some(kind) if takes(kind, tables.views) => found.push(node.path),
                        Some(
                            NodeKind::Database
                            | NodeKind::Schema
                            | NodeKind::Folder(
                                FolderKind::Tables
                                | FolderKind::Views
                                | FolderKind::MaterializedViews,
                            ),
                        ) => next.push(node.path),
                        _ => {}
                    }
                }
            }
            level = next;
        }
    }
    let mut searchable = Vec::new();
    for path in found {
        if let Some(table) = table_at(connection, &path).await? {
            searchable.push(table);
        }
    }
    Ok(searchable)
}

/// Whether a node of `kind` is one of the relations asked for.
fn takes(kind: NodeKind, views: Relations) -> bool {
    match relation_kind(Some(kind)) {
        Some(RelationKind::Table) => true,
        Some(RelationKind::View) => views == Relations::TablesAndViews,
        None => false,
    }
}

fn relation_kind(kind: Option<NodeKind>) -> Option<RelationKind> {
    match kind? {
        NodeKind::Table => Some(RelationKind::Table),
        NodeKind::View | NodeKind::MaterializedView => Some(RelationKind::View),
        _ => None,
    }
}

/// The relation at `path`, with the columns it lists directly or in a Columns folder; `None`
/// for a path that names no schema, which no statement could reach.
async fn table_at(
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<Option<SearchableTable>, DriverError> {
    let (Some(schema), Some(relation)) = (path.name_of(NodeKind::Schema), path.last()) else {
        return Ok(None);
    };
    let mut nodes = connection.children(path).await?;
    if !nodes.iter().any(is_column) {
        let folder = nodes
            .iter()
            .find(|node| node.kind() == Some(NodeKind::Folder(FolderKind::Columns)))
            .map(|node| node.path.clone());
        nodes = match folder {
            Some(folder) => connection.children(&folder).await?,
            None => Vec::new(),
        };
    }
    let columns = nodes
        .iter()
        .filter(|node| is_column(node))
        .map(|node| SearchColumn::new(node.label.clone(), column_type(node)))
        .collect();
    Ok(Some(SearchableTable {
        database: path.name_of(NodeKind::Database).map(str::to_owned),
        name: TableName {
            schema: schema.to_owned(),
            name: relation.name.clone(),
        },
        path: path.clone(),
        relation: relation_kind(Some(relation.kind)).unwrap_or_default(),
        key: Vec::new(),
        row_estimate: None,
        columns,
    }))
}

fn is_column(node: &CatalogNode) -> bool {
    node.kind() == Some(NodeKind::Column)
}

/// The type at the start of a column's detail, before any role the explorer adds after a comma.
/// A type's own commas are inside its parentheses or angle brackets (`NUMBER(38,0)`,
/// `map<string,int>`), so the cut is at the first comma outside them.
fn column_type(node: &CatalogNode) -> String {
    let detail = node.detail.as_deref().unwrap_or_default();
    let mut depth = 0_i32;
    for (index, character) in detail.char_indices() {
        match character {
            '(' | '<' => depth += 1,
            ')' | '>' => depth -= 1,
            ',' if depth == 0 => return detail[..index].trim().to_owned(),
            _ => {}
        }
    }
    detail.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(detail: &str) -> CatalogNode {
        CatalogNode {
            path: CatalogPath::root().child(NodeKind::Column, "c"),
            label: "c".to_owned(),
            detail: Some(detail.to_owned()),
            has_children: false,
            has_definition: false,
            system: false,
        }
    }

    #[test]
    fn reads_the_type_before_the_role() {
        for (detail, data_type) in [
            ("STRING", "STRING"),
            ("STRING, partition key", "STRING"),
            ("NUMBER(38,0)", "NUMBER(38,0)"),
            ("map<string,int>, clustering", "map<string,int>"),
            ("decimal(10,2), partition key", "decimal(10,2)"),
        ] {
            assert_eq!(column_type(&column(detail)), data_type, "{detail}");
        }
    }
}
