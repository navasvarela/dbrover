//! Searching a database's data (W16): a plan, then a run on a session of the search's own,
//! table by table, recorded once in the history; the careful data source's question; a table
//! that runs out of time; Cancel. The PostgreSQL tests are skipped unless
//! `DBROVER_TEST_POSTGRES_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, ParsedUrl, TestEngine, postgres_test_server};
use dbrover_core::{
    CaseMatch, CatalogPath, Connection, ConnectionConfig, DataSearch, DataSource, DataSourceId,
    MatchKind, NodeKind, PostgresConfig, Relations, Route, SafetyLevel, SearchScope, SqliteConfig,
    TablesToSearch, TimeLimit,
};
use dbrover_engine::{
    Command, Event, PasswordUpdate, RunId, SearchEnd, SearchLimits, SearchPlan, SearchQuestion,
    SearchRequest, TableOutcome, TableSearched,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistoryQuery, HistorySource, HistorySourceKind};

const FIXTURE: &str = "
    CREATE TABLE customers (id integer PRIMARY KEY, name text, email text);
    INSERT INTO customers VALUES (1, 'Kate O''Brien', 'kate@example.com'),
                                 (2, 'Liam Hale', 'liam@example.org');
    CREATE TABLE notes (id integer PRIMARY KEY, body text);
    INSERT INTO notes VALUES (88, 'called O''Brien about the refund');
    CREATE TABLE amounts (id integer PRIMARY KEY, total integer);
    INSERT INTO amounts VALUES (1, 300);
";

fn sqlite_source(path: &Path, safety: SafetyLevel) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "shop".to_owned(),
        color: None,
        safety,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: path.to_owned(),
            read_only: false,
        }),
    }
}

/// An engine with `data_source` saved, its password set where it has one.
fn start_engine(
    data_source: &DataSource,
    password: Option<&dbrover_core::Password>,
) -> (TestEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(
        &dir.path().join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: password.map_or(PasswordUpdate::Keep, |password| {
            PasswordUpdate::Set(password.clone())
        }),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    (engine, dir)
}

fn until<T>(engine: &mut TestEngine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(engine.next_event()) {
            return found;
        }
    }
}

fn request(text: &str, scope: CatalogPath, per_table: Option<TimeLimit>) -> SearchRequest {
    SearchRequest {
        search: DataSearch::new(text, MatchKind::Contains, CaseMatch::Ignore).unwrap(),
        tables: TablesToSearch {
            scope: SearchScope::Under(scope),
            views: Relations::TablesOnly,
        },
        limits: SearchLimits {
            per_table,
            ..SearchLimits::default()
        },
    }
}

fn plan(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    run: u64,
    request: SearchRequest,
) -> SearchPlan {
    engine.send(Command::PlanDataSearch {
        data_source,
        run: RunId(run),
        request,
    });
    until(engine, |event| match event {
        Event::DataSearchPlanned {
            run: planned,
            result,
            ..
        } if planned == RunId(run) => Some(result.unwrap()),
        _ => None,
    })
}

/// Runs the plan, and returns each table's outcome and how the search ended.
fn run(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    run: u64,
) -> (Vec<TableSearched>, SearchEnd) {
    engine.send(Command::RunDataSearch {
        data_source,
        run: RunId(run),
    });
    let mut searched = Vec::new();
    let end = until(engine, |event| match event {
        Event::DataSearchTableSearched {
            searched: table, ..
        } => {
            searched.push(table);
            None
        }
        Event::DataSearchEnded {
            run: ended, end, ..
        } if ended == RunId(run) => Some(end),
        _ => None,
    });
    (searched, end)
}

fn table_name(plan: &SearchPlan, searched: &TableSearched) -> String {
    plan.tables[searched.index].table.name.name.clone()
}

fn searches(engine: &mut TestEngine, data_source: DataSourceId) -> Vec<HistoryEntry> {
    engine.send(Command::SearchHistory(HistoryQuery {
        kind: Some(HistorySourceKind::Search),
        ..HistoryQuery::all(data_source)
    }));
    until(engine, |event| match event {
        Event::HistoryFound { result, .. } => Some(result.unwrap()),
        _ => None,
    })
}

fn sqlite_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("shop.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(FIXTURE).unwrap();
    path
}

