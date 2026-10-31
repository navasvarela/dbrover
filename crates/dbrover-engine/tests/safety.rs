//! Consoles of read-only data sources: changes refused before they are sent, and statements
//! run in the database's read-only mode.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{
    TestEngine, clickhouse_test_server, mongo_test_server, postgres_test_server,
    redis_test_servers, search_test_servers, sqlite_data_source, sqlite_database,
};
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, SafetyLevel};
use dbrover_engine::{
    Command, ConsoleId, EditFailure, Event, PasswordUpdate, RunId, RunOrigin, RunOutcome,
    RunPurpose, TransactionMode,
};

const SETUP_CONSOLE: ConsoleId = ConsoleId(1);
const READ_ONLY_CONSOLE: ConsoleId = ConsoleId(2);

fn save(engine: &mut TestEngine, data_source: &DataSource, password: PasswordUpdate) {
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
}

fn read_only(data_source: &DataSource) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "production".to_owned(),
        safety: SafetyLevel::ReadOnly,
        ..data_source.clone()
    }
}

/// The next event about a run, skipping connection state changes. A transaction change
/// fails the test: a read-only console never shows one.
fn next_run_event(engine: &mut TestEngine) -> Event {
    loop {
        match engine.next_event() {
            Event::ConsoleSessionChanged { .. } => {}
            event @ Event::TransactionChanged { .. } => {
                panic!("a read-only console reported a transaction: {event:?}")
            }
            event => return event,
        }
    }
}

/// Runs one statement and returns its failure message, or `None` if it succeeded.
fn run(
    engine: &mut TestEngine,
    console: ConsoleId,
    data_source: DataSourceId,
    run: u64,
    sql: &str,
) -> Option<String> {
    run_for(engine, (console, data_source), run, sql, RunPurpose::Run)
}

/// Runs one statement for `purpose` on the console and data source, and returns its failure
/// message, or `None` if it succeeded.
fn run_for(
    engine: &mut TestEngine,
    (console, data_source): (ConsoleId, DataSourceId),
    run: u64,
    sql: &str,
    purpose: RunPurpose,
) -> Option<String> {
    engine.send(Command::Execute {
        console,
        data_source,
        run: RunId(run),
        statements: vec![sql.to_owned()],
        parameters: dbrover_sql::ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose,
        origin: RunOrigin::Console,
    });
    let mut failure = None;
    loop {
        match next_run_event(engine) {
            Event::StatementFinished {
                result: Err(error), ..
            } => failure = Some(error.message),
            Event::RunFinished {
                run: RunId(r),
                outcome,
                ..
            } if r == run => {
                assert_eq!(
                    outcome == RunOutcome::Completed,
                    failure.is_none(),
                    "{sql}: {failure:?}"
                );
                return failure;
            }
            _ => {}
        }
    }
}

fn refused(kind: &str) -> String {
    format!("Refused: this statement {kind}, and \u{201c}production\u{201d} is read-only.")
}

#[test]
fn a_read_only_sqlite_data_source_refuses_changes_and_edits() {
    let dir = tempfile::tempdir().unwrap();
    let database = sqlite_database(dir.path());
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let production = read_only(&sqlite_data_source(database.clone()));
    save(&mut engine, &production, PasswordUpdate::Keep);
    let id = production.id;

    let cases = [
        (
            "INSERT INTO t (id) VALUES (1)",
            Some(refused("changes data")),
        ),
        (
            "CREATE TABLE u (id INTEGER)",
            Some(refused("changes the schema")),
        ),
        ("BEGIN", Some(refused("controls the transaction"))),
        ("SELECT count(*) FROM t", None),
    ];
    for (number, (sql, expected)) in cases.into_iter().enumerate() {
        let failure = run(&mut engine, READ_ONLY_CONSOLE, id, number as u64 + 1, sql);
        assert_eq!(failure, expected, "{sql}");
    }

    engine.send(Command::ApplyEdits {
        console: READ_ONLY_CONSOLE,
        statements: vec![dbrover_engine::RowEdit {
            sql: "UPDATE t SET id = 2 WHERE id = 1".to_owned(),
            change: None,
        }],
    });
    assert_eq!(
        next_run_event(&mut engine),
        Event::EditsApplied {
            console: READ_ONLY_CONSOLE,
            result: Err(EditFailure {
                statement: None,
                message: "\u{201c}production\u{201d} is read-only, so its rows can't be edited."
                    .to_owned(),
            }),
        }
    );
    let tables: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tables, 1, "the database was changed");
}

