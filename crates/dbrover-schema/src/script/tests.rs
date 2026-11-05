use dbrover_core::{Identity, IdentityGeneration};
use dbrover_sql::{Check, ColumnSpec, ForeignKey, ReferentialAction, UniqueKey};

use super::*;
use crate::{Index, View};

fn column(name: &str, data_type: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        nullable: true,
        default: None,
        identity: None,
    }
}

fn key_column(name: &str, data_type: &str) -> ColumnSpec {
    ColumnSpec {
        nullable: false,
        ..column(name, data_type)
    }
}

fn table(name: &str, columns: Vec<ColumnSpec>) -> Table {
    Table {
        spec: TableSpec {
            name: ObjectName::new(None, name),
            columns,
            primary_key: vec!["id".to_owned()],
            unique_keys: vec![],
            foreign_keys: vec![],
            checks: vec![],
        },
        primary_key_name: Some(format!("{name}_pkey")),
    }
}

fn foreign_key(name: &str, column: &str, target: &str) -> ForeignKey {
    ForeignKey {
        name: Some(name.to_owned()),
        columns: vec![column.to_owned()],
        target: ObjectName::new(None, target),
        target_columns: vec!["id".to_owned()],
        on_delete: ReferentialAction::NoAction,
        on_update: ReferentialAction::NoAction,
    }
}

fn customers() -> Table {
    table(
        "customers",
        vec![key_column("id", "serial"), key_column("email", "text")],
    )
}

fn orders() -> Table {
    let mut orders = table(
        "orders",
        vec![
            key_column("id", "bigserial"),
            key_column("customer_id", "integer"),
            column("legacy_code", "text"),
        ],
    );
    orders.spec.foreign_keys = vec![foreign_key(
        "orders_customer_id_fkey",
        "customer_id",
        "customers",
    )];
    orders.spec.checks = vec![Check {
        name: Some("orders_id_check".to_owned()),
        expression: "(id > 0)".to_owned(),
    }];
    orders
}

fn index(table: &str, key: &str) -> Index {
    Index {
        table: table.to_owned(),
        unique: false,
        method: "btree".to_owned(),
        key: vec![key.to_owned()],
        include: vec![],
        predicate: None,
    }
}

fn view(kind: ViewKind, query: &str, reads: &[&str]) -> View {
    View {
        kind,
        query: query.to_owned(),
        reads: reads.iter().map(|read| (*read).to_owned()).collect(),
    }
}

fn shop(name: &str) -> Schema {
    let mut schema = Schema::empty(Dialect::Postgres, name);
    schema.tables.insert("customers".to_owned(), customers());
    schema.tables.insert("orders".to_owned(), orders());
    schema
}

fn sides() -> Sides {
    Sides {
        left: "schema staging on shop-dev".to_owned(),
        right: "schema public on shop".to_owned(),
    }
}

fn script_of(left: &Schema, right: &Schema) -> Script {
    script(left, right, &sides())
        .unwrap()
        .expect("the schemas differ")
}

/// The script's statements without its header, comments and blank lines.
fn statements(script: &Script) -> Vec<&str> {
    script
        .sql
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("--"))
        .collect()
}

#[test]
fn the_same_schemas_have_no_script() {
    assert_eq!(
        script(&shop("staging"), &shop("public"), &sides()),
        Ok(None)
    );
}

#[test]
fn schemas_of_two_engines_have_no_script() {
    let mut right = shop("public");
    right.dialect = Dialect::Mysql;

    assert!(script(&shop("staging"), &right, &sides()).is_err());
}

/// W13's exit: a column and an index, written as the design's example shows.
#[test]
fn a_column_and_an_index() {
    let left = shop("staging");
    let mut right = shop("public");
    let orders = right.tables.get_mut("orders").unwrap();
    orders
        .spec
        .columns
        .retain(|column| column.name != "legacy_code");
    orders.spec.columns.push(column("note", "text"));
    right.indexes.insert(
        "orders_customer_id_idx".to_owned(),
        index("orders", "customer_id"),
    );

    let script = script_of(&left, &right);

    assert_eq!(
        script.sql,
        "\
-- Changes schema staging on shop-dev (left) to match schema public on shop (right).
-- Written by DbRover's schema compare. Read it before running it: nothing has run.
-- Columns are matched by name, so a renamed column is dropped and added again: edit
-- the script to rename it instead, or its data is lost.

BEGIN;
-- The definitions below name the schema's own objects without the schema.
SET LOCAL search_path TO staging;

-- Drops column orders.legacy_code and the values in it.
ALTER TABLE staging.orders DROP COLUMN legacy_code;
ALTER TABLE staging.orders ADD COLUMN note text;

CREATE INDEX orders_customer_id_idx ON staging.orders USING btree (customer_id);

COMMIT;
"
    );
    assert!(script.not_written.is_empty());
}

