use std::collections::BTreeSet;

use dbrover_core::{Identity, IdentityGeneration};
use dbrover_sql::{
    Check, ColumnSpec, ForeignKey, ObjectName, ReferentialAction, TableSpec, UniqueKey, ViewKind,
};

use super::*;
use crate::{Index, Schema, Table, View, compare};

fn column(name: &str, data_type: &str, nullable: bool) -> ColumnSpec {
    ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        nullable,
        default: None,
        identity: None,
    }
}

/// The design's example (docs/schema-compare.md, "The format").
fn shop() -> Schema {
    let mut schema = Schema::empty(Dialect::Postgres, "public");
    schema.tables.insert(
        "customers".to_owned(),
        Table {
            spec: TableSpec {
                name: ObjectName::new(None, "customers"),
                columns: vec![
                    ColumnSpec {
                        identity: Some(Identity::counting(IdentityGeneration::Always)),
                        ..column("id", "integer", false)
                    },
                    column("email", "text", false),
                    ColumnSpec {
                        default: Some("now()".to_owned()),
                        ..column("created_at", "timestamp with time zone", false)
                    },
                ],
                primary_key: vec!["id".to_owned()],
                unique_keys: vec![UniqueKey {
                    name: Some("customers_email_key".to_owned()),
                    columns: vec!["email".to_owned()],
                }],
                foreign_keys: vec![],
                checks: vec![],
            },
            primary_key_name: Some("customers_pkey".to_owned()),
        },
    );
    schema.tables.insert(
        "orders".to_owned(),
        Table {
            spec: TableSpec {
                name: ObjectName::new(None, "orders"),
                columns: vec![
                    column("id", "bigserial", false),
                    column("customer_id", "integer", false),
                    column("total", "numeric(12,2)", false),
                ],
                primary_key: vec!["id".to_owned()],
                unique_keys: vec![],
                foreign_keys: vec![ForeignKey {
                    name: Some("orders_customer_id_fkey".to_owned()),
                    columns: vec!["customer_id".to_owned()],
                    target: ObjectName::new(None, "customers"),
                    target_columns: vec!["id".to_owned()],
                    on_delete: ReferentialAction::Cascade,
                    on_update: ReferentialAction::NoAction,
                }],
                checks: vec![Check {
                    name: Some("orders_total_check".to_owned()),
                    expression: "total >= 0::numeric".to_owned(),
                }],
            },
            primary_key_name: Some("orders_pkey".to_owned()),
        },
    );
    schema.indexes.insert(
        "orders_customer_id_idx".to_owned(),
        Index {
            table: "orders".to_owned(),
            unique: false,
            method: "btree".to_owned(),
            key: vec!["customer_id".to_owned()],
            include: vec![],
            predicate: None,
        },
    );
    schema.views.insert(
        "recent_orders".to_owned(),
        View {
            kind: ViewKind::View,
            query: " SELECT id,\n    customer_id,\n    total\n   FROM orders\n  WHERE id > 1000"
                .to_owned(),
            reads: BTreeSet::from(["orders".to_owned()]),
        },
    );
    schema
}

const SHOP: &str = r#"# A schema snapshot written by DbRover. Compare it with a live schema in DbRover's schema
# compare, or with `dbrover schema compare`. Regenerated whole each time; edits are lost.

[snapshot]
format = 1
engine = "postgresql"
schema = "public"

[tables.customers]
columns = [
  { name = "id", type = "integer", nullable = false, identity = "always" },
  { name = "email", type = "text", nullable = false },
  { name = "created_at", type = "timestamp with time zone", nullable = false, default = "now()" },
]
primary_key = { name = "customers_pkey", columns = ["id"] }
unique_keys = [
  { name = "customers_email_key", columns = ["email"] },
]

[tables.orders]
columns = [
  { name = "id", type = "bigserial", nullable = false },
  { name = "customer_id", type = "integer", nullable = false },
  { name = "total", type = "numeric(12,2)", nullable = false },
]
primary_key = { name = "orders_pkey", columns = ["id"] }
foreign_keys = [
  { name = "orders_customer_id_fkey", columns = ["customer_id"], references = "customers", referenced_columns = ["id"], on_delete = "cascade" },
]
checks = [
  { name = "orders_total_check", condition = "total >= 0::numeric" },
]

[indexes.orders_customer_id_idx]
table = "orders"
method = "btree"
key = ["customer_id"]

[views.recent_orders]
kind = "view"
reads = ["orders"]
query = """
 SELECT id,
    customer_id,
    total
   FROM orders
  WHERE id > 1000"""
"#;

#[test]
fn writes_the_designs_example() {
    assert_eq!(write_snapshot(&shop()), SHOP);
}

