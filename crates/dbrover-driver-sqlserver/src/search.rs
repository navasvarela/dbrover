//! The tables a data search reads (W16, `docs/data-search.md`), with their columns, keys,
//! collations and row counts, from each database's `sys` catalog views.

use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};

use crate::{SQL_SERVER_QUOTING, SqlClient, column, connection_error};

/// Every column of a database's user tables (and views), in table order, with the table's row
/// count from its heap or clustered index, the column's base type (with a decimal's precision
/// and scale), its collation, and its place in the primary key. An alias type is read as the
/// type it is based on, so `sysname` is an `nvarchar`.
fn sql(catalog: &str, views: Relations) -> String {
    let types = match views {
        Relations::TablesOnly => "'U'",
        Relations::TablesAndViews => "'U', 'V'",
    };
    format!(
        "SELECT s.name, o.name, RTRIM(o.type), \
         (SELECT CAST(SUM(p.rows) AS bigint) FROM {catalog}.partitions p \
          WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)), \
         c.name, \
         CONCAT(t.name, CASE WHEN t.name IN ('decimal', 'numeric') \
           THEN CONCAT('(', c.precision, ',', c.scale, ')') ELSE '' END), \
         c.collation_name, CAST(ic.key_ordinal AS int) \
         FROM {catalog}.objects o \
         JOIN {catalog}.schemas s ON s.schema_id = o.schema_id \
         JOIN {catalog}.columns c ON c.object_id = o.object_id \
         JOIN {catalog}.types t ON t.user_type_id = c.system_type_id \
         LEFT JOIN {catalog}.indexes i ON i.object_id = o.object_id AND i.is_primary_key = 1 \
         LEFT JOIN {catalog}.index_columns ic ON ic.object_id = i.object_id \
           AND ic.index_id = i.index_id AND ic.column_id = c.column_id \
         WHERE o.type IN ({types}) AND o.is_ms_shipped = 0 \
         ORDER BY s.name, o.name, c.column_id"
    )
}

/// The tables of the databases the scope names, or of the connected database when it names
/// none: a search of the whole data source doesn't read every database on the server.
pub(crate) async fn searchable_tables(
    client: &mut SqlClient,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let mut databases: Vec<String> = tables
        .scope
        .databases()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if databases.is_empty() {
        databases.push(connected(client).await?);
    }
    let mut found = Vec::new();
    for database in databases {
        found.extend(
            database_tables(client, &database, tables.views)
                .await?
                .into_iter()
                .filter(|table| {
                    let system = matches!(
                        table.name.schema.as_str(),
                        "sys" | "INFORMATION_SCHEMA" | "guest"
                    );
                    (!system || tables.scope.names_schema(&table.name.schema))
                        && tables.scope.includes(&table.path)
                }),
        );
    }
    Ok(found)
}

async fn connected(client: &mut SqlClient) -> Result<String, DriverError> {
    let row = client
        .query("SELECT DB_NAME()", &[])
        .await
        .map_err(connection_error)?
        .into_row()
        .await
        .map_err(connection_error)?;
    row.as_ref()
        .and_then(|row| column::<&str>(row, 0))
        .map(str::to_owned)
        .ok_or_else(|| connection_error("the server didn't name its database"))
}

async fn database_tables(
    client: &mut SqlClient,
    database: &str,
    views: Relations,
) -> Result<Vec<SearchableTable>, DriverError> {
    let catalog = format!("{}.sys", SQL_SERVER_QUOTING.identifier(database));
    let rows = client
        .query(sql(&catalog, views), &[])
        .await
        .map_err(connection_error)?
        .into_first_result()
        .await
        .map_err(connection_error)?;
    let mut found: Vec<(SearchableTable, Vec<(i32, String)>)> = Vec::new();
    for row in &rows {
        let text = |index: usize| -> Result<String, DriverError> {
            column::<&str>(row, index)
                .map(str::to_owned)
                .ok_or_else(|| connection_error(format!("catalog row without column {index}")))
        };
        let (schema, name) = (text(0)?, text(1)?);
        let same = found
            .last()
            .is_some_and(|(table, _)| table.name.schema == schema && table.name.name == name);
        if !same {
            let estimate = column::<i64>(row, 3).and_then(|rows| u64::try_from(rows).ok());
            found.push((
                table(database, schema, name, &text(2)?, estimate),
                Vec::new(),
            ));
        }
        let Some((table, key)) = found.last_mut() else {
            continue;
        };
        let name = text(4)?;
        if let Some(position) = column::<i32>(row, 7) {
            key.push((position, name.clone()));
        }
        let mut searched = SearchColumn::new(name, text(5)?);
        if let Some(collation) = column::<&str>(row, 6) {
            searched = searched.with_collation(collation);
        }
        table.columns.push(searched);
    }
    Ok(found
        .into_iter()
        .map(|(table, mut key)| {
            key.sort();
            SearchableTable {
                key: key.into_iter().map(|(_, name)| name).collect(),
                ..table
            }
        })
        .collect())
}

fn table(
    database: &str,
    schema: String,
    name: String,
    object_type: &str,
    estimate: Option<u64>,
) -> SearchableTable {
    let (kind, folder, relation) = match object_type {
        "V" => (NodeKind::View, FolderKind::Views, RelationKind::View),
        _ => (NodeKind::Table, FolderKind::Tables, RelationKind::Table),
    };
    SearchableTable {
        database: Some(database.to_owned()),
        path: CatalogPath::root()
            .child(NodeKind::Database, database)
            .child(NodeKind::Schema, schema.clone())
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name.clone()),
        name: TableName { schema, name },
        relation,
        key: Vec::new(),
        row_estimate: estimate,
        columns: Vec::new(),
    }
}
