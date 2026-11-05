//! Schema compare (W13) through the engine, on the PostgreSQL test server: reading schemas, and
//! the script that makes one like another.

use common::MemorySecrets;
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels, read_schema};
use dbrover_schema::{Change, Schema, Sides, compare, read_snapshot, script, write_snapshot};
use dbrover_store::{Secret, SecretStore, TrustedCommands};

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

/// Runs `script` on a console of its own, statement by statement as the console splits it,
/// and waits for the run to end; panics on a statement that fails.
fn run_script(engine: &mut TestEngine, data_source: DataSourceId, script: &str) {
    static RUNS: AtomicU64 = AtomicU64::new(1_000_000);
    let run = RunId(RUNS.fetch_add(1, Ordering::Relaxed));
    let console = ConsoleId(run.0);
    let statements = split_statements(script, Dialect::Postgres)
        .into_iter()
        .map(|range| script[range].to_owned())
        .collect();
    engine.send(Command::Execute {
        console,
        data_source,
        run,
        statements,
        parameters: ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
        cost: CostApproval::Ask,
    });
    loop {
        match engine.next_event() {
            Event::StatementFinished {
                run: finished,
                result: Err(failure),
                ..
            } if finished == run => panic!("the script failed: {failure:?}\n{script}"),
            Event::RunFinished { run: finished, .. } if finished == run => break,
            _ => {}
        }
    }
    engine.send(Command::CloseConsole(console));
    loop {
        if let Event::ConsoleSessionChanged {
            console: closed,
            state: SessionState::Disconnected,
        } = engine.next_event()
            && closed == console
        {
            return;
        }
    }
}

/// W13's exit: two schemas that differ by a column and an index list both differences, and
/// the script, run on the left, makes the comparison come back empty.
#[test]
fn the_script_makes_two_schemas_the_same() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
    save(&mut engine, &postgres, Some(server.password));
    let left = unique("dbrover_compare_left", &postgres);
    let right = unique("dbrover_compare_right", &postgres);
    let build = |schema: &str, extra: &[String]| {
        let mut statements = vec![
            format!("CREATE SCHEMA {schema}"),
            format!("CREATE TABLE {schema}.customers (id serial PRIMARY KEY, email text NOT NULL)"),
            format!(
                "CREATE TABLE {schema}.orders (id bigserial PRIMARY KEY, \
                 customer_id integer NOT NULL REFERENCES {schema}.customers, \
                 total numeric(12,2) NOT NULL CHECK (total >= 0))"
            ),
            format!(
                "CREATE VIEW {schema}.big_orders AS SELECT id, total FROM {schema}.orders \
                 WHERE total > 100"
            ),
        ];
        statements.extend_from_slice(extra);
        statements
    };
    let right_extra = [
        format!("ALTER TABLE {right}.orders ADD COLUMN note text"),
        format!("CREATE INDEX orders_customer_id_idx ON {right}.orders (customer_id)"),
    ];
    apply(&mut engine, postgres.id, build(&left, &[])).unwrap();
    apply(&mut engine, postgres.id, build(&right, &right_extra)).unwrap();

    let before = compare(
        &describe_schema(&mut engine, postgres.id, &left).unwrap(),
        &describe_schema(&mut engine, postgres.id, &right).unwrap(),
    )
    .unwrap();

    let tables: Vec<(&str, String)> = before
        .tables
        .iter()
        .map(|table| match &table.change {
            Change::Differs(differences) => (table.name.as_str(), differences.summary()),
            other => panic!("expected a changed table, got {other:?}"),
        })
        .collect();
    assert_eq!(tables, [("orders", "1 column added".to_owned())]);
    let indexes: Vec<(&str, &str)> = before
        .indexes
        .iter()
        .map(|index| (index.name.as_str(), index.change.mark()))
        .collect();
    assert_eq!(indexes, [("orders_customer_id_idx", "+")]);
    assert!(before.views.is_empty());

    let left_schema = describe_schema(&mut engine, postgres.id, &left).unwrap();
    let right_schema = describe_schema(&mut engine, postgres.id, &right).unwrap();
    let sides = Sides {
        left: format!("schema {left} on postgres"),
        right: format!("schema {right} on postgres"),
    };
    let script = script(&left_schema, &right_schema, &sides)
        .unwrap()
        .expect("a script");
    assert!(script.not_written.is_empty(), "{script:?}");
    run_script(&mut engine, postgres.id, &script.sql);

    let after = compare(
        &describe_schema(&mut engine, postgres.id, &left).unwrap(),
        &describe_schema(&mut engine, postgres.id, &right).unwrap(),
    )
    .unwrap();
    assert!(after.is_empty(), "{after:#?}\n{}", script.sql);

    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("DROP SCHEMA {left} CASCADE"),
            format!("DROP SCHEMA {right} CASCADE"),
        ],
    )
    .unwrap();
}

