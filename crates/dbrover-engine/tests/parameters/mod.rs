//! `:name` parameters through the engine (W6), on every engine with a test server: bound
//! where the driver binds them, written as literals where it doesn't.

use dbrover_core::{Cell, Row};
use dbrover_sql::{Dialect, ParameterValues};

use super::grid::{TestSource, clickhouse_source, run_ok, test_sources};
use super::*;

/// A table name no other test, or another run sharing the test servers, uses at the same time.
fn scratch_table() -> String {
    format!("w6_params_{}", std::process::id())
}

fn values(entries: &[(&str, Option<&str>)]) -> ParameterValues {
    entries
        .iter()
        .map(|(name, value)| (*name, value.map(str::to_owned)))
        .collect()
}

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

/// Runs one statement with `parameters` and returns its rows, or how it failed.
fn run_with(
    engine: &mut TestEngine,
    source: &TestSource,
    run: u64,
    sql: &str,
    parameters: &ParameterValues,
) -> Result<Vec<Row>, StatementFailure> {
    engine.send(Command::Execute {
        console: CONSOLE,
        data_source: source.id,
        run: RunId(run),
        statements: vec![sql.to_owned()],
        parameters: parameters.clone(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
        scans: ScanApproval::Ask,
    });
    let mut rows = Vec::new();
    let mut failure = None;
    loop {
        match next_run_event(engine) {
            Event::RowsFetched {
                run: RunId(r),
                rows: page,
                ..
            } if r == run => rows.extend(page),
            Event::StatementFinished {
                run: RunId(r),
                result: Err(error),
                ..
            } if r == run => failure = Some(error),
            Event::RunFinished { run: RunId(r), .. } if r == run => {
                return failure.map_or(Ok(rows), Err);
            }
            _ => {}
        }
    }
}

fn rows_with(
    engine: &mut TestEngine,
    source: &TestSource,
    run: u64,
    sql: &str,
    parameters: &ParameterValues,
) -> Vec<Row> {
    run_with(engine, source, run, sql, parameters)
        .unwrap_or_else(|failure| panic!("{}: {sql} failed: {failure:?}", source.name))
}

#[test]
fn binds_text_numbers_repeats_and_null_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let table = scratch_table();
    for source in test_sources(&mut engine, dir.path()) {
        let name = source.name;
        let drop = format!("DROP TABLE IF EXISTS {table}");
        run_ok(&mut engine, &source, 1, &[&drop]);
        run_ok(
            &mut engine,
            &source,
            2,
            &[
                &format!(
                    "CREATE TABLE {table} (id INTEGER PRIMARY KEY, name VARCHAR(20), \
                     note VARCHAR(20))"
                ),
                &format!(
                    "INSERT INTO {table} (id, name, note) VALUES (1, 'a', NULL), (2, 'b', 'x')"
                ),
            ],
        );

        let by_id = format!("SELECT name FROM {table} WHERE id = :id");
        assert_eq!(
            rows_with(
                &mut engine,
                &source,
                3,
                &by_id,
                &values(&[("id", Some("2"))])
            ),
            [vec![text("b")]],
            "{name}: a number typed as text, compared with a number column"
        );
        let insert = format!("INSERT INTO {table} (id, name, note) VALUES (:id, :name, :note)");
        let row = values(&[("id", Some("3")), ("name", Some("it's")), ("note", None)]);
        rows_with(&mut engine, &source, 4, &insert, &row);
        let both = format!(
            "SELECT count(*) FROM {table} WHERE note IS NULL AND (name = :name OR id < :id)"
        );
        assert_eq!(
            rows_with(&mut engine, &source, 5, &both, &row),
            [vec![text("2")]],
            "{name}: NULL inserted, a quote and a repeated parameter"
        );
        run_ok(&mut engine, &source, 6, &[&drop]);
    }
}

#[test]
fn writes_literals_for_clickhouse() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let Some(source) = clickhouse_source(&mut engine) else {
        return;
    };

    let rows = rows_with(
        &mut engine,
        &source,
        1,
        "SELECT number, :word FROM numbers(5) WHERE number = :n OR :word = 'none'",
        &values(&[("n", Some("3")), ("word", Some("it's"))]),
    );

    assert_eq!(rows, [vec![text("3"), text("it's")]]);
}

#[test]
fn records_the_values_refuses_a_missing_one_and_exports_with_them() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    let source = TestSource {
        id,
        dialect: Dialect::Sqlite,
        name: "sqlite",
    };

    run_ok(
        &mut engine,
        &source,
        1,
        &["INSERT INTO t VALUES (1), (2), (3)"],
    );
    let found = rows_with(
        &mut engine,
        &source,
        2,
        "SELECT id FROM t WHERE id = :id",
        &values(&[("id", Some("1")), ("unused", Some("x"))]),
    );
    assert_eq!(found, [vec![text("1")]]);
    let failure = run_with(
        &mut engine,
        &source,
        3,
        "SELECT :a, :b",
        &values(&[("a", Some("1"))]),
    )
    .unwrap_err();
    assert_eq!(failure.message, "there is no value for :b");

    engine.send(Command::SearchHistory(dbrover_store::HistoryQuery::all(id)));
    let Event::HistoryFound {
        result: Ok(entries),
        ..
    } = next_run_event(&mut engine)
    else {
        panic!("expected history entries");
    };
    let recorded: Vec<&str> = entries.iter().map(|entry| entry.sql.as_str()).collect();
    assert_eq!(
        recorded,
        [
            "SELECT id FROM t WHERE id = :id\n-- :id = '1'",
            "INSERT INTO t VALUES (1), (2), (3)"
        ],
        "the statement as typed and its values, and nothing for the one never sent"
    );

    let path = dir.path().join("ids.csv");
    engine.send(Command::ExportAllRows {
        console: CONSOLE,
        export: dbrover_engine::ExportAll {
            run: RunId(4),
            sql: "SELECT id FROM t WHERE id <= :last ORDER BY id".to_owned(),
            parameters: values(&[("last", Some("2"))]),
            path: path.clone(),
            spec: dbrover_export::ExportSpec::Csv(Default::default()),
        },
    });
    loop {
        match next_run_event(&mut engine) {
            Event::ExportFinished { result, .. } => {
                assert_eq!(result, Ok(2));
                break;
            }
            Event::ExportProgress { .. } => {}
            other => panic!("expected the export, got {other:?}"),
        }
    }
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 3);
}

#[test]
fn maps_postgres_error_positions_back_to_the_statement_as_typed() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let Some(source) = test_sources(&mut engine, dir.path())
        .into_iter()
        .find(|source| source.dialect == Dialect::Postgres)
    else {
        return;
    };
    let sql = "SELECT :a, no_such_column";

    let failure = run_with(
        &mut engine,
        &source,
        1,
        sql,
        &values(&[("a", Some("a much longer value than the name"))]),
    )
    .unwrap_err();

    assert_eq!(failure.position, sql.find("no_such_column"));
}
