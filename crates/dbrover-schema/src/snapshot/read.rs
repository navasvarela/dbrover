//! Reading a snapshot file back into a [`Schema`], with serde: the file's layout is the
//! writer's business, its contents this module's.

use std::collections::BTreeMap;

use dbrover_core::{Identity, IdentityGeneration};
use dbrover_sql::{
    Check, ColumnSpec, ForeignKey, ObjectName, ReferentialAction, TableSpec, UniqueKey, ViewKind,
};
use serde::Deserialize;

use super::write::{generation_name, view_kind_name};
use super::{FORMAT, SnapshotError, dialect_named};
use crate::{Index, Schema, Table, View};

/// Reads a snapshot file, normalised as a described schema is, so a snapshot compares with
/// its own schema as empty.
///
/// # Errors
///
/// [`SnapshotError`] for a file that isn't a snapshot, one a newer DbRover wrote, or one with
/// a value DbRover doesn't know.
pub fn read_snapshot(text: &str) -> Result<Schema, SnapshotError> {
    let header: HeaderOnly = toml::from_str(text).map_err(|error| invalid(&error))?;
    if header.snapshot.format > FORMAT {
        return Err(SnapshotError::Newer(header.snapshot.format));
    }
    let file: File = toml::from_str(text).map_err(|error| invalid(&error))?;
    let dialect = dialect_named(&file.snapshot.engine)
        .ok_or_else(|| SnapshotError::Engine(file.snapshot.engine.clone()))?;
    let schema_name = file.snapshot.schema;
    let mut schema = Schema::empty(dialect, &schema_name);
    for (name, table) in file.tables {
        let table = table.into_table(&name)?.normalised(&schema_name);
        schema.tables.insert(name, table);
    }
    for (name, index) in file.indexes {
        schema.indexes.insert(name, index.into_index().normalised());
    }
    for (name, view) in file.views {
        schema.views.insert(name, view.into_view()?.normalised());
    }
    Ok(schema)
}

fn invalid(error: &toml::de::Error) -> SnapshotError {
    SnapshotError::Invalid(error.message().to_owned())
}

#[derive(Deserialize)]
struct HeaderOnly {
    snapshot: FormatOnly,
}

#[derive(Deserialize)]
struct FormatOnly {
    format: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    snapshot: Header,
    #[serde(default)]
    tables: BTreeMap<String, TableEntry>,
    #[serde(default)]
    indexes: BTreeMap<String, IndexEntry>,
    #[serde(default)]
    views: BTreeMap<String, ViewEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    #[expect(dead_code, reason = "checked before the whole file is read")]
    format: u32,
    engine: String,
    schema: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableEntry {
    columns: Vec<ColumnEntry>,
    primary_key: Option<KeyEntry>,
    #[serde(default)]
    unique_keys: Vec<KeyEntry>,
    #[serde(default)]
    foreign_keys: Vec<ForeignKeyEntry>,
    #[serde(default)]
    checks: Vec<CheckEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnEntry {
    name: String,
    #[serde(rename = "type")]
    data_type: String,
    #[serde(default = "nullable_by_default")]
    nullable: bool,
    default: Option<String>,
    identity: Option<IdentityEntry>,
}

fn nullable_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IdentityEntry {
    Counting(String),
    Numbered {
        generation: String,
        start: i64,
        increment: i64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    name: Option<String>,
    columns: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForeignKeyEntry {
    name: Option<String>,
    columns: Vec<String>,
    references: String,
    references_schema: Option<String>,
    referenced_columns: Vec<String>,
    on_delete: Option<String>,
    on_update: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckEntry {
    name: Option<String>,
    condition: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexEntry {
    table: String,
    #[serde(default)]
    unique: bool,
    method: String,
    key: Vec<String>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(rename = "where")]
    predicate: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewEntry {
    kind: String,
    #[serde(default)]
    reads: Vec<String>,
    query: String,
}

impl TableEntry {
    fn into_table(self, name: &str) -> Result<Table, SnapshotError> {
        let columns = self
            .columns
            .into_iter()
            .map(ColumnEntry::into_column)
            .collect::<Result<_, _>>()?;
        let (primary_key_name, primary_key) = match self.primary_key {
            Some(key) => (key.name, key.columns),
            None => (None, Vec::new()),
        };
        let foreign_keys = self
            .foreign_keys
            .into_iter()
            .map(ForeignKeyEntry::into_foreign_key)
            .collect::<Result<_, _>>()?;
        Ok(Table {
            spec: TableSpec {
                name: ObjectName::new(None, name),
                columns,
                primary_key,
                unique_keys: self
                    .unique_keys
                    .into_iter()
                    .map(|key| UniqueKey {
                        name: key.name,
                        columns: key.columns,
                    })
                    .collect(),
                foreign_keys,
                checks: self
                    .checks
                    .into_iter()
                    .map(|check| Check {
                        name: check.name,
                        expression: check.condition,
                    })
                    .collect(),
            },
            primary_key_name,
        })
    }
}

impl ColumnEntry {
    fn into_column(self) -> Result<ColumnSpec, SnapshotError> {
        let identity = self
            .identity
            .map(IdentityEntry::into_identity)
            .transpose()?;
        Ok(ColumnSpec {
            name: self.name,
            data_type: self.data_type,
            nullable: self.nullable,
            default: self.default,
            identity,
        })
    }
}

impl IdentityEntry {
    fn into_identity(self) -> Result<Identity, SnapshotError> {
        let (generation, start, increment) = match self {
            Self::Counting(generation) => (generation, 1, 1),
            Self::Numbered {
                generation,
                start,
                increment,
            } => (generation, start, increment),
        };
        let generation = [IdentityGeneration::Always, IdentityGeneration::ByDefault]
            .into_iter()
            .find(|candidate| generation_name(*candidate) == generation)
            .ok_or(SnapshotError::Value {
                what: "identity",
                value: generation,
            })?;
        Ok(Identity {
            generation,
            start,
            increment,
        })
    }
}

impl ForeignKeyEntry {
    fn into_foreign_key(self) -> Result<ForeignKey, SnapshotError> {
        Ok(ForeignKey {
            name: self.name,
            columns: self.columns,
            target: ObjectName {
                schema: self.references_schema,
                name: self.references,
            },
            target_columns: self.referenced_columns,
            on_delete: action(self.on_delete)?,
            on_update: action(self.on_update)?,
        })
    }
}

fn action(name: Option<String>) -> Result<ReferentialAction, SnapshotError> {
    let Some(name) = name else {
        return Ok(ReferentialAction::NoAction);
    };
    ReferentialAction::from_catalog(&name).ok_or(SnapshotError::Value {
        what: "foreign key action",
        value: name,
    })
}

impl IndexEntry {
    fn into_index(self) -> Index {
        Index {
            table: self.table,
            unique: self.unique,
            method: self.method,
            key: self.key,
            include: self.include,
            predicate: self.predicate,
        }
    }
}

impl ViewEntry {
    fn into_view(self) -> Result<View, SnapshotError> {
        let kind = [ViewKind::View, ViewKind::Materialized]
            .into_iter()
            .find(|kind| view_kind_name(*kind) == self.kind)
            .ok_or(SnapshotError::Value {
                what: "view kind",
                value: self.kind,
            })?;
        Ok(View {
            kind,
            query: self.query,
            reads: self.reads.into_iter().collect(),
        })
    }
}
