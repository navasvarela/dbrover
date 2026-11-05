//! A schema as compared: what a driver describes, normalised and keyed by name.

use std::collections::{BTreeMap, BTreeSet};

use dbrover_core::{IndexDescription, SchemaDescription, SchemaTable, ViewDescription};
use dbrover_sql::{Dialect, ExistingTable, ObjectName, TableSpec, ViewKind};

/// One schema's tables, indexes and views, each collection keyed by name.
///
/// Every collection is a `BTreeMap`, and a table's constraints are sorted by name, so
/// everything built from a `Schema` (the differences, the script, the snapshot file) comes out
/// in the same order whatever order the catalog listed things in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub dialect: Dialect,
    /// The schema's name where it was read. Never compared: a schema may be compared with a
    /// copy of it under another name.
    pub name: String,
    pub tables: BTreeMap<String, Table>,
    pub indexes: BTreeMap<String, Index>,
    pub views: BTreeMap<String, View>,
}

/// A table, as E-track's builder describes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Named without a schema, as are the targets of its foreign keys in the same schema; a
    /// target in another schema keeps it.
    pub spec: TableSpec,
    pub primary_key_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// The table or materialized view it is on.
    pub table: String,
    pub unique: bool,
    /// As the database names it, in lowercase: `btree`, `gin`.
    pub method: String,
    pub key: Vec<String>,
    pub include: Vec<String>,
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub kind: ViewKind,
    pub query: String,
    /// The tables and views of the schema it reads.
    pub reads: BTreeSet<String>,
}

impl Schema {
    /// The schema a driver described, normalised so that two descriptions of the same
    /// schema, or a description and a snapshot of it, are equal.
    pub fn from_description(description: SchemaDescription, dialect: Dialect) -> Self {
        let name = description.schema;
        let tables = description
            .tables
            .into_iter()
            .map(|table| Table::from_description(table, &name, dialect))
            .map(|table| (table.spec.name.name.clone(), table))
            .collect();
        let indexes = description
            .indexes
            .into_iter()
            .map(|index| (index.name.clone(), Index::from(index)))
            .collect();
        let views = description
            .views
            .into_iter()
            .map(|view| (view.name.clone(), View::from(view)))
            .collect();
        Self {
            dialect,
            name,
            tables,
            indexes,
            views,
        }
    }

    /// An empty schema, for building one piece by piece (a snapshot, a test).
    pub fn empty(dialect: Dialect, name: &str) -> Self {
        Self {
            dialect,
            name: name.to_owned(),
            tables: BTreeMap::new(),
            indexes: BTreeMap::new(),
            views: BTreeMap::new(),
        }
    }
}

impl Table {
    fn from_description(table: SchemaTable, schema: &str, dialect: Dialect) -> Self {
        let existing = ExistingTable::from_catalog(table.description, table.constraints, dialect);
        Self {
            spec: existing.spec,
            primary_key_name: existing.primary_key_name,
        }
        .normalised(schema)
    }

    /// The table with its names relative to `schema`, its text trimmed, and its constraints
    /// sorted by name. Doing it again changes nothing.
    pub fn normalised(mut self, schema: &str) -> Self {
        let spec = &mut self.spec;
        spec.name.schema = None;
        for column in &mut spec.columns {
            column.data_type = column.data_type.trim().to_owned();
            column.default = trimmed(column.default.take());
        }
        for key in &mut spec.foreign_keys {
            if key.target.schema.as_deref() == Some(schema) {
                key.target.schema = None;
            }
        }
        for check in &mut spec.checks {
            check.expression = check.expression.trim().to_owned();
        }
        spec.unique_keys.sort_by(|a, b| a.name.cmp(&b.name));
        spec.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
        spec.checks.sort_by(|a, b| a.name.cmp(&b.name));
        self
    }

    pub fn name(&self) -> &str {
        &self.spec.name.name
    }

