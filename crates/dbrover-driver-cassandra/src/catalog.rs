//! Catalog introspection from `system_schema`, one query per tree level, with definitions from
//! the server's own `DESCRIBE`.
//!
//! A keyspace holds its tables directly, as a MySQL database does, so keyspaces are listed as
//! schemas at the root and names qualify as `keyspace.table`.

use std::collections::{HashMap, HashSet};

use dbrover_core::{
    CatalogNode, CatalogPath, DriverError, FolderKind, NodeKind, PathSegment, Quoting,
};
use dbrover_sql::Dialect;
use scylla::DeserializeRow;
use scylla::client::session::Session;
use scylla::deserialize::row::DeserializeRow as DeserializeRowTrait;
use scylla::serialize::row::SerializeRow;

use crate::errors;

const KEYSPACE_FOLDERS: &[FolderKind] = &[
    FolderKind::Tables,
    FolderKind::MaterializedViews,
    FolderKind::Types,
    FolderKind::Functions,
    FolderKind::Aggregates,
];

/// What `DESCRIBE` answers, a row per object: its keyspace, kind and name, then the statement
/// that makes it, the last column.
type Described = (String, String, String, String);

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    Keyspaces,
    KeyspaceFolders,
    KeyspaceObjects {
        keyspace: &'a str,
        folder: FolderKind,
    },
    RelationFolders {
        relation_kind: NodeKind,
    },
    RelationChildren {
        keyspace: &'a str,
        relation: &'a str,
        folder: FolderKind,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        let [keyspace, rest @ ..] = path.segments() else {
            return Some(Self::Keyspaces);
        };
        if keyspace.kind != NodeKind::Schema {
            return None;
        }
        let keyspace = keyspace.name.as_str();
        match rest {
            [] => Some(Self::KeyspaceFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| KEYSPACE_FOLDERS.contains(folder))
                .map(|folder| Self::KeyspaceObjects { keyspace, folder }),
            [folder, relation] if is_object_in(folder, relation) => {
                (!relation_folders(relation.kind).is_empty()).then_some(Self::RelationFolders {
                    relation_kind: relation.kind,
                })
            }
            [folder, relation, child] if is_object_in(folder, relation) => folder_kind(child)
                .filter(|child| relation_folders(relation.kind).contains(child))
                .map(|folder| Self::RelationChildren {
                    keyspace,
                    relation: &relation.name,
                    folder,
                }),
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

/// The folder a keyspace's objects of `kind` are listed in.
fn folder_of(kind: NodeKind) -> Option<FolderKind> {
    match kind {
        NodeKind::Table => Some(FolderKind::Tables),
        NodeKind::MaterializedView => Some(FolderKind::MaterializedViews),
        NodeKind::UserType => Some(FolderKind::Types),
        NodeKind::Function => Some(FolderKind::Functions),
        NodeKind::Aggregate => Some(FolderKind::Aggregates),
        _ => None,
    }
}

fn is_object_in(folder: &PathSegment, object: &PathSegment) -> bool {
    folder_of(object.kind).is_some_and(|expected| folder_kind(folder) == Some(expected))
}

/// A materialized view's indexes are its base table's.
fn relation_folders(kind: NodeKind) -> &'static [FolderKind] {
    match kind {
        NodeKind::Table => &[FolderKind::Columns, FolderKind::Indexes],
        NodeKind::MaterializedView => &[FolderKind::Columns],
        _ => &[],
    }
}

pub(crate) async fn children(
    session: &Session,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    match Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))? {
        Level::Keyspaces => keyspaces(session, path).await,
        Level::KeyspaceFolders => Ok(folders(path, KEYSPACE_FOLDERS)),
        Level::KeyspaceObjects { keyspace, folder } => {
            keyspace_objects(session, path, keyspace, folder).await
        }
        Level::RelationFolders { relation_kind } => {
            Ok(folders(path, relation_folders(relation_kind)))
        }
        Level::RelationChildren {
            keyspace,
            relation,
            folder: FolderKind::Columns,
        } => columns(session, path, keyspace, relation).await,
        Level::RelationChildren {
            keyspace, relation, ..
        } => indexes(session, path, keyspace, relation).await,
    }
}

