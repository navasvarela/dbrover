//! Reading a whole schema, for comparing schemas and saving snapshots (W13, VC3).
//!
//! Every query runs in one read-only `REPEATABLE READ` transaction, so they all see one moment
//! of the catalog, with the search path set to the schema alone: `format_type`, `pg_get_expr`
//! and `pg_get_viewdef` then print the schema's own objects without the schema, and other
//! schemas' objects with theirs, so the same objects read the same in any schema.

use std::collections::BTreeMap;

use dbrover_core::{
    ColumnDescription, DriverError, IndexDescription, Quoting, SchemaDescription, SchemaTable,
    TableConstraints, TableDescription, TransactionStatus, ViewDescription,
};
use tokio_postgres::types::Type;
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::table::{constraint_from_row, identity_from};
use crate::{connection_error, execution};

/// Tables of the schema: ordinary and partitioned ones, not partitions (whose parent is
/// compared) nor tables an extension created (which are the extension's).
const TABLES: &str = "c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) \
     AND c.relkind IN ('r', 'p') AND NOT c.relispartition \
     AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_class'::regclass \
       AND e.objid = c.oid AND e.deptype = 'e')";

pub(crate) async fn describe_schema(
    client: &mut Client,
    schema: &str,
) -> Result<Option<SchemaDescription>, DriverError> {
    if execution::transaction_status(client).await? != TransactionStatus::Idle {
        return Err(DriverError::Connection(
            "the connection is in a transaction, so its schema can't be read on its own".into(),
        ));
    }
    let transaction = client
        .build_transaction()
        .read_only(true)
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
        .map_err(connection_error)?;
    transaction
        .batch_execute(&format!(
            "SET LOCAL search_path TO {}",
            Quoting::STANDARD.identifier(schema)
        ))
        .await
        .map_err(connection_error)?;
    let description = read(&transaction, schema).await?;
    transaction.commit().await.map_err(connection_error)?;
    Ok(description)
}

