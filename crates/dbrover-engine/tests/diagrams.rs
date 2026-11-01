//! ER diagrams (ER1): describing many tables in one command on the explorer's connection,
//! each answered with its description, `None` for one that isn't a plain table, in the order
//! asked; and diagram files, saved, read again at start-up and deleted. On PostgreSQL, when `DBROVER_TEST_POSTGRES_URL` is set, a schema of 150
//! tables is described eight at a time, and how long that takes is printed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{TestEngine, postgres_test_server, sqlite_data_source};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, Identifier, Password, PostgresConfig,
    RelationName, Route, SafetyLevel,
};
use dbrover_engine::{Command, DescribedTable, Event, PasswordUpdate};
use dbrover_store::{
    DiagramFile, DiagramFolder, DiagramKey, DiagramName, PlacedTable, SavedDiagram,
};

const SHOP: &str = "
    CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        customer_id INTEGER NOT NULL REFERENCES customers (id),
        product_id INTEGER REFERENCES products (id)
    );
    CREATE VIEW big_orders AS SELECT * FROM orders WHERE id > 100;
";

/// The diagram's batch size, as the tab sends them.
const BATCH: usize = 8;

fn start(dir: &std::path::Path) -> TestEngine {
    let mut engine = TestEngine::start(&dir.join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    engine
}

fn save(engine: &mut TestEngine, data_source: &DataSource, password: Option<Password>) {
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: password.map_or(PasswordUpdate::Keep, PasswordUpdate::Set),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
}

fn relation(schema: Option<&str>, name: &str) -> RelationName {
    let quoted = |name: &str| Identifier {
        name: name.to_owned(),
        quoted: true,
    };
    RelationName {
        schema: schema.map(quoted),
        name: quoted(name),
    }
}

/// Sends one `DescribeTables` and waits for its answer, skipping connection changes.
fn describe(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    tables: Vec<RelationName>,
) -> Vec<DescribedTable> {
    engine.send(Command::DescribeTables {
        data_source,
        tables,
    });
    loop {
        match engine.next_event() {
            Event::TablesDescribed {
                data_source: answered,
                tables,
            } if answered == data_source => return tables,
            Event::SessionChanged { .. } => {}
            other => panic!("expected TablesDescribed, got {other:?}"),
        }
    }
}

#[test]
fn describes_each_table_in_order_and_none_for_what_isnt_a_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(SHOP)
        .unwrap();
    let data_source = sqlite_data_source(path);
    let mut engine = start(dir.path());
    save(&mut engine, &data_source, None);
    let asked = ["orders", "gone", "big_orders", "customers"].map(|name| relation(None, name));

    let described = describe(&mut engine, data_source.id, asked.to_vec());

    let tables: Vec<&RelationName> = described.iter().map(|d| &d.table).collect();
    assert_eq!(tables, asked.iter().collect::<Vec<_>>());
    let found: Vec<Option<&str>> = described
        .iter()
        .map(|d| d.result.as_ref().unwrap().as_ref().map(|t| t.name.as_str()))
        .collect();
    assert_eq!(found, [Some("orders"), None, None, Some("customers")]);
    let orders = described[0].result.as_ref().unwrap().as_ref().unwrap();
    let mut referenced: Vec<&str> = orders
        .foreign_keys
        .iter()
        .map(|key| key.referenced.name.as_str())
        .collect();
    referenced.sort_unstable();
    assert_eq!(referenced, ["customers", "products"]);
    let customers = described[3].result.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(customers.referenced_by.len(), 1, "orders points at it");
}

#[test]
fn saves_a_diagram_reads_it_at_the_next_start_and_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let engine = start(dir.path());
    let key = DiagramKey {
        folder: DiagramFolder::NoProject,
        name: DiagramName::try_from("shop".to_owned()).unwrap(),
    };
    let file = DiagramFile {
        data_source: DataSourceId::generate(),
        schema: Some("main".to_owned()),
        tables: vec![PlacedTable {
            schema: "main".to_owned(),
            name: "orders".to_owned(),
            x: Some(260),
            y: Some(-40),
        }],
    };
    engine.send(Command::SaveDiagram {
        key: key.clone(),
        file: file.clone(),
    });
    engine.shut_down();

    let mut restarted = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    restarted.expect(Event::Started);
    let saved = loop {
        if let Event::DiagramsLoaded(saved) = restarted.next_event() {
            break saved;
        }
    };
    assert_eq!(
        saved,
        [SavedDiagram {
            key: key.clone(),
            file
        }]
    );
    assert!(dir.path().join("diagrams/_none/shop.toml").is_file());

    restarted.send(Command::DeleteDiagram(key));
    restarted.shut_down();
    assert!(!dir.path().join("diagrams/_none/shop.toml").exists());
}

#[test]
fn describes_a_schema_of_150_tables_a_batch_at_a_time_on_postgres() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut admin = runtime
        .block_on(dbrover_driver_postgres::connect(
            &server.config,
            Some(&server.password),
            Route::Direct,
        ))
        .unwrap();
    let schema = format!(
        "dbrover_diagram_{}",
        DataSourceId::generate().to_string().replace('-', "_")
    );
    // Each table but the first points at the one before it, as a chain of keys.
    let create = format!(
        "CREATE SCHEMA {schema}; \
         DO $$ BEGIN FOR i IN 1..150 LOOP \
         EXECUTE format('CREATE TABLE {schema}.t_%s (id int PRIMARY KEY, name text, \
         previous int %s)', lpad(i::text, 3, '0'), \
         CASE WHEN i > 1 THEN format('REFERENCES {schema}.t_%s', lpad((i - 1)::text, 3, '0')) \
         ELSE '' END); \
         END LOOP; END $$"
    );
    run(&runtime, &mut admin, &create);
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "diagram".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: server.config.server.clone(),
        }),
    };
    save(&mut engine, &data_source, Some(server.password.clone()));
    let tables: Vec<RelationName> = (1..=150)
        .map(|n| relation(Some(&schema), &format!("t_{n:03}")))
        .collect();

    let started = Instant::now();
    let mut described = Vec::new();
    for batch in tables.chunks(BATCH) {
        described.extend(describe(&mut engine, data_source.id, batch.to_vec()));
    }
    println!("described 150 tables in {:?}", started.elapsed());
    run(
        &runtime,
        &mut admin,
        &format!("DROP SCHEMA {schema} CASCADE"),
    );
    let _ = runtime.block_on(Box::new(admin).close());

    assert_eq!(described.len(), 150);
    for (n, table) in described.iter().enumerate() {
        let description = table.result.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(description.columns.len(), 3, "{}", description.name);
        assert_eq!(description.foreign_keys.len(), usize::from(n > 0));
        assert_eq!(description.referenced_by.len(), usize::from(n < 149));
    }
}

fn run(runtime: &tokio::runtime::Runtime, connection: &mut dyn Connection, sql: &str) {
    runtime.block_on(async {
        connection
            .execute(sql)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
    });
}
