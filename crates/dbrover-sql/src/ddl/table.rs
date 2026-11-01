//! `CREATE TABLE`, and renaming, emptying and dropping a table.

use std::collections::HashSet;

use dbrover_core::{Identity, IdentityGeneration};

use super::{
    Check, ColumnSpec, DdlError, ForeignKey, ObjectName, ReferentialAction, TableSpec, UniqueKey,
    column_list, quote_name, require_name,
};
use crate::Dialect;

/// Indentation of each column and constraint inside `CREATE TABLE ( … )`.
const INDENT: &str = "    ";

pub(super) fn create(spec: &TableSpec, dialect: Dialect) -> Result<String, DdlError> {
    validate(spec, dialect)?;
    let mut lines: Vec<String> = spec
        .columns
        .iter()
        .map(|column| column_definition(column, dialect))
        .collect();
    // ClickHouse's primary key is its sort order, written after the column list; SQLite's
    // `AUTOINCREMENT` key is written on its column.
    if !spec.primary_key.is_empty()
        && dialect != Dialect::ClickHouse
        && !(dialect == Dialect::Sqlite && has_identity(spec))
    {
        lines.push(format!(
            "PRIMARY KEY {}",
            column_list(dialect, &spec.primary_key)
        ));
    }
    lines.extend(spec.unique_keys.iter().map(|key| unique_key(key, dialect)));
    lines.extend(
        spec.foreign_keys
            .iter()
            .map(|key| foreign_key(key, dialect)),
    );
    lines.extend(
        spec.checks
            .iter()
            .enumerate()
            .map(|(index, check)| self::check(spec, index, check, dialect)),
    );
    let body = lines
        .iter()
        .map(|line| format!("{INDENT}{line}"))
        .collect::<Vec<_>>()
        .join(",\n");
    let mut sql = format!("CREATE TABLE {} (\n{body}\n)", spec.name.to_sql(dialect));
    if dialect == Dialect::ClickHouse {
        sql.push_str(&clickhouse_engine(spec));
    }
    Ok(sql)
}

/// ClickHouse needs a table engine. MergeTree is its general-purpose one; `ORDER BY tuple()`
/// stores rows unsorted, for a table without a key.
fn clickhouse_engine(spec: &TableSpec) -> String {
    let order = if spec.primary_key.is_empty() {
        "tuple()".to_owned()
    } else {
        column_list(Dialect::ClickHouse, &spec.primary_key)
    };
    format!("\nENGINE = MergeTree\nORDER BY {order}")
}

fn has_identity(spec: &TableSpec) -> bool {
    spec.columns.iter().any(|column| column.identity.is_some())
}

pub(super) fn column_definition(column: &ColumnSpec, dialect: Dialect) -> String {
    let name = quote_name(dialect, &column.name);
    let data_type = column.data_type.trim();
    let mut definition = match dialect {
        // ClickHouse's types aren't nullable unless wrapped, inside `LowCardinality` if
        // they are in one.
        Dialect::ClickHouse
            if column.nullable
                && !data_type.starts_with("Nullable(")
                && !data_type.starts_with("LowCardinality(Nullable(") =>
        {
            format!("{name} Nullable({data_type})")
        }
        _ => format!("{name} {data_type}"),
    };
    if let Some(identity) = column.identity {
        definition.push_str(&identity_clause(identity, dialect));
    }
    if let Some(default) = column.default.as_deref().map(str::trim)
        && !default.is_empty()
    {
        definition.push_str(" DEFAULT ");
        definition.push_str(default);
    }
    match (dialect, column.nullable) {
        (Dialect::ClickHouse, _) => {}
        (_, false) => definition.push_str(" NOT NULL"),
        // A column is nullable by default except on SQL Server, where it depends on the
        // session's `ANSI_NULL_DFLT_ON` and the database's settings.
        (Dialect::SqlServer, true) => definition.push_str(" NULL"),
        (_, true) => {}
    }
    definition
}