async fn read(
    transaction: &Transaction<'_>,
    schema: &str,
) -> Result<Option<SchemaDescription>, DriverError> {
    let exists = transaction
        .query_typed_opt(
            "SELECT 1 FROM pg_namespace WHERE nspname = $1",
            &[(&schema, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    if exists.is_none() {
        return Ok(None);
    }
    let mut tables = read_columns(transaction, schema).await?;
    read_constraints(transaction, schema, &mut tables).await?;
    Ok(Some(SchemaDescription {
        schema: schema.to_owned(),
        tables: tables.into_values().collect(),
        indexes: read_indexes(transaction, schema).await?,
        views: read_views(transaction, schema).await?,
    }))
}

/// Every table's columns and primary key, by table name.
///
/// A column whose default is `nextval` of a sequence the column owns is read as the
/// `serial` it was most likely written as, without the default: written back, `CREATE TABLE`
/// makes the sequence again, where a default would name one that isn't there.
async fn read_columns(
    transaction: &Transaction<'_>,
    schema: &str,
) -> Result<BTreeMap<String, SchemaTable>, DriverError> {
    let rows = transaction
        .query_typed(
            &format!(
                "SELECT c.relname::text, a.attname::text, \
                 array_position(i.indkey::int2[], a.attnum), \
                 format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                 pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, \
                 s.seqstart, s.seqincrement, \
                 a.attidentity = '' AND owned.oid IS NOT NULL \
                   AND pg_get_expr(d.adbin, d.adrelid) \
                     = format('nextval(%L::regclass)', owned.oid::regclass::text) \
                 FROM pg_class c \
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 \
                   AND NOT a.attisdropped \
                 LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
                 LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum \
                 LEFT JOIN LATERAL (SELECT q.oid FROM pg_depend dep \
                   JOIN pg_class q ON q.oid = dep.objid AND q.relkind = 'S' \
                   WHERE dep.classid = 'pg_class'::regclass \
                   AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = c.oid \
                   AND dep.refobjsubid = a.attnum AND dep.deptype IN ('a', 'i') \
                   LIMIT 1) owned ON true \
                 LEFT JOIN pg_sequence s ON s.seqrelid = owned.oid AND a.attidentity <> '' \
                 WHERE {TABLES} \
                 ORDER BY c.relname, a.attnum"
            ),
            &[(&schema, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    let mut tables: BTreeMap<String, (SchemaTable, Vec<(i32, String)>)> = BTreeMap::new();
    for row in &rows {
        let table: String = row.try_get(0).map_err(connection_error)?;
        let (entry, key) = tables
            .entry(table.clone())
            .or_insert_with(|| (empty_table(schema, &table), Vec::new()));
        let column = column_from(row)?;
        if let Some(position) = row.try_get::<_, Option<i32>>(2).map_err(connection_error)? {
            key.push((position, column.name.clone()));
        }
        entry.description.columns.push(column);
    }
    Ok(tables
        .into_iter()
        .map(|(name, (mut table, mut key))| {
            key.sort();
            table.description.primary_key = key.into_iter().map(|(_, column)| column).collect();
            (name, table)
        })
        .collect())
}

fn empty_table(schema: &str, name: &str) -> SchemaTable {
    SchemaTable {
        description: TableDescription {
            schema: schema.to_owned(),
            name: name.to_owned(),
            columns: Vec::new(),
            primary_key: Vec::new(),
            foreign_keys: Vec::new(),
            referenced_by: Vec::new(),
        },
        constraints: TableConstraints::default(),
    }
}

/// A row of [`read_columns`]'s query as a column, a serial one read as `serial`.
fn column_from(row: &Row) -> Result<ColumnDescription, DriverError> {
    let mut column = ColumnDescription {
        name: row.try_get(1).map_err(connection_error)?,
        data_type: row.try_get(3).map_err(connection_error)?,
        nullable: row.try_get(4).map_err(connection_error)?,
        default: row.try_get(5).map_err(connection_error)?,
        identity: identity_from(row, 6)?,
    };
    let owns_its_sequence: bool = row.try_get(9).map_err(connection_error)?;
    if owns_its_sequence && let Some(serial) = serial_type(&column.data_type) {
        column.data_type = serial.to_owned();
        column.default = None;
    }
    Ok(column)
}

fn serial_type(data_type: &str) -> Option<&'static str> {
    match data_type {
        "smallint" => Some("smallserial"),
        "integer" => Some("serial"),
        "bigint" => Some("bigserial"),
        _ => None,
    }
}

async fn read_constraints(
    transaction: &Transaction<'_>,
    schema: &str,
    tables: &mut BTreeMap<String, SchemaTable>,
) -> Result<(), DriverError> {
    let rows = transaction
        .query_typed(
            &format!(
                "SELECT k.conname::text, k.contype::text, \
                 ARRAY(SELECT a.attname::text FROM unnest(k.conkey) WITH ORDINALITY u(attnum, i) \
                   JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.attnum \
                   ORDER BY u.i), \
                 rn.nspname::text, r.relname::text, \
                 ARRAY(SELECT a.attname::text FROM unnest(k.confkey) WITH ORDINALITY u(attnum, i) \
                   JOIN pg_attribute a ON a.attrelid = k.confrelid AND a.attnum = u.attnum \
                   ORDER BY u.i), \
                 k.confdeltype::text, k.confupdtype::text, pg_get_expr(k.conbin, k.conrelid), \
                 c.relname::text \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid \
                 LEFT JOIN pg_class r ON r.oid = k.confrelid \
                 LEFT JOIN pg_namespace rn ON rn.oid = r.relnamespace \
                 WHERE {TABLES} AND k.contype IN ('p', 'u', 'f', 'c') \
                 ORDER BY c.relname, k.conname"
            ),
            &[(&schema, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    for row in &rows {
        let table: String = row.try_get(9).map_err(connection_error)?;
        if let Some(entry) = tables.get_mut(&table) {
            constraint_from_row(row, &mut entry.constraints)?;
        }
    }
    Ok(())
}

/// The indexes of the schema's tables and materialized views, apart from those that hold up a
/// primary key, unique or exclusion constraint.
///
/// The key and `INCLUDE` lists are read from `pg_get_indexdef`'s statement rather than column
/// by column: a column's own definition leaves out its ordering (`DESC`, `NULLS LAST`) and its
/// operator class, which are part of what the index is.
async fn read_indexes(
    transaction: &Transaction<'_>,
    schema: &str,
) -> Result<Vec<IndexDescription>, DriverError> {
    let rows = transaction
        .query_typed(
            "SELECT i.relname::text, t.relname::text, x.indisunique, am.amname::text, \
             pg_get_indexdef(x.indexrelid), pg_get_expr(x.indpred, x.indrelid) \
             FROM pg_index x \
             JOIN pg_class i ON i.oid = x.indexrelid \
             JOIN pg_class t ON t.oid = x.indrelid \
             JOIN pg_am am ON am.oid = i.relam \
             WHERE t.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) \
             AND t.relkind IN ('r', 'p', 'm') AND NOT t.relispartition \
             AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid = x.indexrelid \
               AND k.conrelid = x.indrelid AND k.contype IN ('p', 'u', 'x')) \
             AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_class'::regclass \
               AND e.objid = t.oid AND e.deptype = 'e') \
             ORDER BY i.relname",
            &[(&schema, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    rows.iter()
        .map(|row| {
            let method: String = row.try_get(3).map_err(connection_error)?;
            let definition: String = row.try_get(4).map_err(connection_error)?;
            let lists = index_lists(&definition, &method);
            Ok(IndexDescription {
                name: row.try_get(0).map_err(connection_error)?,
                table: row.try_get(1).map_err(connection_error)?,
                unique: row.try_get(2).map_err(connection_error)?,
                method,
                key: lists.key,
                include: lists.include,
                predicate: row.try_get(5).map_err(connection_error)?,
            })
        })
        .collect()
}

/// An index's key and included columns, as `pg_get_indexdef` writes them.
#[derive(Debug, Default, PartialEq, Eq)]
struct IndexLists {
    key: Vec<String>,
    include: Vec<String>,
}

/// The lists in `CREATE INDEX … USING method (key) [INCLUDE (columns)] …`.
fn index_lists(definition: &str, method: &str) -> IndexLists {
    let using = format!(" USING {method} ");
    let Some(start) = definition.find(&using) else {
        return IndexLists::default();
    };
    let after_using = &definition[start + using.len()..];
    let Some((key, rest)) = parenthesised(after_using) else {
        return IndexLists::default();
    };
    let include = rest
        .strip_prefix(" INCLUDE ")
        .and_then(parenthesised)
        .map(|(include, _)| split_top_level(include))
        .unwrap_or_default();
    IndexLists {
        key: split_top_level(key),
        include,
    }
}

/// The text inside the parentheses `text` starts with, and what follows them.
fn parenthesised(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('(')?;
    let end = top_level_positions(inner).find(|&(_, c)| c == ')')?.0;
    Some((&inner[..end], &inner[end + 1..]))
}

/// Splits a list at its commas outside parentheses and quotes.
fn split_top_level(list: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut start = 0;
    for (position, _) in top_level_positions(list).filter(|&(_, c)| c == ',') {
        items.push(list[start..position].trim().to_owned());
        start = position + 1;
    }
    items.push(list[start..].trim().to_owned());
    items.retain(|item| !item.is_empty());
    items
}

/// The characters of `text` outside quotes and nested parentheses, with their byte positions;
/// a `)` that closes the level `text` is at is included, and ends the walk.
fn top_level_positions(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut closed = false;
    text.char_indices().filter_map(move |(position, c)| {
        if closed {
            return None;
        }
        match (quote, c) {
            (Some(open), _) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') if depth == 0 => {
                closed = true;
                return Some((position, c));
            }
            (None, ')') => depth -= 1,
            (None, _) if depth == 0 => return Some((position, c)),
            (None, _) => {}
        }
        None
    })
}

async fn read_views(
    transaction: &Transaction<'_>,
    schema: &str,
) -> Result<Vec<ViewDescription>, DriverError> {
    let rows = transaction
        .query_typed(
            "SELECT c.relname::text, c.relkind = 'm', pg_get_viewdef(c.oid, true), \
             ARRAY(SELECT DISTINCT r.relname::text FROM pg_rewrite w \
               JOIN pg_depend d ON d.classid = 'pg_rewrite'::regclass AND d.objid = w.oid \
                 AND d.refclassid = 'pg_class'::regclass \
               JOIN pg_class r ON r.oid = d.refobjid \
               WHERE w.ev_class = c.oid AND r.oid <> c.oid \
               AND r.relnamespace = c.relnamespace AND r.relkind IN ('r', 'p', 'v', 'm') \
               ORDER BY 1) \
             FROM pg_class c \
             WHERE c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) \
             AND c.relkind IN ('v', 'm') \
             AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_class'::regclass \
               AND e.objid = c.oid AND e.deptype = 'e') \
             ORDER BY c.relname",
            &[(&schema, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    rows.iter()
        .map(|row| {
            Ok(ViewDescription {
                name: row.try_get(0).map_err(connection_error)?,
                materialized: row.try_get(1).map_err(connection_error)?,
                query: row.try_get(2).map_err(connection_error)?,
                reads: row.try_get(3).map_err(connection_error)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
