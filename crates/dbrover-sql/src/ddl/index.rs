//! `CREATE INDEX` and `DROP INDEX`.

use std::collections::HashSet;

use super::{DdlError, IndexSpec, ObjectName, column_list, quote_name, require_name};
use crate::Dialect;

pub(super) fn create(spec: &IndexSpec, dialect: Dialect) -> Result<String, DdlError> {
    require_indexes(dialect)?;
    require_name(&spec.name, "index")?;
    require_name(&spec.table.name, "index's table")?;
    if spec.columns.is_empty() || spec.columns.iter().any(|c| c.trim().is_empty()) {
        return Err(DdlError::NoKeyColumns("index"));
    }
    let mut seen = HashSet::new();
    if let Some(column) = spec
        .columns
        .iter()
        .find(|column| !seen.insert(column.as_str()))
    {
        return Err(DdlError::DuplicateColumn(column.clone()));
    }
    if let Some(method) = spec.method
        && !dialect.capabilities().index_methods.contains(&method)
    {
        return Err(DdlError::Unsupported {
            what: method.label(),
            dialect,
        });
    }
    let unique = if spec.unique { "UNIQUE " } else { "" };
    let columns = column_list(dialect, &spec.columns);
    let table = spec.table.to_sql(dialect);
    Ok(match dialect {
        Dialect::Postgres => {
            let method = spec
                .method
                .map(|method| format!(" USING {}", method.keyword()))
                .unwrap_or_default();
            format!(
                "CREATE {unique}INDEX {} ON {table}{method} {columns}",
                quote_name(dialect, &spec.name)
            )
        }
        // SQLite puts the schema on the index's name, and the table must be in that schema,
        // so it is named bare.
        Dialect::Sqlite => format!(
            "CREATE {unique}INDEX {} ON {} {columns}",
            spec.table.sibling(&spec.name).to_sql(dialect),
            quote_name(dialect, &spec.table.name)
        ),
        Dialect::Mysql => {
            let method = spec
                .method
                .map(|method| format!(" USING {}", method.keyword().to_ascii_uppercase()))
                .unwrap_or_default();
            format!(
                "CREATE {unique}INDEX {} ON {table} {columns}{method}",
                quote_name(dialect, &spec.name)
            )
        }
        Dialect::SqlServer | Dialect::DuckDb | Dialect::ClickHouse | Dialect::Cassandra => format!(
            "CREATE {unique}INDEX {} ON {table} {columns}",
            quote_name(dialect, &spec.name)
        ),
    })
}

pub(super) fn drop(table: &ObjectName, index: &str, dialect: Dialect) -> Result<String, DdlError> {
    require_indexes(dialect)?;
    require_name(index, "index")?;
    Ok(match dialect {
        // An index's name is unique within its table only, so the table is named too.
        Dialect::Mysql | Dialect::SqlServer => format!(
            "DROP INDEX {} ON {}",
            quote_name(dialect, index),
            table.to_sql(dialect)
        ),
        Dialect::Postgres
        | Dialect::Sqlite
        | Dialect::DuckDb
        | Dialect::ClickHouse
        | Dialect::Cassandra => {
            format!("DROP INDEX {}", table.sibling(index).to_sql(dialect))
        }
    })
}

fn require_indexes(dialect: Dialect) -> Result<(), DdlError> {
    if dialect.capabilities().indexes {
        Ok(())
    } else {
        Err(DdlError::Unsupported {
            what: "indexes DbRover can create",
            dialect,
        })
    }
}
