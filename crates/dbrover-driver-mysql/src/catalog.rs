//! Catalog introspection: one `information_schema` query per tree level, and definitions from
//! `SHOW CREATE …`.
//!
//! MySQL's databases are its schemas (`CREATE SCHEMA` is a synonym of `CREATE DATABASE`), so
//! they are listed as schemas at the root, and names qualify as `schema.table` the way
//! statements write them.

use dbrover_core::{CatalogNode, CatalogPath, DriverError, FolderKind, NodeKind, PathSegment};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Params, Value};

use crate::{Flavour, MYSQL_QUOTING, accounts, connection_error};

const SCHEMA_FOLDERS: &[FolderKind] = &[
    FolderKind::Tables,
    FolderKind::Views,
    FolderKind::Functions,
    FolderKind::Procedures,
    FolderKind::Events,
];

/// The server's own databases, which the explorer hides unless asked.
pub(crate) const SYSTEM_SCHEMAS: &[&str] =
    &["information_schema", "mysql", "performance_schema", "sys"];

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    /// The users and roles folders, then the schemas.
    Root,
    /// The users or the roles folder (W18).
    Accounts(FolderKind),
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
            return Some(Self::Root);
        };
        if let (Some(folder @ (FolderKind::Users | FolderKind::Roles)), []) =
            (folder_kind(schema), rest)
        {
            return Some(Self::Accounts(folder));
        }
        if schema.kind != NodeKind::Schema {
            return None;
        }
        let schema = schema.name.as_str();
        match rest {
            [] => Some(Self::SchemaFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| SCHEMA_FOLDERS.contains(folder))
                .map(|folder| Self::SchemaObjects { schema, folder }),
            [folder, relation] if is_relation_in(folder, relation) => Some(Self::RelationFolders {
                relation_kind: relation.kind,
            }),
            [folder, relation, child_folder] if is_relation_in(folder, relation) => {
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

fn is_relation_in(folder: &PathSegment, relation: &PathSegment) -> bool {
    let expected = match relation.kind {
        NodeKind::Table => FolderKind::Tables,
        NodeKind::View => FolderKind::Views,
        _ => return false,
    };
    folder_kind(folder) == Some(expected)
}

fn relation_folders(kind: NodeKind) -> &'static [FolderKind] {
    match kind {
        NodeKind::Table => &[
            FolderKind::Columns,
            FolderKind::Indexes,
            FolderKind::Constraints,
            FolderKind::Triggers,
        ],
        NodeKind::View => &[FolderKind::Columns],
        _ => &[],
    }
}

pub(crate) async fn children(
    conn: &mut Conn,
    flavour: Flavour,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    match level {
        // Accounts belong to the whole server, so their folders come before the databases, as
        // PostgreSQL's roles folder does.
        Level::Root => {
            let mut nodes = folders(path, &[FolderKind::Users, FolderKind::Roles]);
            nodes.extend(schemas(conn, path).await?);
            Ok(nodes)
        }
        Level::Accounts(folder) => accounts::list(conn, flavour, path, folder).await,
        Level::SchemaFolders => Ok(folders(path, SCHEMA_FOLDERS)),
        Level::SchemaObjects { schema, folder } => schema_objects(conn, path, schema, folder).await,
        Level::RelationFolders { relation_kind } => {
            Ok(folders(path, relation_folders(relation_kind)))
        }
        Level::RelationChildren {
            schema,
            relation,
            folder,
        } => relation_children(conn, path, schema, relation, folder).await,
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

/// Runs a catalog query and returns its rows as text, `NULL` as `None`.
pub(crate) async fn query(
    conn: &mut Conn,
    sql: &str,
    params: impl Into<Params> + Send,
) -> Result<Vec<Vec<Option<String>>>, DriverError> {
    let rows: Vec<mysql_async::Row> = conn.exec(sql, params).await.map_err(connection_error)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            row.unwrap_raw()
                .into_iter()
                .map(|value| value.and_then(text))
                .collect()
        })
        .collect())
}

fn text(value: Value) -> Option<String> {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        other => Some(other.as_sql(true)),
    }
}

/// Column `index` of a catalog row, which the query guarantees is there and not `NULL`.
pub(crate) fn field(row: &[Option<String>], index: usize) -> Result<String, DriverError> {
    row.get(index)
        .cloned()
        .flatten()
        .ok_or_else(|| connection_error(format!("catalog row without column {index}")))
}

async fn schemas(conn: &mut Conn, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name";
    query(conn, sql, ())
        .await?
        .iter()
        .map(|row| {
            let name = field(row, 0)?;
            let system = SYSTEM_SCHEMAS.contains(&name.to_ascii_lowercase().as_str());
            Ok(CatalogNode {
                system,
                ..node(path, NodeKind::Schema, name, true, false)
            })
        })
        .collect()
}

