//! The columns of the rows being exported.

use std::collections::HashSet;

use dbrover_core::{ColumnInfo, ValueKind};
use dbrover_sql::Dialect;

/// A column of the rows being exported: its name, and what the formats that write types make
/// of its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportColumn {
    pub name: String,
    /// The type as the driver names it, when it reports one.
    pub type_name: Option<String>,
    pub kind: ValueKind,
    /// Excel's number format for the column's cells, when a report's output declares one
    /// (`#,##0.00`). Other formats don't read it.
    pub excel_format: Option<String>,
}

impl ExportColumn {
    /// A result column from a data source of `dialect`.
    pub fn new(column: &ColumnInfo, dialect: Dialect) -> Self {
        Self {
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            kind: dialect.value_kind(column.type_name.as_deref()),
            excel_format: None,
        }
    }

    /// A result column, typed by `dialect`'s type names when the data source has a SQL
    /// dialect, and undeclared when it speaks another language.
    pub fn typed_by(column: &ColumnInfo, dialect: Option<Dialect>) -> Self {
        match dialect {
            Some(dialect) => Self::new(column, dialect),
            None => Self {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                kind: ValueKind::Undeclared,
                excel_format: None,
            },
        }
    }

    /// A column of text, for rows DbRover makes itself, such as the history's.
    pub fn text(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: None,
            kind: ValueKind::Text,
            excel_format: None,
        }
    }
}

/// Column names as keys of an object: a repeated name (`SELECT a.id, b.id`) gets `_2`, `_3`,
/// … so no value overwrites another.
pub(crate) fn unique_keys(columns: &[ExportColumn]) -> Vec<String> {
    let mut used = HashSet::new();
    columns
        .iter()
        .map(|column| {
            let name = &column.name;
            let key = (1..)
                .map(|n| match n {
                    1 => name.clone(),
                    n => format!("{name}_{n}"),
                })
                .find(|key| !used.contains(key))
                .unwrap_or_else(|| name.clone());
            used.insert(key.clone());
            key
        })
        .collect()
}