    /// The table's spec named in `schema`, as are the targets of its foreign keys that are in
    /// the same schema: what a statement run on that schema names.
    pub fn spec_in(&self, schema: &str) -> TableSpec {
        let mut spec = self.spec.clone();
        spec.name = ObjectName::new(Some(schema), &spec.name.name);
        for key in &mut spec.foreign_keys {
            if key.target.schema.is_none() {
                key.target.schema = Some(schema.to_owned());
            }
        }
        spec
    }
}

impl From<IndexDescription> for Index {
    fn from(index: IndexDescription) -> Self {
        Self {
            table: index.table,
            unique: index.unique,
            method: index.method,
            key: index.key,
            include: index.include,
            predicate: index.predicate,
        }
        .normalised()
    }
}

impl Index {
    pub fn normalised(mut self) -> Self {
        self.method = self.method.trim().to_ascii_lowercase();
        trim_all(&mut self.key);
        trim_all(&mut self.include);
        self.predicate = trimmed(self.predicate.take());
        self
    }
}

impl From<ViewDescription> for View {
    fn from(view: ViewDescription) -> Self {
        Self {
            kind: if view.materialized {
                ViewKind::Materialized
            } else {
                ViewKind::View
            },
            query: view.query,
            reads: view.reads.into_iter().collect(),
        }
        .normalised()
    }
}

impl View {
    pub fn normalised(mut self) -> Self {
        self.query = normalise_query(&self.query);
        self
    }
}

/// A query without trailing spaces on its lines, trailing blank lines, or a final `;`. Its
/// start is kept: PostgreSQL begins a view's query with a space, and a snapshot keeps it.
fn normalise_query(query: &str) -> String {
    let lines: Vec<&str> = query.lines().map(str::trim_end).collect();
    let joined = lines.join("\n");
    let without_end = joined.trim_end();
    without_end
        .strip_suffix(';')
        .unwrap_or(without_end)
        .trim_end()
        .to_owned()
}

