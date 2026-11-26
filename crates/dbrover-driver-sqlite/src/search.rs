//! The tables a data search reads (W16, `docs/data-search.md`), with their columns and keys.
//! Blocking; called through `SqliteConnection::run_blocking`.

use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, Quoting, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};
use rusqlite::Connection;

use crate::catalog::rows;

/// The relations `tables` takes in, from every schema the connection has (`main` and any
/// attached database; `temp` only when named). SQLite keeps no row estimate, so the search
/// orders them by name.
pub(crate) fn searchable_tables(
    connection: &Connection,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let schemas = rows(
        connection,
        "SELECT name FROM pragma_database_list ORDER BY seq",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let mut found = Vec::new();
    for schema in schemas {
        if schema == "temp" && !tables.scope.names_schema(&schema) {
            continue;
        }
        if tables
            .scope
            .schemas()
            .is_some_and(|named| !named.contains(&schema.as_str()))
        {
            continue;
        }
        found.extend(
            schema_tables(connection, &schema, tables.views)?
                .into_iter()
                .filter(|table| tables.scope.includes(&table.path)),
        );
    }
    Ok(found)
}

/// One row per column of `schema`'s tables (and views), in table order. Columns a virtual
/// table hides (`hidden = 1`) aren't stored; generated ones (2 and 3) are searched. Virtual
/// tables, such as FTS5's, are left out: their text is kept again in tables of their own.
fn schema_tables(
    connection: &Connection,
    schema: &str,
    views: Relations,
) -> Result<Vec<SearchableTable>, DriverError> {
    let types = match views {
        Relations::TablesOnly => "'table'",
        Relations::TablesAndViews => "'table', 'view'",
    };
    let sql = format!(
        "SELECT s.name, s.type, p.name, p.type, p.pk FROM {}.sqlite_schema s \
         JOIN pragma_table_xinfo(s.name, ?1) p \
         WHERE s.type IN ({types}) AND s.name NOT LIKE 'sqlite!_%' ESCAPE '!' \
         AND coalesce(s.sql, '') NOT LIKE 'CREATE VIRTUAL%' AND p.hidden <> 1 \
         ORDER BY s.name, p.cid",
        Quoting::STANDARD.identifier(schema)
    );
    let columns = rows(connection, &sql, [schema], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut tables: Vec<(SearchableTable, Vec<(i64, String)>)> = Vec::new();
    for (relation, relation_type, column, data_type, key_position) in columns {
        let same = tables
            .last()
            .is_some_and(|(table, _)| table.name.name == relation);
        if !same {
            tables.push((table(schema, &relation, &relation_type), Vec::new()));
        }
        if let Some((table, key)) = tables.last_mut() {
            if key_position > 0 {
                key.push((key_position, column.clone()));
            }
            table.columns.push(SearchColumn::new(column, data_type));
        }
    }
    Ok(tables
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

fn table(schema: &str, name: &str, relation_type: &str) -> SearchableTable {
    let (kind, folder, relation) = match relation_type {
        "view" => (NodeKind::View, FolderKind::Views, RelationKind::View),
        _ => (NodeKind::Table, FolderKind::Tables, RelationKind::Table),
    };
    SearchableTable {
        database: None,
        name: TableName {
            schema: schema.to_owned(),
            name: name.to_owned(),
        },
        path: CatalogPath::root()
            .child(NodeKind::Schema, schema)
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name),
        relation,
        key: Vec::new(),
        row_estimate: None,
        columns: Vec::new(),
    }
}
