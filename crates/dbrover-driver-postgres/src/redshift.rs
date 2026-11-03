//! Amazon Redshift's catalog (C1d, `docs/cloud-data.md`), read through its `svv_all_*` views.
//!
//! Redshift speaks PostgreSQL 8.0's protocol, but `pg_class` doesn't hold its external
//! (Spectrum) tables or late-binding views' columns, and it has no sequences, indexes or
//! triggers to list. The `svv_all_*` views cover local, external and shared tables of every
//! database the user may see, so every level is read on the connection's own database rather
//! than on a connection per database, as PostgreSQL's catalog does.
//!
//! Redshift takes no arrays as parameters, and has no `COLLATE "C"`: levels are listed whole
//! and filtered here, and nothing here sends either.

use dbrover_core::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, DriverError, FolderKind, NodeKind,
    PathSegment, Quoting, like_contains,
};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{Client, Row, SimpleQueryMessage};

use crate::connection_error;

const SCHEMA_FOLDERS: &[FolderKind] = &[FolderKind::Tables, FolderKind::Views];
const RELATION_FOLDERS: &[FolderKind] = &[FolderKind::Columns];

/// `svv_all_tables.table_type` of a view; every other type (`TABLE`, `EXTERNAL TABLE`, a
/// datashare's) is listed with the tables.
const VIEW_TYPE: &str = "VIEW";
const EXTERNAL_TYPE: &str = "EXTERNAL TABLE";

