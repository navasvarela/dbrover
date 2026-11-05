//! Schema compare (W13) through the engine: reading schemas on the PostgreSQL test server.

use dbrover_schema::Schema;

use super::*;

fn describe_schema(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    schema: &str,
) -> Result<Schema, String> {
    engine.send(Command::DescribeSchema {
        data_source,
        schema: schema.to_owned(),
    });
    loop {
        match engine.next_event() {
            Event::SchemaDescribed {
                schema: described,
                result,
                ..
            } if described == schema => return result,
            Event::SessionChanged { .. } => {}
            other => panic!("expected SchemaDescribed, got {other:?}"),
        }
    }
}

#[test]
fn reads_a_postgres_schema_and_says_when_there_is_none() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
    save(&mut engine, &postgres, Some(server.password));
    let schema = unique("dbrover_described", &postgres);
    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("CREATE SCHEMA {schema}"),
            format!("CREATE TABLE {schema}.orders (id serial PRIMARY KEY, note text)"),
            format!("CREATE INDEX orders_note_idx ON {schema}.orders (note)"),
        ],
    )
    .unwrap();

    let described = describe_schema(&mut engine, postgres.id, &schema).unwrap();

    assert_eq!(described.dialect, Dialect::Postgres);
    assert_eq!(described.tables.keys().collect::<Vec<_>>(), ["orders"]);
    assert_eq!(
        described.tables["orders"].spec.columns[0].data_type,
        "serial"
    );
    assert_eq!(
        described.indexes.keys().collect::<Vec<_>>(),
        ["orders_note_idx"]
    );
    let missing = format!("{schema}_missing");
    assert_eq!(
        describe_schema(&mut engine, postgres.id, &missing),
        Err(format!("There is no schema \u{201c}{missing}\u{201d}."))
    );
    apply(
        &mut engine,
        postgres.id,
        vec![format!("DROP SCHEMA {schema} CASCADE")],
    )
    .unwrap();
}

#[test]
fn an_engine_that_cant_read_its_schemas_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let sqlite = sqlite_data_source(sqlite_database(dir.path()));
    save(&mut engine, &sqlite, None);

    assert_eq!(
        describe_schema(&mut engine, sqlite.id, "main"),
        Err("This database's schemas can't be compared yet.".to_owned())
    );
}