fn folders(parent: &CatalogPath, folders: &[FolderKind]) -> Vec<CatalogNode> {
    folders
        .iter()
        .map(|folder| CatalogNode::folder(parent, *folder))
        .collect()
}

async fn keyspaces(session: &Session, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let rows: Vec<(String,)> = rows(
        session,
        "SELECT keyspace_name FROM system_schema.keyspaces",
        (),
    )
    .await?;
    Ok(sorted(rows.into_iter().map(|(name,)| {
        let system = Dialect::Cassandra.is_system_name(Some(&name), "");
        CatalogNode {
            system,
            ..relation(path, NodeKind::Schema, name, None)
        }
    })))
}

async fn keyspace_objects(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let nodes = match folder {
        FolderKind::Tables => tables(session, path, keyspace).await?,
        FolderKind::MaterializedViews => views(session, path, keyspace).await?,
        FolderKind::Types => types(session, path, keyspace).await?,
        FolderKind::Functions => routines(session, path, keyspace, Routine::Function).await?,
        FolderKind::Aggregates => routines(session, path, keyspace, Routine::Aggregate).await?,
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    Ok(sorted(nodes))
}

async fn tables(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows: Vec<(String,)> = rows(
        session,
        "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?",
        (keyspace,),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name,)| relation(path, NodeKind::Table, name, None))
        .collect())
}

/// The keyspace's materialized views, each with the table it is a view of. ScyllaDB keeps the
/// view behind each secondary index here too, named after the index with `_index`; those are
/// the index's own, listed under their table's indexes rather than here.
async fn views(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let views: Vec<(String, String)> = rows(
        session,
        "SELECT view_name, base_table_name FROM system_schema.views WHERE keyspace_name = ?",
        (keyspace,),
    )
    .await?;
    let indexes: Vec<(String,)> = rows(
        session,
        "SELECT index_name FROM system_schema.indexes WHERE keyspace_name = ?",
        (keyspace,),
    )
    .await?;
    let backing: HashSet<String> = indexes
        .into_iter()
        .map(|(index,)| format!("{index}_index"))
        .collect();
    Ok(views
        .into_iter()
        .filter(|(name, _)| !backing.contains(name))
        .map(|(name, base)| {
            relation(
                path,
                NodeKind::MaterializedView,
                name,
                Some(format!("of {base}")),
            )
        })
        .collect())
}

/// A keyspace, a table or a view: a node with children, and a definition.
fn relation(
    path: &CatalogPath,
    kind: NodeKind,
    name: String,
    detail: Option<String>,
) -> CatalogNode {
    CatalogNode {
        has_children: true,
        has_definition: true,
        ..node(path, kind, name, detail)
    }
}

async fn types(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows: Vec<(String, Option<Vec<String>>)> = rows(
        session,
        "SELECT type_name, field_names FROM system_schema.types WHERE keyspace_name = ?",
        (keyspace,),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, fields)| {
            let fields = fields.unwrap_or_default();
            let detail = Some(format!(
                "{} field{}",
                fields.len(),
                if fields.len() == 1 { "" } else { "s" }
            ));
            definable(path, NodeKind::UserType, name, detail)
        })
        .collect())
}

/// A user-defined function or aggregate: named with its argument types, as overloads are told
/// apart, and with what it returns.
#[derive(Clone, Copy)]
enum Routine {
    Function,
    Aggregate,
}

impl Routine {
    fn query(self) -> &'static str {
        match self {
            Self::Function => {
                "SELECT function_name, argument_types, return_type FROM system_schema.functions \
                 WHERE keyspace_name = ?"
            }
            Self::Aggregate => {
                "SELECT aggregate_name, argument_types, return_type FROM system_schema.aggregates \
                 WHERE keyspace_name = ?"
            }
        }
    }

    fn kind(self) -> NodeKind {
        match self {
            Self::Function => NodeKind::Function,
            Self::Aggregate => NodeKind::Aggregate,
        }
    }
}

