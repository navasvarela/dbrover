//! The tables a data search reads (W16, `docs/data-search.md`), with their columns, keys,
//! collations and row estimates, in one `information_schema` query.

use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};
use mysql_async::{Conn, Value};

use crate::catalog::{SYSTEM_SCHEMAS, field, query};

/// Every column of the tables (and views) asked for, in table order, with the table's row
/// estimate, the column's full type (`int(10) unsigned`, `enum('a','b')`), its collation, and
/// its place in the primary key. `information_schema` lists only what the login may read.
fn sql(views: Relations, schemas: usize) -> String {
    let types = match views {
        Relations::TablesOnly => "'BASE TABLE', 'SYSTEM VERSIONED'",
        Relations::TablesAndViews => "'BASE TABLE', 'SYSTEM VERSIONED', 'VIEW'",
    };
    let schemas = match schemas {
        0 => String::new(),
        count => format!(" AND c.table_schema IN ({})", vec!["?"; count].join(", ")),
    };
    format!(
        "SELECT c.table_schema, c.table_name, t.table_type, t.table_rows, c.column_name, \
         c.column_type, c.collation_name, k.ordinal_position \
         FROM information_schema.columns c \
         JOIN information_schema.tables t \
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
         LEFT JOIN information_schema.key_column_usage k \
           ON k.table_schema = c.table_schema AND k.table_name = c.table_name \
           AND k.column_name = c.column_name AND k.constraint_name = 'PRIMARY' \
         WHERE t.table_type IN ({types}){schemas} \
         ORDER BY c.table_schema, c.table_name, c.ordinal_position"
    )
}

pub(crate) async fn searchable_tables(
    conn: &mut Conn,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let schemas = tables.scope.schemas().unwrap_or_default();
    let params: Vec<Value> = schemas.iter().map(|schema| Value::from(*schema)).collect();
    let rows = query(conn, &sql(tables.views, schemas.len()), params).await?;
    let mut found: Vec<(SearchableTable, Vec<(u32, String)>)> = Vec::new();
    for row in &rows {
        let (schema, name) = (field(row, 0)?, field(row, 1)?);
        let same = found
            .last()
            .is_some_and(|(table, _)| table.name.schema == schema && table.name.name == name);
        if !same {
            let estimate = row.get(3).cloned().flatten();
            found.push((table(schema, name, &field(row, 2)?, estimate), Vec::new()));
        }
        let Some((table, key)) = found.last_mut() else {
            continue;
        };
        let column = field(row, 4)?;
        if let Some(position) = row
            .get(7)
            .cloned()
            .flatten()
            .and_then(|position| position.parse().ok())
        {
            key.push((position, column.clone()));
        }
        let mut searched = SearchColumn::new(column, field(row, 5)?);
        if let Some(collation) = row.get(6).cloned().flatten() {
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
        .filter(|table| {
            let system = SYSTEM_SCHEMAS.contains(&table.name.schema.to_ascii_lowercase().as_str());
            (!system || tables.scope.names_schema(&table.name.schema))
                && tables.scope.includes(&table.path)
        })
        .collect())
}

fn table(
    schema: String,
    name: String,
    table_type: &str,
    estimate: Option<String>,
) -> SearchableTable {
    let (kind, folder, relation) = match table_type {
        "VIEW" => (NodeKind::View, FolderKind::Views, RelationKind::View),
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
        // InnoDB's count is an estimate, refreshed by `ANALYZE TABLE`; enough to order by.
        row_estimate: estimate.and_then(|rows| rows.parse().ok()),
        columns: Vec::new(),
    }
}
