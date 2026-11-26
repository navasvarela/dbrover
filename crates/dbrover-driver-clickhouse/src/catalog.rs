//! Catalog introspection from the `system` tables, one query per tree level, with definitions
//! from `system.tables.create_table_query`.
//!
//! ClickHouse's databases hold its tables directly, as MySQL's do, so they are listed as
//! schemas at the root and names qualify as `database.table`. Functions are left out: a
//! user-defined function belongs to the whole server, not to a database.

use clickhouse::Client;
use dbrover_core::{CatalogNode, CatalogPath, DriverError, FolderKind, NodeKind, PathSegment, Row};

use crate::connection_error;
use crate::execution::rows;

const SCHEMA_FOLDERS: &[FolderKind] = &[
    FolderKind::Tables,
    FolderKind::Views,
    FolderKind::MaterializedViews,
    FolderKind::Dictionaries,
];

/// The server's own databases, which the explorer hides unless asked. `information_schema`
/// comes in both spellings, since names are case-sensitive.
pub(crate) const SYSTEM_SCHEMAS: &[&str] = &["INFORMATION_SCHEMA", "information_schema", "system"];

/// The engines of objects that aren't tables, which their own folders list.
pub(crate) const NOT_TABLES: &str =
    "('View', 'MaterializedView', 'Dictionary', 'LiveView', 'WindowView')";

/// The table where a materialized view without a `TO` table keeps its rows, named
/// `.inner_id.<uuid>` (or `.inner.<view>` in an `Ordinary` database).
pub(crate) const INNER_TABLE_PREFIX: &str = ".inner";

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    Schemas,
    SchemaFolders,
    SchemaObjects {
        schema: &'a str,
        folder: FolderKind,
    },
    RelationFolders {
        relation_kind: NodeKind,
    },
    RelationChildren {
        schema: &'a str,
        relation: &'a str,
        folder: FolderKind,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        let [schema, rest @ ..] = path.segments() else {
            return Some(Self::Schemas);
        };
        if schema.kind != NodeKind::Schema {
            return None;
        }
        let schema = schema.name.as_str();
        match rest {
            [] => Some(Self::SchemaFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| SCHEMA_FOLDERS.contains(folder))
                .map(|folder| Self::SchemaObjects { schema, folder }),
            [folder, relation] if is_object_in(folder, relation) => Some(Self::RelationFolders {
                relation_kind: relation.kind,
            }),
            [folder, relation, child_folder] if is_object_in(folder, relation) => {
                folder_kind(child_folder)
                    .filter(|child| relation_folders(relation.kind).contains(child))
                    .map(|folder| Self::RelationChildren {
                        schema,
                        relation: &relation.name,
                        folder,
                    })
            }
            _ => None,
        }
    }
}

fn folder_kind(segment: &PathSegment) -> Option<FolderKind> {
    match segment.kind {
        NodeKind::Folder(folder) => Some(folder),
        _ => None,
    }
}

/// The folder a database's objects of `kind` are listed in.
fn folder_of(kind: NodeKind) -> Option<FolderKind> {
    match kind {
        NodeKind::Table => Some(FolderKind::Tables),
        NodeKind::View => Some(FolderKind::Views),
        NodeKind::MaterializedView => Some(FolderKind::MaterializedViews),
        NodeKind::Dictionary => Some(FolderKind::Dictionaries),
        _ => None,
    }
}

fn is_object_in(folder: &PathSegment, object: &PathSegment) -> bool {
    folder_of(object.kind).is_some_and(|expected| folder_kind(folder) == Some(expected))
}

/// A table's data-skipping indexes are its only indexes: its primary key is its sort order,
/// shown in its definition.
fn relation_folders(kind: NodeKind) -> &'static [FolderKind] {
    match kind {
        NodeKind::Table => &[FolderKind::Columns, FolderKind::Indexes],
        NodeKind::View | NodeKind::MaterializedView | NodeKind::Dictionary => {
            &[FolderKind::Columns]
        }
        _ => &[],
    }
}

pub(crate) async fn children(
    client: &Client,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    match level {
        Level::Schemas => schemas(client, path).await,
        Level::SchemaFolders => Ok(folders(path, SCHEMA_FOLDERS)),
        Level::SchemaObjects { schema, folder } => {
            schema_objects(client, path, schema, folder).await
        }
        Level::RelationFolders { relation_kind } => {
            Ok(folders(path, relation_folders(relation_kind)))
        }
        Level::RelationChildren {
            schema,
            relation,
            folder,
        } => relation_children(client, path, schema, relation, folder).await,
    }
}

