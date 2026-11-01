//! Catalog introspection from `system_schema`, one query per tree level.
//!
//! A keyspace holds its tables directly, as a MySQL database does, so keyspaces are listed as
//! schemas at the root and names qualify as `keyspace.table`.

use dbrover_core::{CatalogNode, CatalogPath, DriverError, FolderKind, NodeKind, PathSegment};
use dbrover_sql::Dialect;
use scylla::client::session::Session;

use crate::errors;

const KEYSPACE_FOLDERS: &[FolderKind] = &[FolderKind::Tables];

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    Keyspaces,
    KeyspaceFolders,
    Tables { keyspace: &'a str },
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
            [folder] if is_folder(folder, FolderKind::Tables) => Some(Self::Tables { keyspace }),
            _ => None,
        }
    }
}

fn is_folder(segment: &PathSegment, folder: FolderKind) -> bool {
    segment.kind == NodeKind::Folder(folder)
}

pub(crate) async fn children(
    session: &Session,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    match Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))? {
        Level::Keyspaces => keyspaces(session, path).await,
        Level::KeyspaceFolders => Ok(KEYSPACE_FOLDERS
            .iter()
            .map(|folder| CatalogNode::folder(path, *folder))
            .collect()),
        Level::Tables { keyspace } => tables(session, path, keyspace).await,
    }
}

/// No node has a definition until the explorer's layer (N4·4).
pub(crate) async fn definition(
    _session: &Session,
    path: &CatalogPath,
) -> Result<String, DriverError> {
    Err(DriverError::InvalidPath(path.clone()))
}

async fn keyspaces(session: &Session, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let names = texts(
        session,
        "SELECT keyspace_name FROM system_schema.keyspaces",
        (),
    )
    .await?;
    Ok(sorted(names)
        .into_iter()
        .map(|name| CatalogNode {
            path: path.child(NodeKind::Schema, &name),
            system: Dialect::Cassandra.is_system_name(Some(&name), ""),
            label: name,
            detail: None,
            has_children: true,
            has_definition: false,
        })
        .collect())
}

async fn tables(
    session: &Session,
    path: &CatalogPath,
    keyspace: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let names = texts(
        session,
        "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?",
        (keyspace,),
    )
    .await?;
    Ok(sorted(names)
        .into_iter()
        .map(|name| CatalogNode {
            path: path.child(NodeKind::Table, &name),
            label: name,
            detail: None,
            has_children: false,
            has_definition: false,
            system: false,
        })
        .collect())
}

/// The first column of every row of `cql`, run with `values` bound.
async fn texts(
    session: &Session,
    cql: &str,
    values: impl scylla::serialize::row::SerializeRow,
) -> Result<Vec<String>, DriverError> {
    let result = session
        .query_unpaged(cql, values)
        .await
        .map_err(errors::connection)?
        .into_rows_result()
        .map_err(errors::connection)?;
    result
        .rows::<(String,)>()
        .map_err(errors::connection)?
        .map(|row| row.map(|(text,)| text).map_err(errors::connection))
        .collect()
}

/// Names in the order the explorer lists them: by name, ignoring case. The server lists them
/// in token order, which reads as no order at all.
fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort_by_key(|name| name.to_lowercase());
    names
}