#[test]
fn reads_back_what_it_wrote() {
    let read = read_snapshot(SHOP).unwrap();

    assert_eq!(read, shop());
    assert_eq!(write_snapshot(&read), SHOP);
    assert!(compare(&read, &shop()).unwrap().is_empty());
}

/// Everything a snapshot can hold that the example doesn't, written and read back unchanged.
#[test]
fn round_trips_the_unusual() {
    let mut schema = shop();
    let orders = schema.tables.get_mut("orders").unwrap();
    orders.spec.columns.push(ColumnSpec {
        identity: Some(Identity {
            generation: IdentityGeneration::ByDefault,
            start: 100,
            increment: 10,
        }),
        ..column("number", "bigint", false)
    });
    orders.spec.columns.push(ColumnSpec {
        default: Some("'say \"hi\"\\n'::text".to_owned()),
        ..column("Odd \"name\"", "text", true)
    });
    orders.spec.foreign_keys.push(ForeignKey {
        name: Some("orders_auditor_fkey".to_owned()),
        columns: vec!["number".to_owned()],
        target: ObjectName::new(Some("audit"), "auditors"),
        target_columns: vec!["id".to_owned()],
        on_delete: ReferentialAction::SetNull,
        on_update: ReferentialAction::Restrict,
    });
    schema.tables.insert(
        "Order Items".to_owned(),
        Table {
            spec: TableSpec {
                name: ObjectName::new(None, "Order Items"),
                columns: vec![column("note", "text", true)],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
                checks: vec![],
            },
            primary_key_name: None,
        },
    );
    schema.indexes.insert(
        "orders_lookup_idx".to_owned(),
        Index {
            table: "orders".to_owned(),
            unique: true,
            method: "btree".to_owned(),
            key: vec![
                "customer_id DESC NULLS LAST".to_owned(),
                "lower(\"Odd \"\"name\"\"\")".to_owned(),
            ],
            include: vec!["total".to_owned()],
            predicate: Some("(total > (0)::numeric)".to_owned()),
        },
    );
    schema.views.insert(
        "quoted".to_owned(),
        View {
            kind: ViewKind::Materialized,
            query: " SELECT '\"\"\"' AS q,\n    '\\' AS slash\n   FROM \"Order Items\"".to_owned(),
            reads: BTreeSet::from(["Order Items".to_owned()]),
        },
    );

    let orders = schema.tables.remove("orders").unwrap();
    schema
        .tables
        .insert("orders".to_owned(), orders.normalised("public"));

    let written = write_snapshot(&schema);
    let read = read_snapshot(&written).unwrap();

    assert_eq!(read, schema);
    assert_eq!(write_snapshot(&read), written);
    assert!(written.contains("[tables.\"Order Items\"]\ncolumns = [\n"));
    assert!(
        written.contains("identity = { generation = \"by default\", start = 100, increment = 10 }")
    );
    assert!(written.contains(
        "references = \"auditors\", references_schema = \"audit\", referenced_columns = \
         [\"id\"], on_delete = \"set null\", on_update = \"restrict\""
    ));
}

#[test]
fn an_empty_schema_is_its_header() {
    let schema = Schema::empty(Dialect::Postgres, "empty");

    let written = write_snapshot(&schema);

    assert!(
        written.ends_with("[snapshot]\nformat = 1\nengine = \"postgresql\"\nschema = \"empty\"\n")
    );
    assert_eq!(read_snapshot(&written).unwrap(), schema);
}

#[test]
fn refuses_what_it_cant_read() {
    let newer = SHOP.replace("format = 1", "format = 2");
    let engine = SHOP.replace("\"postgresql\"", "\"oracle\"");
    let action = SHOP.replace("\"cascade\"", "\"explode\"");
    let unknown = SHOP.replace("method = \"btree\"", "method = \"btree\"\ncolour = \"red\"");
    let cases = [
        (
            newer.as_str(),
            "It was written by a newer DbRover (format 2); update DbRover to read it.",
        ),
        (
            engine.as_str(),
            "It names an engine DbRover doesn't know: \u{201c}oracle\u{201d}.",
        ),
        (
            action.as_str(),
            "It has a foreign key action DbRover doesn't know: \u{201c}explode\u{201d}.",
        ),
    ];
    for (text, message) in cases {
        assert_eq!(read_snapshot(text).unwrap_err().to_string(), message);
    }
    assert!(matches!(
        read_snapshot(&unknown),
        Err(SnapshotError::Invalid(_))
    ));
    assert!(matches!(
        read_snapshot("SELECT 1"),
        Err(SnapshotError::Invalid(_))
    ));
}