/// How the dialect says the database numbers a column, after its type. `validate` refused
/// the dialects without identity columns.
fn identity_clause(identity: Identity, dialect: Dialect) -> String {
    let counting = Identity::counting(identity.generation);
    let custom = identity.start != counting.start || identity.increment != counting.increment;
    match dialect {
        Dialect::Postgres => {
            let generation = match identity.generation {
                IdentityGeneration::Always => "ALWAYS",
                IdentityGeneration::ByDefault => "BY DEFAULT",
            };
            let options = if custom {
                format!(
                    " (START WITH {} INCREMENT BY {})",
                    identity.start, identity.increment
                )
            } else {
                String::new()
            };
            format!(" GENERATED {generation} AS IDENTITY{options}")
        }
        Dialect::SqlServer => format!(" IDENTITY({}, {})", identity.start, identity.increment),
        Dialect::Mysql => " AUTO_INCREMENT".to_owned(),
        Dialect::Sqlite => " PRIMARY KEY AUTOINCREMENT".to_owned(),
        Dialect::DuckDb | Dialect::ClickHouse | Dialect::Cassandra => String::new(),
    }
}

fn constraint_name(name: Option<&str>, dialect: Dialect) -> String {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => format!("CONSTRAINT {} ", quote_name(dialect, name)),
        _ => String::new(),
    }
}

pub(super) fn unique_key(key: &UniqueKey, dialect: Dialect) -> String {
    format!(
        "{}UNIQUE {}",
        constraint_name(key.name.as_deref(), dialect),
        column_list(dialect, &key.columns)
    )
}

pub(super) fn foreign_key(key: &ForeignKey, dialect: Dialect) -> String {
    let target = match dialect {
        // SQLite's foreign keys point into the table's own schema, and refuse a qualified
        // name; `validate` checked the target is there.
        Dialect::Sqlite => quote_name(dialect, &key.target.name),
        _ => key.target.to_sql(dialect),
    };
    let mut sql = format!(
        "{}FOREIGN KEY {} REFERENCES {target} {}",
        constraint_name(key.name.as_deref(), dialect),
        column_list(dialect, &key.columns),
        column_list(dialect, &key.target_columns)
    );
    for (event, action) in [("DELETE", key.on_delete), ("UPDATE", key.on_update)] {
        if action != ReferentialAction::NoAction {
            sql.push_str(&format!(" ON {event} {}", action.sql()));
        }
    }
    sql
}

pub(super) fn check(spec: &TableSpec, index: usize, check: &Check, dialect: Dialect) -> String {
    let expression = check.expression.trim();
    match dialect {
        // ClickHouse requires a name, and takes the condition without parentheses.
        Dialect::ClickHouse => {
            let name = match check.name.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => name.to_owned(),
                _ => format!("{}_check_{}", spec.name.name, index + 1),
            };
            format!(
                "CONSTRAINT {} CHECK {expression}",
                quote_name(dialect, &name)
            )
        }
        _ => format!(
            "{}CHECK ({expression})",
            constraint_name(check.name.as_deref(), dialect)
        ),
    }
}

