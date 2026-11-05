use std::collections::BTreeSet;

use dbrover_core::{Identity, IdentityGeneration};
use dbrover_sql::{Check, ForeignKey, ObjectName, TableSpec, UniqueKey};

use super::*;

fn column(name: &str, data_type: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        nullable: true,
        default: None,
        identity: None,
    }
}

fn orders() -> Table {
    Table {
        spec: TableSpec {
            name: ObjectName::new(None, "orders"),
            columns: vec![
                ColumnSpec {
                    nullable: false,
                    ..column("id", "integer")
                },
                column("customer_id", "integer"),
                column("total", "numeric(10,2)"),
            ],
            primary_key: vec!["id".to_owned()],
            unique_keys: vec![],
            foreign_keys: vec![ForeignKey {
                name: Some("orders_customer_id_fkey".to_owned()),
                columns: vec!["customer_id".to_owned()],
                target: ObjectName::new(None, "customers"),
                target_columns: vec!["id".to_owned()],
                on_delete: ReferentialAction::NoAction,
                on_update: ReferentialAction::NoAction,
            }],
            checks: vec![Check {
                name: Some("orders_total_check".to_owned()),
                expression: "total >= 0".to_owned(),
            }],
        },
        primary_key_name: Some("orders_pkey".to_owned()),
    }
}

fn index() -> Index {
    Index {
        table: "orders".to_owned(),
        unique: false,
        method: "btree".to_owned(),
        key: vec!["customer_id".to_owned()],
        include: vec![],
        predicate: None,
    }
}

fn view() -> View {
    View {
        kind: ViewKind::View,
        query: " SELECT id\n   FROM orders".to_owned(),
        reads: BTreeSet::from(["orders".to_owned()]),
    }
}

fn schema(name: &str) -> Schema {
    let mut schema = Schema::empty(Dialect::Postgres, name);
    schema.tables.insert("orders".to_owned(), orders());
    schema
        .indexes
        .insert("orders_customer_idx".to_owned(), index());
    schema.views.insert("recent_orders".to_owned(), view());
    schema
}

fn table_differences(comparison: &Comparison) -> &TableDifferences {
    match comparison.tables.as_slice() {
        [
            Difference {
                change: Change::Differs(differences),
                ..
            },
        ] => differences,
        other => panic!("expected one changed table, got {other:?}"),
    }
}

#[test]
fn copies_of_a_schema_under_two_names_are_the_same() {
    let comparison = compare(&schema("staging"), &schema("public")).unwrap();

    assert!(comparison.is_empty());
    assert_eq!(comparison.len(), 0);
}

#[test]
fn schemas_of_two_engines_are_refused() {
    let mut right = schema("public");
    right.dialect = Dialect::Mysql;

    let error = compare(&schema("public"), &right).unwrap_err();

    assert_eq!(
        error.to_string(),
        "A PostgreSQL schema can't be compared with a MySQL one."
    );
}

#[test]
fn objects_on_one_side_only_are_added_or_removed() {
    let left = schema("public");
    let mut right = Schema::empty(Dialect::Postgres, "public");
    right.tables.insert("customers".to_owned(), orders());

    let comparison = compare(&left, &right).unwrap();

    let tables: Vec<(&str, &str)> = comparison
        .tables
        .iter()
        .map(|table| (table.name.as_str(), table.change.mark()))
        .collect();
    assert_eq!(tables, [("customers", "+"), ("orders", "\u{2212}")]);
    assert_eq!(comparison.indexes[0].change, Change::OnlyLeft);
    assert_eq!(comparison.views[0].change, Change::OnlyLeft);
    assert_eq!(comparison.len(), 4);
}

#[test]
fn a_column_added_removed_and_changed() {
    let left = schema("public");
    let mut right = schema("public");
    let columns = &mut right.tables.get_mut("orders").unwrap().spec.columns;
    columns.retain(|column| column.name != "customer_id");
    columns[1] = ColumnSpec {
        nullable: false,
        default: Some("0".to_owned()),
        ..column("total", "numeric(12,2)")
    };
    columns[0].identity = Some(Identity::counting(IdentityGeneration::Always));
    columns.push(column("note", "text"));
    // Column order is never a difference.
    columns.swap(0, 2);

    let comparison = compare(&left, &right).unwrap();

    let differences = table_differences(&comparison);
    let columns: Vec<(&str, Vec<String>)> = differences
        .columns
        .iter()
        .map(|column| {
            let fields = match &column.change {
                Change::Differs(fields) => fields.iter().map(ToString::to_string).collect(),
                other => vec![other.mark().to_owned()],
            };
            (column.name.as_str(), fields)
        })
        .collect();
    assert_eq!(
        columns,
        [
            ("customer_id", vec!["\u{2212}".to_owned()]),
            ("id", vec!["identity: none \u{2192} always".to_owned()]),
            ("note", vec!["+".to_owned()]),
            (
                "total",
                vec![
                    "type: numeric(10,2) \u{2192} numeric(12,2)".to_owned(),
                    "nullable: yes \u{2192} no".to_owned(),
                    "default: none \u{2192} 0".to_owned(),
                ]
            ),
        ]
    );
    assert!(differences.constraints.is_empty());
    assert_eq!(
        differences.summary(),
        "1 column added, 1 column removed, 2 columns changed"
    );
}

