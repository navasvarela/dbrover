//! The tables a data search reads (W16, `docs/data-search.md`), with their columns, keys and
//! row estimates, in one catalog query.

use dbrover_core::{
    CatalogPath, DriverError, FolderKind, NodeKind, RelationKind, Relations, SearchColumn,
    SearchableTable, TableName, TablesToSearch,
};
use tokio_postgres::Client;
use tokio_postgres::types::Type;

use crate::catalog::{is_system_schema, query};
use crate::connection_error;

/// Every column of the relations of the kinds asked for, in the schemas asked for (all when
/// `$2` is null), with the relation's row estimate and the column's place in its primary key.
///
/// An enum's column is reported as `enum`, and a domain's as the type it is based on, so
/// `dbrover_sql::Dialect::search_kind` can tell what each holds. Partitions are left out:
/// their rows are their partitioned table's, which is searched instead. Relations the login
/// can't read are left out too, rather than failing one by one.
const SEARCHABLE_COLUMNS: &str = "\
SELECT current_database(), n.nspname, c.relname, c.relkind::text, c.reltuples::float8, \
       a.attname, \
       CASE WHEN t.typtype = 'e' THEN 'enum' \
            WHEN t.typtype = 'd' THEN format_type(t.typbasetype, t.typtypmod) \
            ELSE format_type(a.atttypid, a.atttypmod) END, \
       array_position(i.indkey::int2[], a.attnum) \
FROM pg_class c \
JOIN pg_namespace n ON n.oid = c.relnamespace \
JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
JOIN pg_type t ON t.oid = a.atttypid \
LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
WHERE c.relkind::text = ANY($1) AND NOT c.relispartition \
  AND ($2::text[] IS NULL OR n.nspname::text = ANY($2)) \
  AND has_schema_privilege(n.oid, 'USAGE') AND has_table_privilege(c.oid, 'SELECT') \
ORDER BY n.nspname, c.relname, a.attnum";

/// The tables and, if asked for, views and materialized views of the connected database that
/// `tables` takes in.
pub(crate) async fn searchable_tables(
    client: &Client,
    tables: &TablesToSearch,
) -> Result<Vec<SearchableTable>, DriverError> {
    let relkinds: &[&str] = match tables.views {
        Relations::TablesOnly => &["r", "p"],
        Relations::TablesAndViews => &["r", "p", "v", "m"],
    };
    let schemas = tables.scope.schemas();
    let rows = query(
        client,
        SEARCHABLE_COLUMNS,
        &[(&relkinds, Type::TEXT_ARRAY), (&schemas, Type::TEXT_ARRAY)],
    )
    .await?;
    let mut found: Vec<Found> = Vec::new();
    for row in &rows {
        let column = Column::read(row)?;
        if let Some(last) = found.last_mut()
            && last.is(&column)
        {
            last.add(column);
        } else {
            found.push(Found::new(column));
        }
    }
    let connected = rows
        .first()
        .map(|row| row.try_get::<_, String>(0))
        .transpose()
        .map_err(connection_error)?;
    if let Some(connected) = &connected
        && tables
            .scope
            .databases()
            .iter()
            .any(|database| database != connected)
    {
        return Err(DriverError::Unsupported(
            "PostgreSQL searches the database its data source connects to; another database \
             needs a data source of its own.",
        ));
    }
    Ok(found
        .into_iter()
        .map(Found::into_table)
        .filter(|table| {
            let system = is_system_schema(&table.name.schema);
            (!system || tables.scope.names_schema(&table.name.schema))
                && tables.scope.includes(&table.path)
        })
        .collect())
}

/// The relation a row of [`SEARCHABLE_COLUMNS`] is about.
#[derive(PartialEq)]
struct Relation {
    database: String,
    schema: String,
    name: String,
    relkind: String,
    row_estimate: Option<f64>,
}

/// One row of [`SEARCHABLE_COLUMNS`].
struct Column {
    relation: Relation,
    name: String,
    data_type: String,
    key_position: Option<i32>,
}

impl Column {
    fn read(row: &tokio_postgres::Row) -> Result<Self, DriverError> {
        Ok(Self {
            relation: Relation {
                database: row.try_get(0).map_err(connection_error)?,
                schema: row.try_get(1).map_err(connection_error)?,
                name: row.try_get(2).map_err(connection_error)?,
                relkind: row.try_get(3).map_err(connection_error)?,
                row_estimate: row.try_get(4).map_err(connection_error)?,
            },
            name: row.try_get(5).map_err(connection_error)?,
            data_type: row.try_get(6).map_err(connection_error)?,
            key_position: row.try_get(7).map_err(connection_error)?,
        })
    }
}

/// A relation's columns, gathered from consecutive rows.
struct Found {
    relation: Relation,
    columns: Vec<SearchColumn>,
    key: Vec<(i32, String)>,
}

impl Found {
    fn new(column: Column) -> Self {
        let Column {
            relation,
            name,
            data_type,
            key_position,
        } = column;
        Self {
            relation,
            key: key_position
                .map(|position| (position, name.clone()))
                .into_iter()
                .collect(),
            columns: vec![SearchColumn::new(name, data_type)],
        }
    }

    fn is(&self, column: &Column) -> bool {
        self.relation == column.relation
    }

    fn add(&mut self, column: Column) {
        if let Some(position) = column.key_position {
            self.key.push((position, column.name.clone()));
        }
        self.columns
            .push(SearchColumn::new(column.name, column.data_type));
    }

    fn into_table(mut self) -> SearchableTable {
        self.key.sort();
        let Relation {
            database,
            schema,
            name,
            relkind,
            row_estimate,
        } = self.relation;
        let (kind, folder, relation) = match relkind.as_str() {
            "v" => (NodeKind::View, FolderKind::Views, RelationKind::View),
            "m" => (
                NodeKind::MaterializedView,
                FolderKind::MaterializedViews,
                RelationKind::View,
            ),
            _ => (NodeKind::Table, FolderKind::Tables, RelationKind::Table),
        };
        let path = CatalogPath::root()
            .child(NodeKind::Database, database)
            .child(NodeKind::Schema, schema.clone())
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name.clone());
        SearchableTable {
            database: None,
            name: TableName { schema, name },
            path,
            relation,
            key: self.key.into_iter().map(|(_, name)| name).collect(),
            // A table never analysed has an estimate of -1 (PostgreSQL 14 on) or 0 (before).
            row_estimate: row_estimate
                .filter(|rows| *rows >= 0.0)
                .map(estimate_to_rows),
            columns: self.columns,
        }
    }
}

/// `reltuples` is a `real` the planner keeps, a whole number in practice; rounding it to a
/// count only orders the search.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn estimate_to_rows(rows: f64) -> u64 {
    rows.round() as u64
}
