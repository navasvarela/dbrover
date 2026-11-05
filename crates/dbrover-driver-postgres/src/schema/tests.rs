use dbrover_core::{Connection, Route};
use dbrover_schema::{Schema, compare};
use dbrover_sql::Dialect;

use super::*;
use crate::connect;
use crate::tests::test_server;

#[test]
fn reads_an_index_definitions_lists() {
    let cases = [
        (
            "CREATE INDEX o_idx ON s1.orders USING btree (customer_id DESC NULLS LAST, \
             lower(note) text_pattern_ops) INCLUDE (total) WHERE (total > (0)::numeric)",
            "btree",
            vec![
                "customer_id DESC NULLS LAST",
                "lower(note) text_pattern_ops",
            ],
            vec!["total"],
        ),
        (
            "CREATE UNIQUE INDEX \"a(b\" ON public.t USING gin (\"x,y\", (data ->> ','))",
            "gin",
            vec!["\"x,y\"", "(data ->> ',')"],
            vec![],
        ),
        (
            "CREATE INDEX i ON public.t USING btree (a) WITH (fillfactor='70')",
            "btree",
            vec!["a"],
            vec![],
        ),
    ];
    for (definition, method, key, include) in cases {
        assert_eq!(
            index_lists(definition, method),
            IndexLists {
                key: key.into_iter().map(str::to_owned).collect(),
                include: include.into_iter().map(str::to_owned).collect(),
            },
            "{definition}"
        );
    }
}

#[test]
fn a_definition_without_its_method_reads_as_empty() {
    assert_eq!(
        index_lists("CREATE INDEX i ON t (a)", "btree"),
        IndexLists::default()
    );
}

#[test]
fn serial_types_follow_the_integer_they_number() {
    assert_eq!(serial_type("integer"), Some("serial"));
    assert_eq!(serial_type("bigint"), Some("bigserial"));
    assert_eq!(serial_type("smallint"), Some("smallserial"));
    assert_eq!(serial_type("numeric"), None);
}

/// One schema's objects, built in each of the test's schemas by replacing `{s}`: a little of
/// everything that is read, and names of the schema inside definitions, which must read the
/// same from either schema.
const FIXTURE: &str = "
    DROP SCHEMA IF EXISTS {s} CASCADE;
    CREATE SCHEMA {s};
    CREATE TYPE {s}.status AS ENUM ('open', 'closed');
    CREATE TABLE {s}.customers (
        id serial PRIMARY KEY,
        email text NOT NULL UNIQUE,
        status {s}.status NOT NULL DEFAULT 'open'
    );
    CREATE TABLE {s}.orders (
        id bigint GENERATED ALWAYS AS IDENTITY (START WITH 100) PRIMARY KEY,
        customer_id integer NOT NULL REFERENCES {s}.customers ON DELETE CASCADE,
        auditor oid REFERENCES dbrover_schema_read_shared.auditors,
        total numeric(12,2) NOT NULL CHECK (total >= 0),
        note text,
        borrowed integer DEFAULT nextval('{s}.customers_id_seq')
    );
    CREATE INDEX orders_note_idx ON {s}.orders (customer_id DESC NULLS LAST, lower(note))
        INCLUDE (total) WHERE total > 0;
    CREATE VIEW {s}.big_orders AS
        SELECT o.id, c.email FROM {s}.orders o JOIN {s}.customers c ON c.id = o.customer_id
        WHERE o.total > 100;
    CREATE MATERIALIZED VIEW {s}.order_emails AS SELECT * FROM {s}.big_orders;
    CREATE UNIQUE INDEX order_emails_id_idx ON {s}.order_emails (id);
";

const SHARED: &str = "
    CREATE SCHEMA IF NOT EXISTS dbrover_schema_read_shared;
    CREATE TABLE IF NOT EXISTS dbrover_schema_read_shared.auditors (id oid PRIMARY KEY);
";