#[test]
fn foreign_keys_go_before_tables_and_come_after_them() {
    let mut left = shop("staging");
    let mut audit = table("audit", vec![key_column("id", "integer")]);
    audit.spec.foreign_keys = vec![foreign_key("audit_id_fkey", "id", "orders")];
    left.tables.insert("audit".to_owned(), audit);
    let mut right = shop("public");
    right.tables.get_mut("orders").unwrap().spec.foreign_keys = vec![foreign_key(
        "orders_customer_id_fkey",
        "customer_id",
        "accounts",
    )];
    let mut accounts = table("accounts", vec![key_column("id", "integer")]);
    accounts.spec.foreign_keys = vec![foreign_key("accounts_id_fkey", "id", "accounts")];
    right.tables.insert("accounts".to_owned(), accounts);

    let script = script_of(&left, &right);

    assert_eq!(
        statements(&script),
        [
            "BEGIN;",
            "SET LOCAL search_path TO staging;",
            "ALTER TABLE staging.audit DROP CONSTRAINT audit_id_fkey;",
            "ALTER TABLE staging.orders DROP CONSTRAINT orders_customer_id_fkey;",
            "DROP TABLE staging.audit;",
            "CREATE TABLE staging.accounts (",
            "    id integer NOT NULL,",
            "    PRIMARY KEY (id)",
            ");",
            "ALTER TABLE staging.accounts ADD CONSTRAINT accounts_id_fkey FOREIGN KEY (id) \
             REFERENCES staging.accounts (id);",
            "ALTER TABLE staging.orders ADD CONSTRAINT orders_customer_id_fkey FOREIGN KEY \
             (customer_id) REFERENCES staging.accounts (id);",
            "COMMIT;",
        ]
    );
    assert!(
        script
            .sql
            .contains("-- Drops table audit and every row in it.\n")
    );
}

#[test]
fn a_key_that_points_at_a_changing_key_is_dropped_and_added_again() {
    let left = shop("staging");
    let mut right = shop("public");
    let customers = right.tables.get_mut("customers").unwrap();
    customers.spec.unique_keys = vec![UniqueKey {
        name: Some("customers_email_key".to_owned()),
        columns: vec!["email".to_owned()],
    }];

    let script = script_of(&left, &right);

    assert_eq!(
        statements(&script)[2..5],
        [
            "ALTER TABLE staging.orders DROP CONSTRAINT orders_customer_id_fkey;",
            "ALTER TABLE staging.customers ADD CONSTRAINT customers_email_key UNIQUE (email);",
            "ALTER TABLE staging.orders ADD CONSTRAINT orders_customer_id_fkey FOREIGN KEY \
             (customer_id) REFERENCES staging.customers (id);",
        ]
    );
}

#[test]
fn views_over_a_changing_table_are_dropped_and_created_again_in_order() {
    let mut left = shop("staging");
    let mut right = shop("public");
    for schema in [&mut left, &mut right] {
        schema.views.insert(
            "big_orders".to_owned(),
            view(ViewKind::View, " SELECT id\n   FROM orders", &["orders"]),
        );
        schema.views.insert(
            "a_totals".to_owned(),
            view(
                ViewKind::Materialized,
                " SELECT count(*) AS n\n   FROM big_orders",
                &["big_orders"],
            ),
        );
        schema.views.insert(
            "customer_list".to_owned(),
            view(
                ViewKind::View,
                " SELECT email\n   FROM customers",
                &["customers"],
            ),
        );
        schema
            .indexes
            .insert("a_totals_n_idx".to_owned(), index("a_totals", "n"));
    }
    right
        .tables
        .get_mut("orders")
        .unwrap()
        .spec
        .columns
        .push(column("note", "text"));

    let script = script_of(&left, &right);

    assert_eq!(
        statements(&script),
        [
            "BEGIN;",
            "SET LOCAL search_path TO staging;",
            "DROP MATERIALIZED VIEW staging.a_totals;",
            "DROP VIEW staging.big_orders;",
            "ALTER TABLE staging.orders ADD COLUMN note text;",
            "CREATE VIEW staging.big_orders AS",
            "SELECT id",
            "   FROM orders;",
            "CREATE MATERIALIZED VIEW staging.a_totals AS",
            "SELECT count(*) AS n",
            "   FROM big_orders;",
            "CREATE INDEX a_totals_n_idx ON staging.a_totals USING btree (n);",
            "COMMIT;",
        ]
    );
    assert!(
        script
            .sql
            .contains("-- Drops materialized view a_totals and the rows it stores.\n")
    );
}