#[test]
fn plans_runs_and_records_a_search_once() {
    let files = tempfile::tempdir().unwrap();
    let shop = sqlite_source(&sqlite_file(files.path()), SafetyLevel::Normal);
    let (mut engine, _dir) = start_engine(&shop, None);
    let id = shop.id;

    let plan = plan(
        &mut engine,
        id,
        1,
        request("o'brien", CatalogPath::root(), None),
    );
    let names: Vec<&str> = plan
        .tables
        .iter()
        .map(|table| table.table.name.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["amounts", "customers", "notes"],
        "by name, with no estimate"
    );
    assert!(
        plan.tables[0].statement.is_none(),
        "a table of numbers isn't read"
    );
    assert_eq!(plan.searched(), 2);
    assert_eq!(plan.question, None);

    let (searched, end) = run(&mut engine, id, 1);
    assert_eq!(end, SearchEnd::Finished);
    let found: Vec<(String, TableOutcome)> = searched
        .iter()
        .map(|table| (table_name(&plan, table), table.outcome.clone()))
        .collect();
    let text = |value: &str| Some(value.to_owned());
    assert_eq!(
        found,
        [
            (
                "customers".to_owned(),
                TableOutcome::Matched {
                    rows: vec![vec![
                        text("1"),
                        text("Kate O'Brien"),
                        text("kate@example.com")
                    ]],
                    more: false,
                }
            ),
            (
                "notes".to_owned(),
                TableOutcome::Matched {
                    rows: vec![vec![text("88"), text("called O'Brien about the refund")]],
                    more: false,
                }
            ),
        ]
    );

    let entries = searches(&mut engine, id);
    let [entry] = entries.as_slice() else {
        panic!("one entry per search: {entries:#?}");
    };
    assert_eq!(entry.outcome, HistoryOutcome::Ok);
    assert_eq!(entry.rows, Some(2));
    let statements: Vec<String> = plan.tables[1..]
        .iter()
        .filter_map(|table| table.statement.as_ref())
        .map(|statement| format!("{};", statement.sql))
        .collect();
    assert_eq!(entry.sql, statements.join("\n\n"));
    assert_eq!(
        entry.source,
        HistorySource::Search {
            searched: "Searched 2 of 2 tables for \u{201c}o'brien\u{201d}, contains, ignoring \
                       case: 2 matched"
                .to_owned()
        }
    );

    // Running a plan twice runs nothing the second time: each run needs its plan.
    let (searched, end) = run(&mut engine, id, 1);
    assert!(searched.is_empty());
    assert!(matches!(end, SearchEnd::Failed(_)), "{end:?}");
    engine.send(Command::CloseDataSearch(id));
    engine.shut_down();
}

#[test]
fn caps_the_rows_read_from_a_table() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("many.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE lines (id integer PRIMARY KEY, note text);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 150)
             INSERT INTO lines SELECT i, 'refund ' || i FROM n;",
        )
        .unwrap();
    drop(connection);
    let shop = sqlite_source(&path, SafetyLevel::Normal);
    let (mut engine, _dir) = start_engine(&shop, None);
    plan(
        &mut engine,
        shop.id,
        1,
        request("refund", CatalogPath::root(), None),
    );
    let (searched, _) = run(&mut engine, shop.id, 1);
    let [table] = searched.as_slice() else {
        panic!("{searched:#?}");
    };
    let TableOutcome::Matched { rows, more } = &table.outcome else {
        panic!("{table:#?}");
    };
    assert_eq!((rows.len(), *more), (100, true));
    engine.shut_down();
}

#[test]
fn a_careful_data_source_asks_before_more_than_one_table_and_a_read_only_one_is_searched() {
    let files = tempfile::tempdir().unwrap();
    let path = sqlite_file(files.path());
    let careful = sqlite_source(&path, SafetyLevel::Careful);
    let (mut engine, _dir) = start_engine(&careful, None);
    let whole = plan(
        &mut engine,
        careful.id,
        1,
        request("o'brien", CatalogPath::root(), None),
    );
    assert_eq!(whole.question, Some(SearchQuestion::Careful { tables: 2 }));
    let notes = CatalogPath::root()
        .child(NodeKind::Schema, "main")
        .child(
            NodeKind::Folder(dbrover_core::FolderKind::Tables),
            dbrover_core::FolderKind::Tables.label(),
        )
        .child(NodeKind::Table, "notes");
    let one = plan(&mut engine, careful.id, 2, request("o'brien", notes, None));
    assert_eq!(one.searched(), 1);
    assert_eq!(one.question, None, "one table is searched at once");
    engine.shut_down();

    let read_only = sqlite_source(&path, SafetyLevel::ReadOnly);
    let (mut reader, _reader_dir) = start_engine(&read_only, None);
    plan(
        &mut reader,
        read_only.id,
        1,
        request("o'brien", CatalogPath::root(), None),
    );
    let (searched, end) = run(&mut reader, read_only.id, 1);
    assert_eq!((searched.len(), end), (2, SearchEnd::Finished));
    reader.shut_down();
}

