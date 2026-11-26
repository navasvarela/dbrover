//! The tables a data search reads (W16, `docs/data-search.md`), with their columns and row
//! counts, from `system.columns` and `system.tables`.
//!
//! A ClickHouse table's primary key is a prefix of its sorting key, which doesn't make rows
//! unique; it is still what names a row best, so it is listed as the table's key, and a found
//! row opens with the rows that share its key's values.

use clickhouse::Client;
use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};

use crate::catalog::{INNER_TABLE_PREFIX, NOT_TABLES, SYSTEM_SCHEMAS, field};
use crate::execution::rows;

/// Every column of the tables (and views) asked for, in table order, with the table's row
/// count where its engine keeps one. A materialized view's rows are its inner table's, which
/// is left out, so the view is searched with the views.
fn sql(views: Relations) -> String {
    let engines = match views {
        Relations::TablesOnly => format!("t.engine NOT IN {NOT_TABLES}"),
        Relations::TablesAndViews => {
            format!("(t.engine NOT IN {NOT_TABLES} OR t.engine IN ('View', 'MaterializedView'))")
        }
    };
    format!(
        "SELECT c.database, c.table, t.engine, t.total_rows, c.name, c.type, \
         c.is_in_primary_key \
         FROM system.columns c \
         JOIN system.tables t ON t.database = c.database AND t.name = c.table \
         WHERE NOT t.is_temporary AND {engines} \
           AND NOT startsWith(t.name, '{INNER_TABLE_PREFIX}') \
         ORDER BY c.database, c.table, c.position"
    )
}

pub(crate) async fn searchable_tables(
    client: &Client,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let mut found: Vec<SearchableTable> = Vec::new();
    for row in rows(client, &sql(tables.views), &[]).await? {
        let (schema, name) = (field(&row, 0)?, field(&row, 1)?);
        let same = found
            .last()
            .is_some_and(|table| table.name.schema == schema && table.name.name == name);
        if !same {
            let estimate = row.get(3).cloned().flatten();
            found.push(table(schema, name, &field(&row, 2)?, estimate));
        }
        if let Some(table) = found.last_mut() {
            let column = field(&row, 4)?;
            if field(&row, 6)? == "1" {
                table.key.push(column.clone());
            }
            table
                .columns
                .push(SearchColumn::new(column, field(&row, 5)?));
        }
    }
    Ok(found
        .into_iter()
        .filter(|table| {
            let system = SYSTEM_SCHEMAS.contains(&table.name.schema.as_str());
            (!system || tables.scope.names_schema(&table.name.schema))
                && tables.scope.includes(&table.path)
        })
        .collect())
}

fn table(schema: String, name: String, engine: &str, estimate: Option<String>) -> SearchableTable {
    let (kind, folder, relation) = match engine {
        "View" => (NodeKind::View, FolderKind::Views, RelationKind::View),
        "MaterializedView" => (
            NodeKind::MaterializedView,
            FolderKind::MaterializedViews,
            RelationKind::View,
        ),
        _ => (NodeKind::Table, FolderKind::Tables, RelationKind::Table),
    };
    SearchableTable {
        database: None,
        path: CatalogPath::root()
            .child(NodeKind::Schema, schema.clone())
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name.clone()),
        name: TableName { schema, name },
        relation,
        key: Vec::new(),
        row_estimate: estimate.and_then(|rows| rows.parse().ok()),
        columns: Vec::new(),
    }
}
