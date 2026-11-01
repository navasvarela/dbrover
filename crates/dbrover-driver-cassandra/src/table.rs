//! Describing a table for editing its rows: its columns with their CQL types, and its primary
//! key, the partition key's columns then the clustering key's.

use dbrover_core::{ColumnDescription, DriverError, Identifier, RelationName, TableDescription};
use scylla::client::session::Session;

use crate::catalog::{self, ColumnRow};

/// The table `table` names, resolved as a statement on this session would: in its keyspace, or
/// the session's. `None` for a name that isn't a table, a materialized view among them, whose
/// rows can't be written.
pub(crate) async fn describe_table(
    session: &Session,
    table: &RelationName,
) -> Result<Option<TableDescription>, DriverError> {
    let keyspace = match &table.schema {
        Some(keyspace) => as_stored(keyspace),
        None => match session.get_keyspace() {
            Some(keyspace) => keyspace.to_string(),
            None => return Ok(None),
        },
    };
    let name = as_stored(&table.name);
    let tables: Vec<(String,)> = catalog::rows(
        session,
        "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ? AND table_name = ?",
        (keyspace.as_str(), name.as_str()),
    )
    .await?;
    if tables.is_empty() {
        return Ok(None);
    }
    let columns = catalog::column_rows(session, &keyspace, &name).await?;
    Ok(Some(description(keyspace, name, columns)))
}

/// A name as `system_schema` keeps it: CQL folds an unquoted name to lowercase.
fn as_stored(identifier: &Identifier) -> String {
    if identifier.quoted {
        identifier.name.clone()
    } else {
        identifier.name.to_lowercase()
    }
}

/// `columns`, in key order, as a table's description.
fn description(keyspace: String, name: String, columns: Vec<ColumnRow>) -> TableDescription {
    let primary_key = columns
        .iter()
        .filter(|column| is_key(column))
        .map(|column| column.column_name.clone())
        .collect();
    TableDescription {
        schema: keyspace,
        name,
        columns: columns
            .into_iter()
            .map(|column| ColumnDescription {
                nullable: !is_key(&column),
                name: column.column_name,
                data_type: column.column_type,
                default: None,
                identity: None,
            })
            .collect(),
        primary_key,
        foreign_keys: Vec::new(),
        referenced_by: Vec::new(),
    }
}

fn is_key(column: &ColumnRow) -> bool {
    matches!(column.kind.as_str(), "partition_key" | "clustering")
}