fn postgres_source(server: &ParsedUrl) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "production".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: server.config.server.clone(),
        }),
    }
}

fn connect(runtime: &tokio::runtime::Runtime, server: &ParsedUrl) -> Box<dyn Connection> {
    Box::new(
        runtime
            .block_on(dbrover_driver_postgres::connect(
                &server.config,
                Some(&server.password),
                Route::Direct,
            ))
            .unwrap(),
    )
}

fn execute(runtime: &tokio::runtime::Runtime, connection: &mut dyn Connection, sql: &str) {
    runtime.block_on(async {
        let cursor = connection.execute(sql).await.unwrap();
        cursor.finish().await.unwrap();
    });
}

/// A table locked by another session can't be read: with a time limit it times out and the
/// search goes on to the next; without one, Cancel ends the search while it waits, keeping
/// what was found.
#[test]
fn a_locked_table_times_out_and_cancel_ends_the_search() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let schema = format!(
        "w16_engine_{}",
        DataSourceId::generate().to_string().replace('-', "_")
    );
    let mut setup = connect(&runtime, &server);
    for sql in [
        format!("CREATE SCHEMA {schema}"),
        format!("CREATE TABLE {schema}.a_found (id int PRIMARY KEY, note text)"),
        format!("INSERT INTO {schema}.a_found VALUES (1, 'refund asked')"),
        format!("CREATE TABLE {schema}.b_locked (id int PRIMARY KEY, note text)"),
    ] {
        execute(&runtime, setup.as_mut(), &sql);
    }
    let mut holder = connect(&runtime, &server);
    execute(&runtime, holder.as_mut(), "BEGIN");
    execute(
        &runtime,
        holder.as_mut(),
        &format!("LOCK TABLE {schema}.b_locked IN ACCESS EXCLUSIVE MODE"),
    );

    let production = postgres_source(&server);
    let (mut engine, _dir) = start_engine(&production, Some(&server.password));
    let id = production.id;
    let scope = CatalogPath::root()
        .child(NodeKind::Database, server.config.server.database.clone())
        .child(NodeKind::Schema, schema.clone());

    let one_second = TimeLimit::from_seconds(1);
    let limited = plan(
        &mut engine,
        id,
        1,
        request("refund", scope.clone(), one_second),
    );
    let (searched, end) = run(&mut engine, id, 1);
    assert_eq!(end, SearchEnd::Finished);
    let outcomes: Vec<(String, &TableOutcome)> = searched
        .iter()
        .map(|table| (table_name(&limited, table), &table.outcome))
        .collect();
    assert!(
        matches!(
            outcomes.as_slice(),
            [
                (found, TableOutcome::Matched { .. }),
                (locked, TableOutcome::TimedOut(_)),
            ] if found == "a_found" && locked == "b_locked"
        ),
        "{outcomes:#?}"
    );

    plan(&mut engine, id, 2, request("refund", scope, None));
    engine.send(Command::RunDataSearch {
        data_source: id,
        run: RunId(2),
    });
    until(&mut engine, |event| match event {
        Event::DataSearchTableStarted { index: 1, .. } => Some(()),
        _ => None,
    });
    engine.send(Command::CancelDataSearch {
        data_source: id,
        run: RunId(2),
    });
    let end = until(&mut engine, |event| match event {
        Event::DataSearchEnded {
            run: RunId(2), end, ..
        } => Some(end),
        _ => None,
    });
    assert_eq!(end, SearchEnd::Cancelled);

    let outcomes: Vec<HistoryOutcome> = searches(&mut engine, id)
        .iter()
        .map(|entry| entry.outcome)
        .collect();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    assert!(
        outcomes.contains(&HistoryOutcome::Cancelled),
        "{outcomes:?}"
    );
    assert!(outcomes.contains(&HistoryOutcome::Ok), "{outcomes:?}");

    engine.send(Command::CloseDataSearch(id));
    engine.shut_down();
    execute(&runtime, holder.as_mut(), "ROLLBACK");
    execute(
        &runtime,
        setup.as_mut(),
        &format!("DROP SCHEMA {schema} CASCADE"),
    );
}