#[test]
fn constraints_are_matched_by_kind_and_name() {
    let left = schema("public");
    let mut right = schema("public");
    let table = right.tables.get_mut("orders").unwrap();
    table.primary_key_name = Some("orders_id_pkey".to_owned());
    table.spec.unique_keys.push(UniqueKey {
        name: Some("orders_total_key".to_owned()),
        columns: vec!["total".to_owned()],
    });
    table.spec.foreign_keys[0].on_delete = ReferentialAction::Cascade;
    table.spec.checks.clear();

    let comparison = compare(&left, &right).unwrap();

    let differences = table_differences(&comparison);
    assert!(differences.columns.is_empty());
    let constraints: Vec<(ConstraintKind, &str, String)> = differences
        .constraints
        .iter()
        .map(|constraint| {
            let change = match &constraint.change {
                Change::Differs(change) => change.to_string(),
                other => other.mark().to_owned(),
            };
            (constraint.kind, constraint.name.as_str(), change)
        })
        .collect();
    assert_eq!(
        constraints,
        [
            (
                ConstraintKind::PrimaryKey,
                "orders_pkey",
                "primary key: orders_pkey (id) \u{2192} orders_id_pkey (id)".to_owned()
            ),
            (ConstraintKind::Unique, "orders_total_key", "+".to_owned()),
            (
                ConstraintKind::ForeignKey,
                "orders_customer_id_fkey",
                "foreign key: (customer_id) \u{2192} customers (id) \u{2192} (customer_id) \
                 \u{2192} customers (id) on delete cascade"
                    .to_owned()
            ),
            (
                ConstraintKind::Check,
                "orders_total_check",
                "\u{2212}".to_owned()
            ),
        ]
    );
    assert_eq!(
        differences.summary(),
        "1 primary key changed, 1 unique key added, 1 foreign key changed, 1 check removed"
    );
}

#[test]
fn a_primary_key_added_or_dropped() {
    let left = schema("public");
    let mut right = schema("public");
    right
        .tables
        .get_mut("orders")
        .unwrap()
        .spec
        .primary_key
        .clear();

    let comparison = compare(&left, &right).unwrap();

    let constraint = &table_differences(&comparison).constraints[0];
    assert_eq!(constraint.kind, ConstraintKind::PrimaryKey);
    assert_eq!(constraint.name, "orders_pkey");
    assert_eq!(constraint.change, Change::OnlyLeft);
}

#[test]
fn a_foreign_key_into_another_schema_differs_from_one_into_this_one() {
    let left = schema("public");
    let mut right = schema("public");
    right.tables.get_mut("orders").unwrap().spec.foreign_keys[0].target =
        ObjectName::new(Some("crm"), "customers");

    let comparison = compare(&left, &right).unwrap();

    let constraint = &table_differences(&comparison).constraints[0];
    let Change::Differs(change) = &constraint.change else {
        panic!("expected a changed foreign key, got {constraint:?}");
    };
    assert_eq!(change.right, "(customer_id) \u{2192} crm.customers (id)");
}

#[test]
fn every_field_of_an_index_is_compared() {
    let left = schema("public");
    let mut right = schema("public");
    *right.indexes.get_mut("orders_customer_idx").unwrap() = Index {
        table: "archived_orders".to_owned(),
        unique: true,
        method: "hash".to_owned(),
        key: vec!["customer_id".to_owned(), "lower(note)".to_owned()],
        include: vec!["total".to_owned()],
        predicate: Some("total > 0".to_owned()),
    };

    let comparison = compare(&left, &right).unwrap();

    let [
        Difference {
            change: Change::Differs(fields),
            ..
        },
    ] = comparison.indexes.as_slice()
    else {
        panic!("expected one changed index, got {:?}", comparison.indexes);
    };
    let fields: Vec<String> = fields.iter().map(ToString::to_string).collect();
    assert_eq!(
        fields,
        [
            "table: orders \u{2192} archived_orders",
            "unique: no \u{2192} yes",
            "method: btree \u{2192} hash",
            "key: (customer_id) \u{2192} (customer_id, lower(note))",
            "include: none \u{2192} (total)",
            "where: none \u{2192} total > 0",
        ]
    );
}

#[test]
fn a_view_differs_by_kind_and_query_but_not_by_what_it_reads() {
    let left = schema("public");
    let mut right = schema("public");
    let view = right.views.get_mut("recent_orders").unwrap();
    view.kind = ViewKind::Materialized;
    view.query = " SELECT id,\n    total\n   FROM orders".to_owned();
    view.reads.insert("customers".to_owned());

    let comparison = compare(&left, &right).unwrap();

    let [
        Difference {
            change: Change::Differs(fields),
            ..
        },
    ] = comparison.views.as_slice()
    else {
        panic!("expected one changed view, got {:?}", comparison.views);
    };
    let fields: Vec<String> = fields.iter().map(ToString::to_string).collect();
    assert_eq!(
        fields,
        ["kind: view \u{2192} materialized view", "query changed"]
    );

    let mut reads_more = schema("public");
    reads_more
        .views
        .get_mut("recent_orders")
        .unwrap()
        .reads
        .insert("customers".to_owned());
    assert!(compare(&left, &reads_more).unwrap().is_empty());
}

#[test]
fn identities_say_where_they_start_when_not_from_one() {
    let mut left = schema("public");
    let mut right = schema("public");
    left.tables.get_mut("orders").unwrap().spec.columns[0].identity =
        Some(Identity::counting(IdentityGeneration::ByDefault));
    right.tables.get_mut("orders").unwrap().spec.columns[0].identity = Some(Identity {
        generation: IdentityGeneration::ByDefault,
        start: 100,
        increment: 10,
    });

    let comparison = compare(&left, &right).unwrap();

    let Change::Differs(fields) = &table_differences(&comparison).columns[0].change else {
        panic!("expected a changed column");
    };
    assert_eq!(
        fields[0].to_string(),
        "identity: by default \u{2192} by default, from 100 by 10"
    );
}