async fn schema_objects(
    conn: &mut Conn,
    path: &CatalogPath,
    schema: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let (kind, sql): (NodeKind, &str) = match folder {
        // MariaDB's system-versioned tables are tables with their history kept alongside.
        FolderKind::Tables => (
            NodeKind::Table,
            "SELECT table_name, 1 FROM information_schema.tables \
             WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'SYSTEM VERSIONED') \
             ORDER BY table_name",
        ),
        // `information_schema`'s own tables are system views, with no definition to show.
        FolderKind::Views => (
            NodeKind::View,
            "SELECT table_name, table_type = 'VIEW' FROM information_schema.tables \
             WHERE table_schema = ? AND table_type IN ('VIEW', 'SYSTEM VIEW') \
             ORDER BY table_name",
        ),
        FolderKind::Functions => (
            NodeKind::Function,
            "SELECT routine_name, 1 FROM information_schema.routines \
             WHERE routine_schema = ? AND routine_type = 'FUNCTION' ORDER BY routine_name",
        ),
        FolderKind::Procedures => (
            NodeKind::Procedure,
            "SELECT routine_name, 1 FROM information_schema.routines \
             WHERE routine_schema = ? AND routine_type = 'PROCEDURE' ORDER BY routine_name",
        ),
        FolderKind::Events => (
            NodeKind::Event,
            "SELECT event_name, 1 FROM information_schema.events \
             WHERE event_schema = ? ORDER BY event_name",
        ),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    let is_relation = matches!(kind, NodeKind::Table | NodeKind::View);
    query(conn, sql, (schema,))
        .await?
        .iter()
        .map(|row| {
            let has_definition = field(row, 1)? == "1";
            Ok(node(
                path,
                kind,
                field(row, 0)?,
                is_relation,
                has_definition,
            ))
        })
        .collect()
}

async fn relation_children(
    conn: &mut Conn,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let (kind, has_definition, sql) = match folder {
        FolderKind::Columns => (
            NodeKind::Column,
            false,
            "SELECT column_name, IF(is_nullable = 'NO', CONCAT(column_type, ', not null'), \
             column_type) FROM information_schema.columns \
             WHERE table_schema = ? AND table_name = ? ORDER BY ordinal_position",
        ),
        // Functional indexes have no column name, which GROUP_CONCAT leaves out.
        FolderKind::Indexes => (
            NodeKind::Index,
            false,
            "SELECT index_name, CONCAT(IF(MIN(non_unique) = 0, 'unique ', ''), '(', \
             IFNULL(GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', '), ''), ')') \
             FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? \
             GROUP BY index_name ORDER BY index_name",
        ),
        FolderKind::Constraints => (
            NodeKind::Constraint,
            false,
            "SELECT constraint_name, LOWER(constraint_type) \
             FROM information_schema.table_constraints \
             WHERE table_schema = ? AND table_name = ? ORDER BY constraint_name",
        ),
        FolderKind::Triggers => (
            NodeKind::Trigger,
            true,
            "SELECT trigger_name, LOWER(CONCAT(action_timing, ' ', event_manipulation)) \
             FROM information_schema.triggers \
             WHERE event_object_schema = ? AND event_object_table = ? ORDER BY trigger_name",
        ),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    query(conn, sql, (schema, relation))
        .await?
        .iter()
        .map(|row| {
            Ok(CatalogNode {
                detail: row.get(1).cloned().flatten(),
                ..node(path, kind, field(row, 0)?, false, has_definition)
            })
        })
        .collect()
}

/// The object's `SHOW CREATE …` statement, and which of its result's columns holds the
/// definition.
fn show_create(path: &CatalogPath) -> Option<(String, usize)> {
    let object = path.last()?;
    let schema = path.name_of(NodeKind::Schema)?;
    let name = format!(
        "{}.{}",
        MYSQL_QUOTING.identifier(schema),
        MYSQL_QUOTING.identifier(&object.name)
    );
    let (what, column) = match object.kind {
        NodeKind::Table => ("TABLE", 1),
        NodeKind::View => ("VIEW", 1),
        NodeKind::Function => ("FUNCTION", 2),
        NodeKind::Procedure => ("PROCEDURE", 2),
        NodeKind::Event => ("EVENT", 3),
        NodeKind::Trigger => ("TRIGGER", 2),
        _ => return None,
    };
    Some((format!("SHOW CREATE {what} {name}"), column))
}

pub(crate) async fn definition(conn: &mut Conn, path: &CatalogPath) -> Result<String, DriverError> {
    // Only paths `children` returns: the level above must be a valid one.
    let valid_parent = path
        .parent()
        .is_some_and(|parent| Level::of(&parent).is_some());
    let (sql, column) = show_create(path)
        .filter(|_| valid_parent)
        .ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let row: Option<mysql_async::Row> = conn.query_first(sql).await.map_err(connection_error)?;
    let definition = row
        .and_then(|row| row.unwrap_raw().into_iter().nth(column).flatten())
        .and_then(text);
    // SHOW CREATE FUNCTION and PROCEDURE answer NULL to a user who may run the routine but
    // not read its source.
    definition.ok_or_else(|| {
        connection_error(format!(
            "the server shows no definition of {} to this user",
            path.last().map_or("", |object| object.name.as_str())
        ))
    })
}