/// Every phase of the script on a real server: a schema written from nothing, and back.
#[test]
fn the_script_builds_a_schema_from_nothing_and_empties_it_again() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
    save(&mut engine, &postgres, Some(server.password));
    let empty = unique("dbrover_compare_empty", &postgres);
    let full = unique("dbrover_compare_full", &postgres);
    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("CREATE SCHEMA {empty}"),
            format!("CREATE SCHEMA {full}"),
            format!(
                "CREATE TABLE {full}.customers (id serial PRIMARY KEY, \
                 email text NOT NULL UNIQUE, created_at timestamptz NOT NULL DEFAULT now())"
            ),
            format!(
                "CREATE TABLE {full}.orders (\
                 id bigint GENERATED ALWAYS AS IDENTITY (START WITH 100) PRIMARY KEY, \
                 customer_id integer NOT NULL REFERENCES {full}.customers ON DELETE CASCADE, \
                 parent_id bigint REFERENCES {full}.orders, \
                 total numeric(12,2) NOT NULL CHECK (total >= 0), note text)"
            ),
            format!(
                "CREATE INDEX orders_lookup_idx ON {full}.orders \
                 (customer_id DESC NULLS LAST, lower(note)) INCLUDE (total) WHERE total > 0"
            ),
            format!(
                "CREATE VIEW {full}.big_orders AS SELECT o.id, c.email \
                 FROM {full}.orders o JOIN {full}.customers c ON c.id = o.customer_id \
                 WHERE o.total > 100"
            ),
            format!("CREATE MATERIALIZED VIEW {full}.big_order_emails AS SELECT * FROM {full}.big_orders"),
            format!("CREATE UNIQUE INDEX big_order_emails_id_idx ON {full}.big_order_emails (id)"),
        ],
    )
    .unwrap();

    let full_schema = describe_schema(&mut engine, postgres.id, &full).unwrap();
    let nothing = Schema::empty(Dialect::Postgres, &full);
    for target in [&full_schema, &nothing] {
        let source = describe_schema(&mut engine, postgres.id, &empty).unwrap();
        let sides = Sides {
            left: format!("schema {empty}"),
            right: format!("schema {}", target.name),
        };
        let script = script(&source, target, &sides).unwrap().expect("a script");
        assert!(script.not_written.is_empty(), "{script:?}");
        run_script(&mut engine, postgres.id, &script.sql);
        let after = compare(
            &describe_schema(&mut engine, postgres.id, &empty).unwrap(),
            target,
        )
        .unwrap();
        assert!(after.is_empty(), "{after:#?}\n{}", script.sql);
    }

    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("DROP SCHEMA {empty} CASCADE"),
            format!("DROP SCHEMA {full} CASCADE"),
        ],
    )
    .unwrap();
}

/// The first half of VC3's exit: a snapshot of a schema taken twice is the same file, whether
/// taken headless as `dbrover schema snapshot` does or saved by the app's command; after a
/// column and an index are added, the live schema compared with the first snapshot lists both.
#[test]
fn a_snapshot_taken_twice_is_the_same_file() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let password = server.password.clone();
    let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
    save(&mut engine, &postgres, Some(server.password));
    let schema = unique("dbrover_snapshot", &postgres);
    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("CREATE SCHEMA {schema}"),
            format!("CREATE TABLE {schema}.orders (id serial PRIMARY KEY, total numeric(12,2))"),
            format!("CREATE VIEW {schema}.big_orders AS SELECT id FROM {schema}.orders WHERE total > 100"),
        ],
    )
    .unwrap();

    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets
        .set(postgres.id, Secret::Database, &password)
        .unwrap();
    let tunnels = Tunnels::new(dir.path().join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let headless = || {
        let read = runtime
            .block_on(read_schema(&postgres, &schema, &access))
            .unwrap();
        write_snapshot(&read)
    };
    let first = headless();
    let second = headless();
    assert_eq!(first, second);
    assert!(first.contains("[tables.orders]\n"), "{first}");

    let path = dir.path().join("orders.schema.toml");
    engine.send(Command::SaveSchemaSnapshot {
        data_source: postgres.id,
        schema: schema.clone(),
        path: path.clone(),
    });
    loop {
        match engine.next_event() {
            Event::SchemaSnapshotSaved { result, .. } => break result.unwrap(),
            Event::SessionChanged { .. } => {}
            other => panic!("expected SchemaSnapshotSaved, got {other:?}"),
        }
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

    apply(
        &mut engine,
        postgres.id,
        vec![
            format!("ALTER TABLE {schema}.orders ADD COLUMN note text"),
            format!("CREATE INDEX orders_total_idx ON {schema}.orders (total)"),
        ],
    )
    .unwrap();
    let live = describe_schema(&mut engine, postgres.id, &schema).unwrap();
    let comparison = compare(&read_snapshot(&first).unwrap(), &live).unwrap();
    let tables: Vec<String> = comparison
        .tables
        .iter()
        .map(|table| match &table.change {
            Change::Differs(differences) => format!("{}: {}", table.name, differences.summary()),
            other => panic!("expected a changed table, got {other:?}"),
        })
        .collect();
    assert_eq!(tables, ["orders: 1 column added"]);
    let indexes: Vec<(&str, &str)> = comparison
        .indexes
        .iter()
        .map(|index| (index.name.as_str(), index.change.mark()))
        .collect();
    assert_eq!(indexes, [("orders_total_idx", "+")]);

    apply(
        &mut engine,
        postgres.id,
        vec![format!("DROP SCHEMA {schema} CASCADE")],
    )
    .unwrap();
}