#[test]
fn what_cant_change_in_place_is_left_as_a_comment_and_the_rest_written() {
    let left = shop("staging");
    let mut right = shop("public");
    let columns = &mut right.tables.get_mut("orders").unwrap().spec.columns;
    columns[1].identity = Some(Identity::counting(IdentityGeneration::Always));
    columns[0].data_type = "bigint".to_owned();
    columns[2].data_type = "varchar(20)".to_owned();

    let script = script_of(&left, &right);

    assert_eq!(
        statements(&script)[2..],
        [
            "ALTER TABLE staging.orders ALTER COLUMN legacy_code TYPE varchar(20);",
            "COMMIT;",
        ]
    );
    let reasons: Vec<&str> = script
        .not_written
        .iter()
        .map(|not_written| not_written.reason.as_str())
        .collect();
    assert_eq!(
        reasons,
        [
            "column orders.id changes from bigserial to bigint, and a serial type can't be \
             changed in place; change it by hand.",
            "column orders.customer_id becomes an identity column, which can't be changed in \
             place; change it by hand.",
        ]
    );
    assert!(script.sql.contains(
        "-- Not written: column orders.customer_id becomes an identity column, which can't be \
         changed in place; change it by hand.\n"
    ));
}

#[test]
fn a_primary_key_takes_the_rights_name() {
    let left = shop("staging");
    let mut right = shop("public");
    right.tables.get_mut("customers").unwrap().primary_key_name =
        Some("customers_id_pk".to_owned());
    let mut tags = table("tags", vec![key_column("id", "integer")]);
    tags.primary_key_name = Some("tags_pk".to_owned());
    right.tables.insert("tags".to_owned(), tags);

    let script = script_of(&left, &right);

    let statements = statements(&script);
    assert!(statements.contains(
        &"ALTER TABLE staging.customers RENAME CONSTRAINT customers_pkey TO customers_id_pk;"
    ));
    assert!(
        statements.contains(&"ALTER TABLE staging.tags RENAME CONSTRAINT tags_pkey TO tags_pk;")
    );
}

#[test]
fn an_index_on_a_dropped_table_goes_with_it() {
    let mut left = shop("staging");
    left.tables.insert(
        "archive".to_owned(),
        table("archive", vec![key_column("id", "integer")]),
    );
    left.indexes
        .insert("archive_id_idx".to_owned(), index("archive", "id"));
    left.indexes
        .insert("orders_code_idx".to_owned(), index("orders", "legacy_code"));
    let mut right = shop("public");
    right.indexes.insert(
        "orders_code_idx".to_owned(),
        Index {
            unique: true,
            ..index("orders", "legacy_code")
        },
    );

    let script = script_of(&left, &right);

    assert_eq!(
        statements(&script)[2..],
        [
            "DROP INDEX staging.orders_code_idx;",
            "DROP TABLE staging.archive;",
            "CREATE UNIQUE INDEX orders_code_idx ON staging.orders USING btree (legacy_code);",
            "COMMIT;",
        ]
    );
}

#[test]
fn a_view_that_reads_another_schema_keeps_reading_it() {
    let left = shop("staging");
    let mut right = shop("public");
    right.views.insert(
        "events".to_owned(),
        view(ViewKind::View, " SELECT id\n   FROM audit.events", &[]),
    );

    let script = script_of(&left, &right);

    assert!(
        script
            .sql
            .contains("CREATE VIEW staging.events AS\nSELECT id\n   FROM audit.events;\n")
    );
}
