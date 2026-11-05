//! The explorer's tree: catalogs on top (`SHOW CATALOGS`), then each catalog's schemas,
//! tables and views, and columns, from its own `information_schema`, which every connector
//! has. Definitions are `SHOW CREATE TABLE` and `SHOW CREATE VIEW`.

use dbrover_core::{CatalogNode, CatalogPath, DriverError, NodeKind, Quoting, Row};

use crate::statement::Runner;

/// The catalog Trino keeps for itself (its nodes, queries and the JDBC metadata), hidden
/// unless **System** is ticked.
const SYSTEM_CATALOG: &str = "system";

/// Every catalog's own schema of metadata.
const INFORMATION_SCHEMA: &str = "information_schema";

/// What a path names, by its shape.
enum Level<'a> {
    Catalogs,
    Schemas {
        catalog: &'a str,
    },
    Tables {
        catalog: &'a str,
        schema: &'a str,
    },
    Columns {
        catalog: &'a str,
        schema: &'a str,
        table: &'a str,
        kind: NodeKind,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        let names: Vec<(NodeKind, &str)> = path
            .segments()
            .iter()
            .map(|segment| (segment.kind, segment.name.as_str()))
            .collect();
        Some(match names.as_slice() {
            [] => Self::Catalogs,
            [(NodeKind::Database, catalog)] => Self::Schemas { catalog },
            [(NodeKind::Database, catalog), (NodeKind::Schema, schema)] => {
                Self::Tables { catalog, schema }
            }
            [
                (NodeKind::Database, catalog),
                (NodeKind::Schema, schema),
                (kind @ (NodeKind::Table | NodeKind::View), table),
            ] => Self::Columns {
                catalog,
                schema,
                table,
                kind: *kind,
            },
            _ => return None,
        })
    }
}

fn quoted(name: &str) -> String {
    Quoting::STANDARD.identifier(name)
}

fn literal(text: &str) -> String {
    Quoting::STANDARD.string_literal(text)
}

/// The nodes under `path`.
///
/// # Errors
///
/// [`DriverError::InvalidPath`] for a path this driver never lists, and
/// [`DriverError::Connection`] when a catalog query fails.
pub(crate) async fn children(
    runner: &Runner,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    let node = |kind: NodeKind, row: &Row, has_children: bool, system: bool| {
        let name = first(row);
        CatalogNode {
            path: path.child(kind, name),
            label: name.to_owned(),
            detail: None,
            has_children,
            has_definition: matches!(kind, NodeKind::Table | NodeKind::View),
            system,
        }
    };
    Ok(match level {
        Level::Catalogs => runner
            .rows("SHOW CATALOGS")
            .await?
            .iter()
            .map(|row| node(NodeKind::Database, row, true, first(row) == SYSTEM_CATALOG))
            .collect(),
        Level::Schemas { catalog } => runner
            .rows(&format!(
                "SELECT schema_name FROM {}.information_schema.schemata ORDER BY schema_name",
                quoted(catalog)
            ))
            .await?
            .iter()
            .map(|row| {
                node(
                    NodeKind::Schema,
                    row,
                    true,
                    first(row) == INFORMATION_SCHEMA,
                )
            })
            .collect(),
        Level::Tables { catalog, schema } => runner
            .rows(&format!(
                "SELECT table_name, table_type FROM {}.information_schema.tables \
                 WHERE table_schema = {} ORDER BY table_name",
                quoted(catalog),
                literal(schema)
            ))
            .await?
            .iter()
            .map(|row| {
                let kind = match second(row) {
                    "VIEW" => NodeKind::View,
                    _ => NodeKind::Table,
                };
                node(kind, row, true, false)
            })
            .collect(),
        Level::Columns {
            catalog,
            schema,
            table,
            ..
        } => runner
            .rows(&format!(
                "SELECT column_name, data_type FROM {}.information_schema.columns \
                 WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
                quoted(catalog),
                literal(schema),
                literal(table)
            ))
            .await?
            .iter()
            .map(|row| CatalogNode {
                detail: Some(second(row).to_owned()),
                ..node(NodeKind::Column, row, false, false)
            })
            .collect(),
    })
}

/// The `CREATE` statement of the table or view at `path`.
///
/// # Errors
///
/// [`DriverError::InvalidPath`] for anything but a table or view, and
/// [`DriverError::Connection`] when Trino can't show it.
pub(crate) async fn definition(runner: &Runner, path: &CatalogPath) -> Result<String, DriverError> {
    let Some(Level::Columns {
        catalog,
        schema,
        table,
        kind,
    }) = Level::of(path)
    else {
        return Err(DriverError::InvalidPath(path.clone()));
    };
    let object = match kind {
        NodeKind::View => "VIEW",
        _ => "TABLE",
    };
    let rows = runner
        .rows(&format!(
            "SHOW CREATE {object} {}.{}.{}",
            quoted(catalog),
            quoted(schema),
            quoted(table)
        ))
        .await?;
    Ok(rows.first().map(first).unwrap_or_default().to_owned())
}

fn first(row: &Row) -> &str {
    row.first().and_then(Option::as_deref).unwrap_or_default()
}

fn second(row: &Row) -> &str {
    row.get(1).and_then(Option::as_deref).unwrap_or_default()
}