fn trimmed(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn trim_all(items: &mut [String]) {
    for item in items {
        *item = item.trim().to_owned();
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{
        CheckConstraint, ColumnDescription, ForeignKeyConstraint, ReferentialAction,
        TableConstraints, TableDescription, TableName, UniqueConstraint,
    };

    use super::*;

    fn column(name: &str, data_type: &str) -> ColumnDescription {
        ColumnDescription {
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            nullable: true,
            default: None,
            identity: None,
        }
    }

    fn table_in(schema: &str) -> SchemaTable {
        SchemaTable {
            description: TableDescription {
                schema: schema.to_owned(),
                name: "orders".to_owned(),
                columns: vec![
                    column("id", "integer "),
                    ColumnDescription {
                        default: Some(" now() ".to_owned()),
                        ..column("placed_at", "timestamp with time zone")
                    },
                    ColumnDescription {
                        default: Some("  ".to_owned()),
                        ..column("customer_id", "integer")
                    },
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![],
                referenced_by: vec![],
            },
            constraints: TableConstraints {
                primary_key_name: Some("orders_pkey".to_owned()),
                unique_keys: vec![
                    UniqueConstraint {
                        name: Some("orders_z_key".to_owned()),
                        columns: vec!["placed_at".to_owned()],
                    },
                    UniqueConstraint {
                        name: Some("orders_a_key".to_owned()),
                        columns: vec!["customer_id".to_owned()],
                    },
                ],
                foreign_keys: vec![
                    ForeignKeyConstraint {
                        name: Some("orders_customer_fkey".to_owned()),
                        columns: vec!["customer_id".to_owned()],
                        referenced: TableName {
                            schema: schema.to_owned(),
                            name: "customers".to_owned(),
                        },
                        referenced_columns: vec!["id".to_owned()],
                        on_delete: ReferentialAction::Cascade,
                        on_update: ReferentialAction::NoAction,
                    },
                    ForeignKeyConstraint {
                        name: Some("orders_audit_fkey".to_owned()),
                        columns: vec!["id".to_owned()],
                        referenced: TableName {
                            schema: "audit".to_owned(),
                            name: "events".to_owned(),
                        },
                        referenced_columns: vec!["id".to_owned()],
                        on_delete: ReferentialAction::NoAction,
                        on_update: ReferentialAction::NoAction,
                    },
                ],
                checks: vec![CheckConstraint {
                    name: Some("orders_id_check".to_owned()),
                    expression: " id > 0 ".to_owned(),
                }],
                default_names: BTreeMap::new(),
            },
        }
    }

    fn described(schema: &str) -> SchemaDescription {
        SchemaDescription {
            schema: schema.to_owned(),
            tables: vec![table_in(schema)],
            indexes: vec![IndexDescription {
                name: "orders_placed_idx".to_owned(),
                table: "orders".to_owned(),
                unique: false,
                method: "BTREE ".to_owned(),
                key: vec![" lower(note)".to_owned()],
                include: vec![],
                predicate: Some(" ".to_owned()),
            }],
            views: vec![ViewDescription {
                name: "recent".to_owned(),
                materialized: false,
                query: " SELECT id  \n   FROM orders;\n\n".to_owned(),
                reads: vec!["orders".to_owned()],
            }],
        }
    }

    #[test]
    fn the_same_schema_under_two_names_reads_the_same() {
        let staging = Schema::from_description(described("staging"), Dialect::Postgres);
        let public = Schema::from_description(described("public"), Dialect::Postgres);

        assert_eq!(staging.tables, public.tables);
        assert_eq!(staging.indexes, public.indexes);
        assert_eq!(staging.views, public.views);
    }

    #[test]
    fn names_are_kept_relative_to_the_schema() {
        let schema = Schema::from_description(described("staging"), Dialect::Postgres);
        let table = &schema.tables["orders"];

        assert_eq!(table.spec.name, ObjectName::new(None, "orders"));
        let targets: Vec<&ObjectName> = table
            .spec
            .foreign_keys
            .iter()
            .map(|key| &key.target)
            .collect();
        assert_eq!(
            targets,
            [
                &ObjectName::new(Some("audit"), "events"),
                &ObjectName::new(None, "customers"),
            ]
        );
        let spec = table.spec_in("public");
        assert_eq!(spec.name, ObjectName::new(Some("public"), "orders"));
        assert_eq!(
            spec.foreign_keys[1].target,
            ObjectName::new(Some("public"), "customers")
        );
        assert_eq!(
            spec.foreign_keys[0].target,
            ObjectName::new(Some("audit"), "events")
        );
    }

    #[test]
    fn text_is_trimmed_and_constraints_sorted() {
        let schema = Schema::from_description(described("public"), Dialect::Postgres);
        let spec = &schema.tables["orders"].spec;

        assert_eq!(spec.columns[0].data_type, "integer");
        assert_eq!(spec.columns[1].default.as_deref(), Some("now()"));
        assert_eq!(spec.columns[2].default, None);
        assert_eq!(spec.checks[0].expression, "id > 0");
        let unique: Vec<Option<&str>> = spec
            .unique_keys
            .iter()
            .map(|key| key.name.as_deref())
            .collect();
        assert_eq!(unique, [Some("orders_a_key"), Some("orders_z_key")]);

        let index = &schema.indexes["orders_placed_idx"];
        assert_eq!(index.method, "btree");
        assert_eq!(index.key, ["lower(note)"]);
        assert_eq!(index.predicate, None);
        assert_eq!(schema.views["recent"].query, " SELECT id\n   FROM orders");
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        let schema = Schema::from_description(described("public"), Dialect::Postgres);
        for table in schema.tables.values() {
            assert_eq!(&table.clone().normalised("public"), table);
        }
        for index in schema.indexes.values() {
            assert_eq!(&index.clone().normalised(), index);
        }
        for view in schema.views.values() {
            assert_eq!(&view.clone().normalised(), view);
        }
    }

    #[test]
    fn a_query_loses_only_its_end() {
        let cases = [
            (" SELECT 1;", " SELECT 1"),
            (" SELECT 1 ;  \n", " SELECT 1"),
            ("SELECT a,   \n  b\nFROM t", "SELECT a,\n  b\nFROM t"),
            ("SELECT ';'", "SELECT ';'"),
        ];
        for (query, expected) in cases {
            assert_eq!(normalise_query(query), expected, "{query:?}");
        }
    }
}
