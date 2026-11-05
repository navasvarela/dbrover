//! Describing a table for editing its rows.

use dbrover_core::{
    CheckConstraint, ColumnDescription, DriverError, ForeignKey, ForeignKeyConstraint, Identity,
    IdentityGeneration, Quoting, ReferentialAction, RelationName, TableConstraints,
    TableDescription, TableName, UniqueConstraint,
};
use tokio_postgres::Client;
use tokio_postgres::types::Type;

use crate::connection_error;

/// Kinds of `pg_class` rows whose rows can be updated: ordinary and partitioned tables.
const PLAIN_TABLE_KINDS: [&str; 2] = ["r", "p"];

/// Resolves `table` with `to_regclass`, which applies the search path and folds unquoted
/// names to lowercase exactly as a statement would, then reads its columns (with their types,
/// nullability and defaults), primary key and foreign keys both ways.
pub(crate) async fn describe_table(
    client: &Client,
    table: &RelationName,
) -> Result<Option<TableDescription>, DriverError> {
    let written = table.to_sql(Quoting::STANDARD);
    let found = client
        .query_typed(
            "SELECT n.nspname, c.relname, c.relkind::text \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = to_regclass($1)",
            &[(&written, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    let Some(row) = found.first() else {
        return Ok(None);
    };
    let kind: String = row.try_get(2).map_err(connection_error)?;
    if !PLAIN_TABLE_KINDS.contains(&kind.as_str()) {
        return Ok(None);
    }
    let columns = client
        .query_typed(
            "SELECT a.attname, array_position(i.indkey::int2[], a.attnum), \
             format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, s.seqstart, s.seqincrement \
             FROM pg_attribute a \
             LEFT JOIN pg_index i ON i.indrelid = a.attrelid AND i.indisprimary \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_sequence s ON a.attidentity <> '' \
               AND s.seqrelid = pg_get_serial_sequence($1, a.attname)::regclass \
             WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[(&written, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    let mut description = TableDescription {
        schema: row.try_get(0).map_err(connection_error)?,
        name: row.try_get(1).map_err(connection_error)?,
        columns: Vec::new(),
        primary_key: Vec::new(),
        foreign_keys: Vec::new(),
        referenced_by: Vec::new(),
    };
    let mut key = Vec::new();
    for column in &columns {
        let name: String = column.try_get(0).map_err(connection_error)?;
        let key_position: Option<i32> = column.try_get(1).map_err(connection_error)?;
        if let Some(position) = key_position {
            key.push((position, name.clone()));
        }
        description.columns.push(ColumnDescription {
            name,
            data_type: column.try_get(2).map_err(connection_error)?,
            nullable: column.try_get(3).map_err(connection_error)?,
            default: column.try_get(4).map_err(connection_error)?,
            identity: identity_from(column, 5)?,
        });
    }
    key.sort();
    description.primary_key = key.into_iter().map(|(_, name)| name).collect();
    let me = description.table_name();
    for key in foreign_keys(client, &written).await? {
        if key.table == me {
            description.foreign_keys.push(key.clone());
        }
        if key.referenced == me {
            description.referenced_by.push(key);
        }
    }
    Ok(Some(description))
}

/// An identity column's `attidentity` (`a` for `ALWAYS`, `d` for `BY DEFAULT`, empty for a
/// column that isn't one) at `index`, and its sequence's start and increment in the two
/// columns after it.
pub(crate) fn identity_from(
    row: &tokio_postgres::Row,
    index: usize,
) -> Result<Option<Identity>, DriverError> {
    let generation = match row
        .try_get::<_, String>(index)
        .map_err(connection_error)?
        .as_str()
    {
        "a" => IdentityGeneration::Always,
        "d" => IdentityGeneration::ByDefault,
        _ => return Ok(None),
    };
    let start: Option<i64> = row.try_get(index + 1).map_err(connection_error)?;
    let increment: Option<i64> = row.try_get(index + 2).map_err(connection_error)?;
    let counting = Identity::counting(generation);
    Ok(Some(Identity {
        start: start.unwrap_or(counting.start),
        increment: increment.unwrap_or(counting.increment),
        ..counting
    }))
}

/// The foreign keys from or to the table, each column list in key order.
///
/// A key of or to a partitioned table is copied onto its partitions, each copy naming its
/// parent in `conparentid`. A copy on a partition of the table's own is a key that partition
/// has; a copy that only points at a partition of the referenced table repeats a key the
/// table already lists, and is left out.
async fn foreign_keys(client: &Client, written: &str) -> Result<Vec<ForeignKey>, DriverError> {
    let rows = client
        .query_typed(
            "SELECT tn.nspname, t.relname, \
             ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(attnum, i) \
               JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
               ORDER BY k.i), \
             rn.nspname, r.relname, \
             ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY k(attnum, i) \
               JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum \
               ORDER BY k.i) \
             FROM pg_constraint c \
             JOIN pg_class t ON t.oid = c.conrelid \
             JOIN pg_namespace tn ON tn.oid = t.relnamespace \
             JOIN pg_class r ON r.oid = c.confrelid \
             JOIN pg_namespace rn ON rn.oid = r.relnamespace \
             WHERE c.contype = 'f' \
             AND (c.conrelid = to_regclass($1) OR c.confrelid = to_regclass($1)) \
             AND NOT EXISTS (SELECT 1 FROM pg_constraint p \
               WHERE p.oid = c.conparentid AND p.conrelid = c.conrelid) \
             ORDER BY c.conname",
            &[(&written, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    rows.iter()
        .map(|row| {
            let get = |index| row.try_get::<_, String>(index).map_err(connection_error);
            Ok(ForeignKey {
                table: TableName {
                    schema: get(0)?,
                    name: get(1)?,
                },
                columns: row.try_get(2).map_err(connection_error)?,
                referenced: TableName {
                    schema: get(3)?,
                    name: get(4)?,
                },
                referenced_columns: row.try_get(5).map_err(connection_error)?,
            })
        })
        .collect()
}

/// The table's primary key, unique, foreign key and check constraints, by name. A partition's
/// copies of its parent's constraints are its own as far as changing it goes.
pub(crate) async fn describe_constraints(
    client: &Client,
    table: &TableName,
) -> Result<TableConstraints, DriverError> {
    let written = format!(
        "{}.{}",
        Quoting::STANDARD.identifier(&table.schema),
        Quoting::STANDARD.identifier(&table.name)
    );
    let rows = client
        .query_typed(
            "SELECT c.conname::text, c.contype::text, \
             ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(attnum, i) \
               JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
               ORDER BY k.i), \
             rn.nspname::text, r.relname::text, \
             ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY k(attnum, i) \
               JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum \
               ORDER BY k.i), \
             c.confdeltype::text, c.confupdtype::text, pg_get_expr(c.conbin, c.conrelid) \
             FROM pg_constraint c \
             LEFT JOIN pg_class r ON r.oid = c.confrelid \
             LEFT JOIN pg_namespace rn ON rn.oid = r.relnamespace \
             WHERE c.conrelid = to_regclass($1) AND c.contype IN ('p', 'u', 'f', 'c') \
             ORDER BY c.conname",
            &[(&written, Type::TEXT)],
        )
        .await
        .map_err(connection_error)?;
    let mut constraints = TableConstraints::default();
    for row in &rows {
        constraint_from_row(row, &mut constraints)?;
    }
    Ok(constraints)
}

/// Adds the constraint a row describes to `constraints`. The row's first nine columns are
/// [`describe_constraints`]'s: name, kind, columns, the referenced table's schema, name and
/// columns, the actions on delete and on update, and a check's condition.
pub(crate) fn constraint_from_row(
    row: &tokio_postgres::Row,
    constraints: &mut TableConstraints,
) -> Result<(), DriverError> {
    let text = |index| {
        row.try_get::<_, Option<String>>(index)
            .map_err(connection_error)
    };
    let name: String = row.try_get(0).map_err(connection_error)?;
    let columns: Vec<String> = row.try_get(2).map_err(connection_error)?;
    match text(1)?.as_deref() {
        Some("p") => constraints.primary_key_name = Some(name),
        Some("u") => constraints.unique_keys.push(UniqueConstraint {
            name: Some(name),
            columns,
        }),
        Some("f") => constraints.foreign_keys.push(ForeignKeyConstraint {
            name: Some(name),
            columns,
            referenced: TableName {
                schema: text(3)?.unwrap_or_default(),
                name: text(4)?.unwrap_or_default(),
            },
            referenced_columns: row.try_get(5).map_err(connection_error)?,
            on_delete: referential_action(text(6)?.as_deref()),
            on_update: referential_action(text(7)?.as_deref()),
        }),
        _ => constraints.checks.push(CheckConstraint {
            name: Some(name),
            expression: text(8)?.unwrap_or_default(),
        }),
    }
    Ok(())
}

/// `pg_constraint`'s one-letter code for a foreign key's action.
fn referential_action(code: Option<&str>) -> ReferentialAction {
    match code {
        Some("r") => ReferentialAction::Restrict,
        Some("c") => ReferentialAction::Cascade,
        Some("n") => ReferentialAction::SetNull,
        Some("d") => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}