async fn routines(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
    routine: Routine,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows: Vec<(String, Option<Vec<String>>, Option<String>)> =
        rows(session, routine.query(), (keyspace,)).await?;
    Ok(rows
        .into_iter()
        .map(|(name, arguments, returns)| {
            let label = format!("{name}({})", arguments.unwrap_or_default().join(", "));
            let detail = returns.map(|returns| format!("returns {returns}"));
            definable(path, routine.kind(), label, detail)
        })
        .collect())
}

/// One of a relation's columns, as `system_schema.columns` lists it.
#[derive(DeserializeRow)]
#[scylla(crate = scylla)]
pub(crate) struct ColumnRow {
    pub(crate) column_name: String,
    pub(crate) kind: String,
    position: i32,
    #[scylla(rename = "type")]
    pub(crate) column_type: String,
    clustering_order: Option<String>,
}

impl ColumnRow {
    /// Where the column sits in the key: partition key columns first, then clustering ones,
    /// each in key order, then the static columns and the rest by name.
    pub(crate) fn order(&self) -> (u8, i32, String) {
        let rank = match self.kind.as_str() {
            "partition_key" => 0,
            "clustering" => 1,
            "static" => 2,
            _ => 3,
        };
        (rank, self.position, self.column_name.clone())
    }

    fn detail(&self) -> String {
        let descending = self
            .clustering_order
            .as_deref()
            .is_some_and(|order| order.eq_ignore_ascii_case("desc"));
        let role = match (self.kind.as_str(), descending) {
            ("partition_key", _) => Some("partition key".to_owned()),
            ("clustering", true) => Some("clustering key, descending".to_owned()),
            ("clustering", _) => Some("clustering key".to_owned()),
            ("static", _) => Some("static".to_owned()),
            _ => None,
        };
        match role {
            Some(role) => format!("{} · {role}", self.column_type),
            None => self.column_type.clone(),
        }
    }
}

/// A table's or a view's columns, in key order (see [`ColumnRow::order`]).
pub(crate) async fn column_rows(
    session: &Session,
    keyspace: &str,
    relation: &str,
) -> Result<Vec<ColumnRow>, DriverError> {
    let mut rows: Vec<ColumnRow> = rows(
        session,
        "SELECT column_name, kind, position, type, clustering_order FROM system_schema.columns \
         WHERE keyspace_name = ? AND table_name = ?",
        (keyspace, relation),
    )
    .await?;
    rows.sort_by_key(ColumnRow::order);
    Ok(rows)
}

async fn columns(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows = column_rows(session, keyspace, relation).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let detail = Some(row.detail());
            node(path, NodeKind::Column, row.column_name, detail)
        })
        .collect())
}

async fn indexes(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
    table: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows: Vec<(String, Option<HashMap<String, String>>)> = rows(
        session,
        "SELECT index_name, options FROM system_schema.indexes \
         WHERE keyspace_name = ? AND table_name = ?",
        (keyspace, table),
    )
    .await?;
    Ok(sorted(rows.into_iter().map(|(name, options)| {
        let target = options.and_then(|mut options| options.remove("target"));
        definable(
            path,
            NodeKind::Index,
            name,
            target.map(|target| format!("on {target}")),
        )
    })))
}

fn node(path: &CatalogPath, kind: NodeKind, name: String, detail: Option<String>) -> CatalogNode {
    CatalogNode {
        path: path.child(kind, &name),
        label: name,
        detail,
        has_children: false,
        has_definition: false,
        system: false,
    }
}

fn definable(
    path: &CatalogPath,
    kind: NodeKind,
    name: String,
    detail: Option<String>,
) -> CatalogNode {
    CatalogNode {
        has_definition: true,
        ..node(path, kind, name, detail)
    }
}

/// Nodes in the order the explorer lists them: by name, ignoring case. The server lists them
/// in token order, which reads as no order at all.
fn sorted(nodes: impl IntoIterator<Item = CatalogNode>) -> Vec<CatalogNode> {
    let mut nodes: Vec<CatalogNode> = nodes.into_iter().collect();
    nodes.sort_by_key(|node| node.label.to_lowercase());
    nodes
}