/// Whether `version()`'s text is Redshift's: `PostgreSQL 8.0.2 on i686-pc-linux-gnu, compiled
/// by GCC gcc (GCC) 3.4.2 20041017 (Red Hat 3.4.2-6.fc3), Redshift 1.0.77467`.
pub(crate) fn redshift_version(version: &str) -> Option<&str> {
    let (_, after) = version.rsplit_once("Redshift ")?;
    Some(after.split_whitespace().next().unwrap_or(after))
}

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    Databases,
    Schemas {
        database: &'a str,
    },
    SchemaFolders,
    Relations {
        database: &'a str,
        schema: &'a str,
        folder: FolderKind,
    },
    RelationFolders,
    Columns {
        database: &'a str,
        schema: &'a str,
        relation: &'a str,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        let is = |segment: &PathSegment, kind| segment.kind == kind;
        match path.segments() {
            [] => Some(Self::Databases),
            [database] if is(database, NodeKind::Database) => Some(Self::Schemas {
                database: &database.name,
            }),
            [database, schema, rest @ ..]
                if is(database, NodeKind::Database) && is(schema, NodeKind::Schema) =>
            {
                Self::within_schema(&database.name, &schema.name, rest)
            }
            _ => None,
        }
    }

    fn within_schema(database: &'a str, schema: &'a str, rest: &'a [PathSegment]) -> Option<Self> {
        match rest {
            [] => Some(Self::SchemaFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| SCHEMA_FOLDERS.contains(folder))
                .map(|folder| Self::Relations {
                    database,
                    schema,
                    folder,
                }),
            [folder, relation] if is_relation_in(folder, relation) => Some(Self::RelationFolders),
            [folder, relation, columns]
                if is_relation_in(folder, relation)
                    && folder_kind(columns) == Some(FolderKind::Columns) =>
            {
                Some(Self::Columns {
                    database,
                    schema,
                    relation: &relation.name,
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

fn relation_kind(folder: FolderKind) -> Option<NodeKind> {
    match folder {
        FolderKind::Tables => Some(NodeKind::Table),
        FolderKind::Views => Some(NodeKind::View),
        _ => None,
    }
}

fn is_relation_in(folder: &PathSegment, relation: &PathSegment) -> bool {
    folder_kind(folder).and_then(relation_kind) == Some(relation.kind)
}

/// Redshift's own schemas, and PostgreSQL's it keeps.
fn is_system_schema(name: &str) -> bool {
    matches!(
        name,
        "pg_catalog" | "information_schema" | "pg_internal" | "pg_automv" | "catalog_history"
    ) || name.starts_with("pg_toast")
        || name.starts_with("pg_temp_")
}

pub(crate) async fn children(
    client: &Client,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    match level {
        Level::Databases => databases(client, path).await,
        Level::Schemas { database } => schemas(client, path, database).await,
        Level::SchemaFolders => Ok(folders(path, SCHEMA_FOLDERS)),
        Level::Relations {
            database,
            schema,
            folder,
        } => relations(client, path, database, schema, folder).await,
        Level::RelationFolders => Ok(folders(path, RELATION_FOLDERS)),
        Level::Columns {
            database,
            schema,
            relation,
        } => columns(client, path, database, schema, relation).await,
    }
}

fn folders(parent: &CatalogPath, folders: &[FolderKind]) -> Vec<CatalogNode> {
    folders
        .iter()
        .map(|folder| CatalogNode::folder(parent, *folder))
        .collect()
}

fn node(parent: &CatalogPath, kind: NodeKind, name: String) -> CatalogNode {
    CatalogNode {
        path: parent.child(kind, name.clone()),
        label: name,
        detail: None,
        has_children: false,
        has_definition: false,
        system: false,
    }
}

/// Runs a catalog query whose parameters are all text, as an unnamed statement (see
/// `catalog::query`).
async fn query(
    client: &Client,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Vec<Row>, DriverError> {
    let typed: Vec<_> = params.iter().map(|param| (*param, Type::TEXT)).collect();
    client
        .query_typed(sql, &typed)
        .await
        .map_err(connection_error)
}

fn text(row: &Row, column: usize) -> Result<String, DriverError> {
    row.try_get(column).map_err(connection_error)
}

async fn databases(client: &Client, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT database_name::text FROM svv_redshift_databases ORDER BY 1";
    query(client, sql, &[])
        .await?
        .iter()
        .map(|row| {
            Ok(CatalogNode {
                has_children: true,
                ..node(path, NodeKind::Database, text(row, 0)?)
            })
        })
        .collect()
}

async fn schemas(
    client: &Client,
    path: &CatalogPath,
    database: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT schema_name::text FROM svv_all_schemas \
               WHERE database_name = $1 ORDER BY 1";
    query(client, sql, &[&database])
        .await?
        .iter()
        .map(|row| {
            let name = text(row, 0)?;
            Ok(CatalogNode {
                has_children: true,
                system: is_system_schema(&name),
                ..node(path, NodeKind::Schema, name)
            })
        })
        .collect()
}

/// A schema's tables or views. Only the connected database's have a definition: `SHOW TABLE`
/// and `SHOW VIEW` read nowhere else.
async fn relations(
    client: &Client,
    path: &CatalogPath,
    database: &str,
    schema: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let kind = relation_kind(folder).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let wanted = if kind == NodeKind::View { "=" } else { "<>" };
    let sql = format!(
        "SELECT table_name::text, table_type::text, \
         (database_name = current_database())::text FROM svv_all_tables \
         WHERE database_name = $1 AND schema_name = $2 AND table_type {wanted} '{VIEW_TYPE}' \
         ORDER BY 1"
    );
    query(client, &sql, &[&database, &schema])
        .await?
        .iter()
        .map(|row| {
            let external = text(row, 1)? == EXTERNAL_TYPE;
            Ok(CatalogNode {
                detail: external.then(|| "external".to_owned()),
                has_children: true,
                has_definition: is_true(&text(row, 2)?) && !external,
                ..node(path, kind, text(row, 0)?)
            })
        })
        .collect()
}

/// `true` as `bool::text` writes it on PostgreSQL (`true`) and on Redshift (`true`, or `t`
/// from older clusters).
fn is_true(text: &str) -> bool {
    matches!(text, "true" | "t")
}

async fn columns(
    client: &Client,
    path: &CatalogPath,
    database: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT column_name::text, data_type::text, is_nullable::text \
               FROM svv_all_columns \
               WHERE database_name = $1 AND schema_name = $2 AND table_name = $3 \
               ORDER BY ordinal_position";
    query(client, sql, &[&database, &schema, &relation])
        .await?
        .iter()
        .map(|row| {
            let data_type = text(row, 1)?;
            let detail = if text(row, 2)? == "NO" {
                format!("{data_type}, not null")
            } else {
                data_type
            };
            Ok(CatalogNode {
                detail: Some(detail),
                ..node(path, NodeKind::Column, text(row, 0)?)
            })
        })
        .collect()
}

/// The connected database's tables and views whose names contain the search's text, by name.
pub(crate) async fn search(
    client: &Client,
    search: &CatalogSearch,
) -> Result<CatalogMatches, DriverError> {
    // A number DbRover chose, written into the SQL rather than sent as a parameter.
    let read = search.limit.saturating_add(1);
    let sql = format!(
        "SELECT database_name::text, schema_name::text, table_name::text, table_type::text \
         FROM svv_all_tables \
         WHERE database_name = current_database() AND table_name ILIKE $1 \
         ORDER BY table_name, schema_name LIMIT {read}"
    );
    let pattern = like_contains(&search.text);
    let rows = query(client, &sql, &[&pattern]).await?;
    let nodes = rows
        .iter()
        .map(|row| {
            let (database, schema, name) = (text(row, 0)?, text(row, 1)?, text(row, 2)?);
            let table_type = text(row, 3)?;
            let (kind, folder) = if table_type == VIEW_TYPE {
                (NodeKind::View, FolderKind::Views)
            } else {
                (NodeKind::Table, FolderKind::Tables)
            };
            let folder_path = CatalogPath::root()
                .child(NodeKind::Database, database)
                .child(NodeKind::Schema, schema.clone())
                .child(NodeKind::Folder(folder), folder.label());
            Ok(CatalogNode {
                detail: Some(schema.clone()),
                system: is_system_schema(&schema),
                has_children: true,
                has_definition: table_type != EXTERNAL_TYPE,
                ..node(&folder_path, kind, name)
            })
        })
        .collect::<Result<Vec<_>, DriverError>>()?;
    Ok(CatalogMatches::cut(nodes, search.limit))
}

/// The statement `SHOW TABLE` or `SHOW VIEW` answers with for a relation of the connected
/// database, which is how Redshift keeps a table's DDL: there is no `pg_get_tabledef`.
pub(crate) fn definition_statement(path: &CatalogPath) -> Option<String> {
    let [database, schema, folder, relation] = path.segments() else {
        return None;
    };
    let in_place = database.kind == NodeKind::Database
        && schema.kind == NodeKind::Schema
        && is_relation_in(folder, relation);
    let keyword = match relation.kind {
        NodeKind::Table => "TABLE",
        _ => "VIEW",
    };
    in_place.then(|| {
        format!(
            "SHOW {keyword} {}.{}",
            Quoting::STANDARD.identifier(&schema.name),
            Quoting::STANDARD.identifier(&relation.name)
        )
    })
}

pub(crate) async fn definition(client: &Client, path: &CatalogPath) -> Result<String, DriverError> {
    let statement =
        definition_statement(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let messages = client
        .simple_query(&statement)
        .await
        .map_err(connection_error)?;
    messages
        .iter()
        .find_map(|message| match message {
            SimpleQueryMessage::Row(row) => row.get(0),
            _ => None,
        })
        .map(str::to_owned)
        .ok_or_else(|| DriverError::InvalidPath(path.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_redshifts_version_and_not_postgresqls() {
        for (version, expected) in [
            (
                "PostgreSQL 8.0.2 on i686-pc-linux-gnu, compiled by GCC gcc (GCC) 3.4.2 \
                 20041017 (Red Hat 3.4.2-6.fc3), Redshift 1.0.77467",
                Some("1.0.77467"),
            ),
            (
                "PostgreSQL 8.0.2 on i686-pc-linux-gnu, compiled by GCC gcc (GCC) 3.4.2 \
                 20041017 (Red Hat 3.4.2-6.fc3), Redshift 1.0.12103",
                Some("1.0.12103"),
            ),
            (
                "PostgreSQL 17.6 (Debian 17.6-1.pgdg13+1) on aarch64-unknown-linux-gnu, \
                 compiled by gcc (Debian 14.2.0-19) 14.2.0, 64-bit",
                None,
            ),
        ] {
            assert_eq!(redshift_version(version), expected, "{version}");
        }
    }

    #[test]
    fn shows_the_ddl_of_tables_and_views_only() {
        let schema = CatalogPath::root()
            .child(NodeKind::Database, "dev")
            .child(NodeKind::Schema, "Sales");
        let tables = schema.child(NodeKind::Folder(FolderKind::Tables), "Tables");
        let views = schema.child(NodeKind::Folder(FolderKind::Views), "Views");
        let table = tables.child(NodeKind::Table, "orders");
        for (path, expected) in [
            (table.clone(), Some(r#"SHOW TABLE "Sales"."orders""#)),
            (
                views.child(NodeKind::View, "big \"orders\""),
                Some(r#"SHOW VIEW "Sales"."big ""orders""""#),
            ),
            (
                table
                    .child(NodeKind::Folder(FolderKind::Columns), "Columns")
                    .child(NodeKind::Column, "id"),
                None,
            ),
            (views.child(NodeKind::Table, "misfiled"), None),
        ] {
            assert_eq!(definition_statement(&path).as_deref(), expected, "{path}");
        }
    }
}