fn folders(parent: &CatalogPath, folders: &[FolderKind]) -> Vec<CatalogNode> {
    folders
        .iter()
        .map(|folder| CatalogNode::folder(parent, *folder))
        .collect()
}

/// A node with no detail, not a system object.
fn node(
    parent: &CatalogPath,
    kind: NodeKind,
    name: String,
    has_children: bool,
    has_definition: bool,
) -> CatalogNode {
    CatalogNode {
        path: parent.child(kind, name.clone()),
        label: name,
        detail: None,
        has_children,
        has_definition,
        system: false,
    }
}

/// Column `index` of a catalog row, which the query guarantees is there and not `NULL`.
pub(crate) fn field(row: &Row, index: usize) -> Result<String, DriverError> {
    row.get(index)
        .cloned()
        .flatten()
        .ok_or_else(|| connection_error(format!("catalog row without column {index}")))
}

async fn schemas(client: &Client, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT name FROM system.databases ORDER BY name";
    rows(client, sql, &[])
        .await?
        .iter()
        .map(|row| {
            let name = field(row, 0)?;
            let system = SYSTEM_SCHEMAS.contains(&name.as_str());
            Ok(CatalogNode {
                system,
                ..node(path, NodeKind::Schema, name, true, false)
            })
        })
        .collect()
}

async fn schema_objects(
    client: &Client,
    path: &CatalogPath,
    schema: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let (kind, engines) = match folder {
        FolderKind::Tables => (NodeKind::Table, format!("engine NOT IN {NOT_TABLES}")),
        FolderKind::Views => (NodeKind::View, "engine = 'View'".to_owned()),
        FolderKind::MaterializedViews => (
            NodeKind::MaterializedView,
            "engine = 'MaterializedView'".to_owned(),
        ),
        FolderKind::Dictionaries => (NodeKind::Dictionary, "engine = 'Dictionary'".to_owned()),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    // Tables of `system` and `information_schema` have no `CREATE` statement to show.
    let sql = format!(
        "SELECT name, create_table_query != '' FROM system.tables \
         WHERE database = {{schema:String}} AND NOT is_temporary AND {engines} ORDER BY name"
    );
    rows(client, &sql, &[("schema", schema)])
        .await?
        .iter()
        .map(|row| {
            let name = field(row, 0)?;
            let has_definition = field(row, 1)? == "1";
            Ok(CatalogNode {
                system: name.starts_with(INNER_TABLE_PREFIX),
                ..node(path, kind, name, true, has_definition)
            })
        })
        .collect()
}

async fn relation_children(
    client: &Client,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let (kind, sql) = match folder {
        FolderKind::Columns => (
            NodeKind::Column,
            "SELECT name, type FROM system.columns \
             WHERE database = {schema:String} AND table = {relation:String} ORDER BY position",
        ),
        FolderKind::Indexes => (
            NodeKind::Index,
            "SELECT name, concat(type, ' (', expr, ')') FROM system.data_skipping_indices \
             WHERE database = {schema:String} AND table = {relation:String} ORDER BY name",
        ),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    rows(client, sql, &[("schema", schema), ("relation", relation)])
        .await?
        .iter()
        .map(|row| {
            Ok(CatalogNode {
                detail: row.get(1).cloned().flatten(),
                ..node(path, kind, field(row, 0)?, false, false)
            })
        })
        .collect()
}

pub(crate) async fn definition(client: &Client, path: &CatalogPath) -> Result<String, DriverError> {
    // Only paths `children` returns: the level above must be a valid one.
    let valid_parent = path
        .parent()
        .is_some_and(|parent| Level::of(&parent).is_some());
    let object = path
        .last()
        .filter(|object| valid_parent && folder_of(object.kind).is_some())
        .ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let schema = path
        .name_of(NodeKind::Schema)
        .ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let sql = "SELECT create_table_query FROM system.tables \
               WHERE database = {schema:String} AND name = {name:String}";
    let found = rows(client, sql, &[("schema", schema), ("name", &object.name)]).await?;
    found
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .filter(|definition| !definition.is_empty())
        .ok_or_else(|| connection_error(format!("{} has no definition to show", object.name)))
}