/// Checks the spec is complete and consistent, and that the dialect has what it asks for.
pub(super) fn validate(spec: &TableSpec, dialect: Dialect) -> Result<(), DdlError> {
    require_name(&spec.name.name, "table")?;
    if spec.columns.is_empty() {
        return Err(DdlError::NoColumns);
    }
    let mut names = HashSet::new();
    for (index, column) in spec.columns.iter().enumerate() {
        if column.name.trim().is_empty() {
            return Err(DdlError::UnnamedColumn {
                position: index + 1,
            });
        }
        if column.data_type.trim().is_empty() {
            return Err(DdlError::UntypedColumn {
                column: column.name.clone(),
            });
        }
        if !names.insert(column.name.as_str()) {
            return Err(DdlError::DuplicateColumn(column.name.clone()));
        }
    }
    let known = |columns: &[String], used_in: &'static str| -> Result<(), DdlError> {
        if columns.is_empty() {
            return Err(DdlError::NoKeyColumns(used_in));
        }
        match columns
            .iter()
            .find(|column| !names.contains(column.as_str()))
        {
            Some(column) => Err(DdlError::UnknownColumn {
                column: column.clone(),
                used_in,
            }),
            None => Ok(()),
        }
    };
    if !spec.primary_key.is_empty() {
        known(&spec.primary_key, "primary key")?;
    }
    let capabilities = dialect.capabilities();
    let unsupported = |what| DdlError::Unsupported { what, dialect };
    validate_identity(spec, dialect)?;
    if !spec.unique_keys.is_empty() && !capabilities.unique_keys {
        return Err(unsupported("unique keys"));
    }
    for key in &spec.unique_keys {
        known(&key.columns, "unique key")?;
    }
    if !spec.foreign_keys.is_empty() && !capabilities.foreign_keys {
        return Err(unsupported("foreign keys"));
    }
    for key in &spec.foreign_keys {
        known(&key.columns, "foreign key")?;
        require_name(&key.target.name, "foreign key's target table")?;
        if key.target_columns.len() != key.columns.len() {
            return Err(DdlError::ForeignKeyColumnCount);
        }
        if dialect == Dialect::Sqlite
            && key.target.schema.is_some()
            && key.target.schema != spec.name.schema
        {
            return Err(unsupported("foreign keys to a table in another schema"));
        }
        for action in [key.on_delete, key.on_update] {
            if !capabilities.referential_actions.contains(&action) {
                return Err(unsupported(referential_action_name(action)));
            }
        }
    }
    if spec
        .checks
        .iter()
        .any(|check| check.expression.trim().is_empty())
    {
        return Err(DdlError::EmptyCheck);
    }
    Ok(())
}

fn validate_identity(spec: &TableSpec, dialect: Dialect) -> Result<(), DdlError> {
    let mut numbered = spec
        .columns
        .iter()
        .filter(|column| column.identity.is_some());
    let Some(first) = numbered.next() else {
        return Ok(());
    };
    let unsupported = |what| DdlError::Unsupported { what, dialect };
    match dialect {
        Dialect::DuckDb | Dialect::ClickHouse => Err(unsupported("identity columns")),
        // Both allow one numbered column per table, which SQLite also needs to be the key.
        Dialect::Mysql | Dialect::SqlServer | Dialect::Sqlite if numbered.next().is_some() => {
            Err(unsupported("second identity column in a table"))
        }
        Dialect::Sqlite if spec.primary_key != [first.name.clone()] => Err(unsupported(
            "AUTOINCREMENT on a column that isn't the whole primary key",
        )),
        _ => Ok(()),
    }
}

fn referential_action_name(action: ReferentialAction) -> &'static str {
    match action {
        ReferentialAction::NoAction => "NO ACTION for foreign keys",
        ReferentialAction::Restrict => "RESTRICT for foreign keys",
        ReferentialAction::Cascade => "CASCADE for foreign keys",
        ReferentialAction::SetNull => "SET NULL for foreign keys",
        ReferentialAction::SetDefault => "SET DEFAULT for foreign keys",
    }
}

pub(super) fn rename(
    table: &ObjectName,
    new_name: &str,
    dialect: Dialect,
) -> Result<String, DdlError> {
    require_name(new_name, "table's new name")?;
    Ok(super::rename("TABLE", table, new_name, dialect))
}

pub(super) fn truncate(table: &ObjectName, dialect: Dialect) -> String {
    let name = table.to_sql(dialect);
    match dialect {
        // SQLite has no `TRUNCATE`; a `DELETE` without `WHERE` empties the table as quickly.
        Dialect::Sqlite => format!("DELETE FROM {name}"),
        _ => format!("TRUNCATE TABLE {name}"),
    }
}

pub(super) fn drop(
    table: &ObjectName,
    cascade: bool,
    dialect: Dialect,
) -> Result<String, DdlError> {
    let name = table.to_sql(dialect);
    if !cascade {
        return Ok(format!("DROP TABLE {name}"));
    }
    if !dialect.capabilities().drop_cascade {
        return Err(DdlError::Unsupported {
            what: "CASCADE for dropping a table",
            dialect,
        });
    }
    Ok(format!("DROP TABLE {name} CASCADE"))
}