/// W11's exit on ClickHouse: a read-only data source refuses an `INSERT` before sending it, and
/// ClickHouse itself refuses what the check reads as a `SELECT` but changes the session's
/// `readonly` setting. ClickHouse's functions don't write, so this is the write that hides from
/// the check there; the driver's tests send an `INSERT` past it. A query's own settings still
/// run. Needs `DBROVER_TEST_CLICKHOUSE_URL`; skipped without it.
#[test]
fn a_read_only_clickhouse_data_source_is_refused_by_clickhouse_itself() {
    let Some((config, password)) = clickhouse_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let events = DataSource {
        id: DataSourceId::generate(),
        name: "events".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::ClickHouse(config),
    };
    let production = read_only(&events);
    save(&mut engine, &production, PasswordUpdate::Set(password));
    let cases = [
        (
            "INSERT INTO dbrover_events VALUES (1)",
            Some(refused("changes data")),
        ),
        (
            "SELECT number FROM numbers(1) SETTINGS max_threads = 1",
            None,
        ),
    ];
    for (number, (sql, expected)) in cases.into_iter().enumerate() {
        let failure = run(
            &mut engine,
            READ_ONLY_CONSOLE,
            production.id,
            number as u64 + 1,
            sql,
        );
        assert_eq!(failure, expected, "{sql}");
    }
    let lifted = run(
        &mut engine,
        READ_ONLY_CONSOLE,
        production.id,
        9,
        "SELECT number FROM numbers(1) SETTINGS readonly = 0",
    )
    .expect("ClickHouse refused to lift readonly");
    assert!(
        lifted.contains("Cannot modify 'readonly' setting in readonly mode"),
        "{lifted}"
    );
}

/// N1's exit: a read-only Redis data source refuses `SET` before sending it, and still reads.
/// Needs `DBROVER_TEST_REDIS_URL` or `DBROVER_TEST_VALKEY_URL`; skipped without them.
#[test]
fn a_read_only_redis_data_source_refuses_set() {
    for (config, password) in redis_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let cache = DataSource {
            id: DataSourceId::generate(),
            name: "cache".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Redis(config),
        };
        let production = read_only(&cache);
        save(&mut engine, &production, PasswordUpdate::Set(password));
        let key = format!("dbrover:safety:{}", std::process::id());
        let cases = [
            (format!("SET {key} 1"), Some(refused("changes data"))),
            (
                "FLUSHALL".to_owned(),
                Some(refused("administers the database")),
            ),
            (format!("GET {key}"), None),
        ];
        for (number, (command, expected)) in cases.into_iter().enumerate() {
            let failure = run(
                &mut engine,
                READ_ONLY_CONSOLE,
                production.id,
                number as u64 + 1,
                &command,
            );
            assert_eq!(failure, expected, "{command}");
        }
        // A command the table doesn't know may do anything, so it is refused too.
        let unknown = run(
            &mut engine,
            READ_ONLY_CONSOLE,
            production.id,
            9,
            "NOSUCH key",
        );
        assert!(
            unknown.is_some_and(|message| message.contains("unknown command `NOSUCH`")),
            "an unknown command was sent"
        );
    }
}

/// The exit test of W2: an `INSERT`, a `CREATE TABLE`, and a write hidden in a function.
#[test]
fn a_read_only_postgres_data_source_refuses_writes_even_inside_a_function() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let normal = DataSource {
        id: DataSourceId::generate(),
        name: "setup".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config.clone()),
    };
    let production = read_only(&normal);
    for data_source in [&normal, &production] {
        save(
            &mut engine,
            data_source,
            PasswordUpdate::Set(server.password.clone()),
        );
    }
    // A table and a function of this test's own, so tests sharing the server don't collide.
    let table = format!("w2_{}", normal.id.to_string().replace('-', ""));
    let setup = [
        format!("CREATE TABLE {table} (n integer)"),
        format!(
            "CREATE FUNCTION {table}_add() RETURNS integer LANGUAGE sql \
             AS $$ INSERT INTO {table} VALUES (1) RETURNING n $$"
        ),
    ];
    for (number, sql) in setup.iter().enumerate() {
        assert_eq!(
            run(
                &mut engine,
                SETUP_CONSOLE,
                normal.id,
                number as u64 + 1,
                sql
            ),
            None
        );
    }

    let insert = run(
        &mut engine,
        READ_ONLY_CONSOLE,
        production.id,
        10,
        &format!("INSERT INTO {table} VALUES (2)"),
    );
    let create = run(
        &mut engine,
        READ_ONLY_CONSOLE,
        production.id,
        11,
        "CREATE TABLE w2_never (n integer)",
    );
    let hidden = run(
        &mut engine,
        READ_ONLY_CONSOLE,
        production.id,
        12,
        &format!("SELECT {table}_add()"),
    );
    let read = run(
        &mut engine,
        READ_ONLY_CONSOLE,
        production.id,
        13,
        &format!("SELECT count(*) FROM {table}"),
    );
    let pages = page_through(&mut engine, production.id, 14);
    // Explain analyse runs the statement, so it is checked and runs read-only as a run does.
    let analyse = |engine: &mut TestEngine, run: u64, sql: String| {
        run_for(
            engine,
            (READ_ONLY_CONSOLE, production.id),
            run,
            &format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"),
            RunPurpose::ExplainAnalyse,
        )
    };
    let analysed_read = analyse(&mut engine, 15, format!("SELECT count(*) FROM {table}"));
    let analysed_delete = analyse(&mut engine, 16, format!("DELETE FROM {table}"));
    let analysed_hidden = analyse(&mut engine, 17, format!("SELECT {table}_add()"));

    for (number, sql) in [
        format!("DROP FUNCTION {table}_add()"),
        format!("DROP TABLE {table}"),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            run(
                &mut engine,
                SETUP_CONSOLE,
                normal.id,
                number as u64 + 20,
                sql
            ),
            None
        );
    }
    assert_eq!(insert, Some(refused("changes data")));
    assert_eq!(create, Some(refused("changes the schema")));
    let hidden = hidden.expect("the write inside the function was refused");
    assert!(hidden.contains("read-only transaction"), "{hidden}");
    assert_eq!(read, None);
    assert_eq!(pages, vec![500, 500, 200]);
    assert_eq!(analysed_read, None);
    assert_eq!(analysed_delete, Some(refused("changes data")));
    let analysed_hidden = analysed_hidden.expect("the analysed write was refused");
    assert!(
        analysed_hidden.contains("read-only transaction"),
        "{analysed_hidden}"
    );
}