async fn build(connection: &mut crate::PostgresConnection, schema: &str) {
    connection
        .primary
        .client
        .batch_execute(&FIXTURE.replace("{s}", schema))
        .await
        .unwrap();
}

#[tokio::test]
async fn two_schemas_built_alike_compare_the_same() {
    let Some((config, password)) = test_server() else {
        return;
    };
    let mut connection = connect(&config, password.as_ref(), Route::Direct)
        .await
        .unwrap();
    connection
        .primary
        .client
        .batch_execute(SHARED)
        .await
        .unwrap();
    build(&mut connection, "dbrover_schema_read_a").await;
    build(&mut connection, "dbrover_schema_read_b").await;

    let left = connection
        .describe_schema("dbrover_schema_read_a")
        .await
        .unwrap()
        .unwrap();
    let right = connection
        .describe_schema("dbrover_schema_read_b")
        .await
        .unwrap()
        .unwrap();
    let left = Schema::from_description(left, Dialect::Postgres);
    let right = Schema::from_description(right, Dialect::Postgres);

    assert_eq!(compare(&left, &right).unwrap(), Default::default());
    assert_eq!(
        left.tables.keys().collect::<Vec<_>>(),
        ["customers", "orders"]
    );
    let customers = &left.tables["customers"].spec;
    assert_eq!(customers.columns[0].data_type, "serial");
    assert_eq!(customers.columns[0].default, None);
    assert_eq!(customers.columns[2].data_type, "status");
    assert_eq!(
        customers.columns[2].default.as_deref(),
        Some("'open'::status")
    );
    let orders = &left.tables["orders"];
    assert_eq!(orders.primary_key_name.as_deref(), Some("orders_pkey"));
    let identity = orders.spec.columns[0].identity.unwrap();
    assert_eq!(identity.start, 100);
    assert_eq!(
        orders.spec.columns[5].default.as_deref(),
        Some("nextval('customers_id_seq'::regclass)"),
        "a sequence the column doesn't own stays its default"
    );
    let targets: Vec<String> = orders
        .spec
        .foreign_keys
        .iter()
        .map(|key| key.target.to_sql(Dialect::Postgres))
        .collect();
    assert_eq!(
        targets,
        ["dbrover_schema_read_shared.auditors", "customers"]
    );

    assert_eq!(
        left.indexes.keys().collect::<Vec<_>>(),
        ["order_emails_id_idx", "orders_note_idx"],
        "an index that holds up a key is the key's"
    );
    let index = &left.indexes["orders_note_idx"];
    assert_eq!(index.key, ["customer_id DESC NULLS LAST", "lower(note)"]);
    assert_eq!(index.include, ["total"]);
    assert_eq!(index.predicate.as_deref(), Some("(total > (0)::numeric)"));

    let big_orders = &left.views["big_orders"];
    assert!(
        big_orders.query.contains("FROM orders o"),
        "{}",
        big_orders.query
    );
    assert_eq!(
        big_orders.reads.iter().collect::<Vec<_>>(),
        ["customers", "orders"]
    );
    assert_eq!(
        left.views["order_emails"].reads.iter().collect::<Vec<_>>(),
        ["big_orders"]
    );
}

#[tokio::test]
async fn a_missing_schema_reads_as_none_and_a_transaction_is_left_alone() {
    let Some((config, password)) = test_server() else {
        return;
    };
    let mut connection = connect(&config, password.as_ref(), Route::Direct)
        .await
        .unwrap();

    assert_eq!(
        connection
            .describe_schema("dbrover_schema_read_missing")
            .await
            .unwrap(),
        None
    );

    connection
        .primary
        .client
        .batch_execute("BEGIN")
        .await
        .unwrap();
    let error = connection.describe_schema("public").await.unwrap_err();
    assert!(
        error.to_string().contains("failed"),
        "expected a connection error, got {error}"
    );
    assert_eq!(
        connection.transaction_status().await.unwrap(),
        TransactionStatus::InTransaction
    );
}
