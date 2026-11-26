//! The tables a data search reads (W16, `docs/data-search.md`), with their columns, keys and
//! row estimates. Blocking; called through `DuckDbConnection::run_blocking`.

use std::collections::HashMap;

use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};

use crate::connection_error;

/// Every column of every table and view in a database DuckDB doesn't keep for itself, with
/// the table's estimated size (`NULL` for a view). The `system` and `temp` databases are
/// internal, as are the `information_schema` and `pg_catalog` schemas.
const SEARCHABLE_COLUMNS: &str = "\
SELECT c.database_name, c.schema_name, c.table_name, \
       CASE WHEN t.table_name IS NULL THEN 'view' ELSE 'table' END, \
       t.estimated_size, c.column_name, c.data_type \
FROM duckdb_columns() c \
JOIN duckdb_databases() d ON d.database_name = c.database_name \
LEFT JOIN duckdb_tables() t ON t.database_name = c.database_name \
     AND t.schema_name = c.schema_name AND t.table_name = c.table_name \
WHERE NOT d.internal AND NOT c.internal \
  AND c.schema_name NOT IN ('information_schema', 'pg_catalog') \
ORDER BY c.database_name, c.schema_name, c.table_name, c.column_index";

/// Each primary key's columns, in key order.
const PRIMARY_KEYS: &str = "\
SELECT database_name, schema_name, table_name, \
       unnest(constraint_column_names), generate_subscripts(constraint_column_names, 1) \
FROM duckdb_constraints() WHERE constraint_type = 'PRIMARY KEY'";

type Relation = (String, String, String);

pub(crate) fn searchable_tables(
    connection: &duckdb::Connection,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let keys = primary_keys(connection)?;
    let mut statement = connection
        .prepare(SEARCHABLE_COLUMNS)
        .map_err(connection_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ),
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                SearchColumn::new(row.get::<_, String>(5)?, row.get::<_, String>(6)?),
            ))
        })
        .map_err(connection_error)?;
    let mut found: Vec<SearchableTable> = Vec::new();
    for row in rows {
        let (relation, relation_type, estimate, column) = row.map_err(connection_error)?;
        let same = found.last().is_some_and(|table| {
            table.database.as_deref() == Some(relation.0.as_str())
                && table.name.schema == relation.1
                && table.name.name == relation.2
        });
        if !same {
            if relation_type == "view" && tables.views == Relations::TablesOnly {
                continue;
            }
            let key = keys.get(&relation).cloned().unwrap_or_default();
            found.push(table(relation, &relation_type, estimate, key));
        }
        if let Some(table) = found.last_mut() {
            table.columns.push(column);
        }
    }
    Ok(found
        .into_iter()
        .filter(|table| tables.scope.includes(&table.path))
        .collect())
}

fn primary_keys(
    connection: &duckdb::Connection,
) -> Result<HashMap<Relation, Vec<String>>, DriverError> {
    let mut statement = connection.prepare(PRIMARY_KEYS).map_err(connection_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ),
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(connection_error)?;
    let mut keys: HashMap<Relation, Vec<(i64, String)>> = HashMap::new();
    for row in rows {
        let (relation, column, position) = row.map_err(connection_error)?;
        keys.entry(relation).or_default().push((position, column));
    }
    Ok(keys
        .into_iter()
        .map(|(relation, mut columns)| {
            columns.sort();
            (
                relation,
                columns.into_iter().map(|(_, name)| name).collect(),
            )
        })
        .collect())
}

fn table(
    (database, schema, name): Relation,
    relation_type: &str,
    estimate: Option<i64>,
    key: Vec<String>,
) -> SearchableTable {
    let (kind, folder, relation) = match relation_type {
        "view" => (NodeKind::View, FolderKind::Views, RelationKind::View),
        _ => (NodeKind::Table, FolderKind::Tables, RelationKind::Table),
    };
    SearchableTable {
        path: CatalogPath::root()
            .child(NodeKind::Database, database.clone())
            .child(NodeKind::Schema, schema.clone())
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name.clone()),
        database: Some(database),
        name: TableName { schema, name },
        relation,
        key,
        row_estimate: estimate.and_then(|rows| u64::try_from(rows).ok()),
        columns: Vec::new(),
    }
}