/// Reads 1,200 rows a page at a time on the read-only console, which keeps its read-only
/// transaction open while rows are left, and returns the size of each page.
fn page_through(engine: &mut TestEngine, data_source: DataSourceId, run: u64) -> Vec<usize> {
    engine.send(Command::Execute {
        console: READ_ONLY_CONSOLE,
        data_source,
        run: RunId(run),
        statements: vec!["SELECT g FROM generate_series(1, 1200) AS g".to_owned()],
        parameters: dbrover_sql::ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
    });
    let mut pages = Vec::new();
    loop {
        match next_run_event(engine) {
            Event::RowsFetched { rows, more, .. } => {
                pages.push(rows.len());
                if !more {
                    break;
                }
                engine.send(Command::FetchMore {
                    console: READ_ONLY_CONSOLE,
                    run: RunId(run),
                });
            }
            Event::StatementFinished {
                result: Err(error), ..
            } => {
                panic!("paging failed: {}", error.message)
            }
            _ => {}
        }
    }
    pages
}

/// N2's exit: a read-only MongoDB data source refuses `deleteMany` before sending it, and still
/// reads. Needs `DBROVER_TEST_MONGODB_URL`; skipped without it.
#[test]
fn a_read_only_mongodb_data_source_refuses_delete_many() {
    let Some((config, password)) = mongo_test_server("dbrover_engine_safety") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let shop = DataSource {
        id: DataSourceId::generate(),
        name: "shop".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Mongo(config),
    };
    let production = read_only(&shop);
    save(&mut engine, &production, PasswordUpdate::Set(password));
    let cases = [
        ("db.orders.deleteMany({})", Some(refused("changes data"))),
        ("db.orders.drop()", Some(refused("changes the schema"))),
        (
            "db.runCommand({createUser: 'x', pwd: 'y', roles: []})",
            Some(refused("administers the database")),
        ),
        ("db.orders.find({status: 'open'})", None),
    ];
    for (number, (command, expected)) in cases.into_iter().enumerate() {
        let failure = run(
            &mut engine,
            READ_ONLY_CONSOLE,
            production.id,
            number as u64 + 1,
            command,
        );
        assert_eq!(failure, expected, "{command}");
    }
}

/// N3's exit: a read-only Elasticsearch or OpenSearch data source refuses a
/// `_delete_by_query` before sending it, and still searches. Needs
/// `DBROVER_TEST_ELASTICSEARCH_URL` or `DBROVER_TEST_OPENSEARCH_URL`; skipped without.
#[test]
fn a_read_only_search_data_source_refuses_delete_by_query() {
    for (name, config, password) in search_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let search = DataSource {
            id: DataSourceId::generate(),
            name: "search".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Elasticsearch(config),
        };
        let production = read_only(&search);
        save(&mut engine, &production, password);
        let cases = [
            (
                "POST /dbrover_engine_safety/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
                Some(refused("changes data")),
            ),
            (
                "DELETE /dbrover_engine_safety",
                Some(refused("changes the schema")),
            ),
            (
                "PUT /_cluster/settings\n{ \"transient\": {} }",
                Some(refused("administers the database")),
            ),
            (
                "GET /_search\n{ \"query\": { \"match_all\": {} }, \"size\": 1 }",
                None,
            ),
        ];
        for (number, (request, expected)) in cases.into_iter().enumerate() {
            let failure = run(
                &mut engine,
                READ_ONLY_CONSOLE,
                production.id,
                number as u64 + 1,
                request,
            );
            assert_eq!(failure, expected, "{name}: {request}");
        }
    }
}