/// The statements that make the object at `path` again, from the server's `DESCRIBE`, which
/// ScyllaDB and Cassandra 4.0 and later answer as rows. A table's comes with its indexes and
/// views; a function's or an aggregate's with every overload of its name.
pub(crate) async fn definition(
    session: &Session,
    path: &CatalogPath,
) -> Result<String, DriverError> {
    let describe =
        describe_statement(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let described: Vec<Described> = rows(session, &describe, ()).await?;
    let statements: Vec<String> = described
        .into_iter()
        .map(|(_, _, _, statement)| statement.trim_end().to_owned())
        .collect();
    if statements.is_empty() {
        return Err(DriverError::InvalidPath(path.clone()));
    }
    Ok(statements.join("\n\n"))
}

/// The `DESCRIBE` for the object at `path`, or `None` for a node without a definition.
fn describe_statement(path: &CatalogPath) -> Option<String> {
    let quote = |name: &str| Quoting::STANDARD.identifier_if_needed(name);
    match path.segments() {
        [keyspace] if keyspace.kind == NodeKind::Schema => {
            Some(format!("DESCRIBE ONLY KEYSPACE {}", quote(&keyspace.name)))
        }
        [keyspace, folder, object] if is_object_in(folder, object) => {
            let object_kind = match object.kind {
                NodeKind::Table => "TABLE",
                NodeKind::MaterializedView => "MATERIALIZED VIEW",
                NodeKind::UserType => "TYPE",
                NodeKind::Function => "FUNCTION",
                NodeKind::Aggregate => "AGGREGATE",
                _ => return None,
            };
            // A function's or an aggregate's node is named with its argument types, which
            // `DESCRIBE` doesn't take.
            let name = object.name.split('(').next().unwrap_or(&object.name);
            Some(format!(
                "DESCRIBE {object_kind} {}.{}",
                quote(&keyspace.name),
                quote(name)
            ))
        }
        [keyspace, folder, table, indexes, index]
            if is_object_in(folder, table)
                && table.kind == NodeKind::Table
                && indexes.kind == NodeKind::Folder(FolderKind::Indexes)
                && index.kind == NodeKind::Index =>
        {
            Some(format!(
                "DESCRIBE INDEX {}.{}",
                quote(&keyspace.name),
                quote(&index.name)
            ))
        }
        _ => None,
    }
}

/// Every row of `cql`, run with `values` bound.
pub(crate) async fn rows<R>(
    session: &Session,
    cql: &str,
    values: impl SerializeRow,
) -> Result<Vec<R>, DriverError>
where
    R: for<'frame, 'metadata> DeserializeRowTrait<'frame, 'metadata>,
{
    let result = session
        .query_unpaged(cql, values)
        .await
        .map_err(errors::connection)?
        .into_rows_result()
        .map_err(errors::connection)?;
    result
        .rows::<R>()
        .map_err(errors::connection)?
        .map(|row| row.map_err(errors::connection))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_each_kind_of_object_by_its_name() {
        let keyspace = CatalogPath::root().child(NodeKind::Schema, "Shop");
        let tables = keyspace.child(NodeKind::Folder(FolderKind::Tables), "tables");
        let table = tables.child(NodeKind::Table, "orders");
        let functions = keyspace.child(NodeKind::Folder(FolderKind::Functions), "functions");
        let cases = [
            (keyspace.clone(), Some("DESCRIBE ONLY KEYSPACE \"Shop\"")),
            (table.clone(), Some("DESCRIBE TABLE \"Shop\".orders")),
            (
                functions.child(NodeKind::Function, "twice(int)"),
                Some("DESCRIBE FUNCTION \"Shop\".twice"),
            ),
            (
                table
                    .child(NodeKind::Folder(FolderKind::Indexes), "indexes")
                    .child(NodeKind::Index, "by_customer"),
                Some("DESCRIBE INDEX \"Shop\".by_customer"),
            ),
            (
                table
                    .child(NodeKind::Folder(FolderKind::Columns), "columns")
                    .child(NodeKind::Column, "id"),
                None,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(describe_statement(&path).as_deref(), expected, "{path}");
        }
    }
}
