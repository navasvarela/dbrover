//! Query console runs, driven headlessly: statements, paging, failures and cancellation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod grid;
mod history;
mod parameters;

use std::sync::Arc;

use common::{
    TestEngine, clickhouse_test_server, cql_test_servers, mongo_test_server, mysql_test_servers,
    postgres_test_server, redis_test_servers, search_test_servers, sqlite_data_source,
    sqlite_database, sqlserver_test_server,
};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, Identifier,
    RelationName,
};
use dbrover_core::{Quoting, RejectionKind, TransactionStatus};
use dbrover_engine::{
    Command, ConsoleId, EditFailure, Event, PasswordUpdate, RowEdit, RunId, RunOrigin, RunOutcome,
    RunPurpose, SessionState, StatementCheck, StatementFailure, TransactionEnd, TransactionMode,
};
use dbrover_store::{HistoryOutcome, HistoryQuery};

const CONSOLE: ConsoleId = ConsoleId(1);
const ROWS: usize = 1200;

/// An engine with one saved SQLite data source, and its id.
fn engine_with_sqlite(dir: &std::path::Path) -> (TestEngine, DataSourceId) {
    let mut engine = TestEngine::start(&dir.join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = sqlite_data_source(sqlite_database(dir));
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    (engine, id)
}

/// The next event that isn't a console connection state change.
fn next_run_event(engine: &mut TestEngine) -> Event {
    loop {
        match engine.next_event() {
            Event::ConsoleSessionChanged { .. } => {}
            event => return event,
        }
    }
}

fn execute(engine: &TestEngine, data_source: DataSourceId, run: u64, statements: &[&str]) {
    execute_in(engine, data_source, run, statements, TransactionMode::Auto);
}

fn execute_in(
    engine: &TestEngine,
    data_source: DataSourceId,
    run: u64,
    statements: &[&str],
    transactions: TransactionMode,
) {
    engine.send(Command::Execute {
        console: CONSOLE,
        data_source,
        run: RunId(run),
        statements: statements.iter().map(|s| (*s).to_owned()).collect(),
        parameters: dbrover_sql::ParameterValues::default(),
        transactions,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
    });
}

fn expect_transaction(engine: &mut TestEngine, expected: TransactionStatus) {
    match next_run_event(engine) {
        Event::TransactionChanged { console, status } if console == CONSOLE => {
            assert_eq!(status, expected);
        }
        other => panic!("expected the transaction to become {expected:?}, got {other:?}"),
    }
}

/// Skips the run's remaining events up to and including its end.
fn finish_run(engine: &mut TestEngine, run: u64) {
    loop {
        if let Event::RunFinished { run: RunId(r), .. } = next_run_event(engine)
            && r == run
        {
            return;
        }
    }
}

fn expect_rows(engine: &mut TestEngine, run: u64, statement: usize) -> (usize, bool) {
    match next_run_event(engine) {
        Event::RowsFetched {
            run: RunId(r),
            statement: s,
            rows,
            more,
        } if r == run && s == statement => (rows.len(), more),
        other => panic!("expected rows of statement {statement}, got {other:?}"),
    }
}

fn expect_statement_ok(engine: &mut TestEngine, run: u64, statement: usize) -> (Option<u64>, bool) {
    match next_run_event(engine) {
        Event::StatementFinished {
            run: RunId(r),
            statement: s,
            result: Ok(summary),
        } if r == run && s == statement => (summary.rows_changed, summary.truncated),
        other => panic!("expected statement {statement} to succeed, got {other:?}"),
    }
}

fn expect_run_finished(engine: &mut TestEngine, run: u64, expected: RunOutcome) {
    match next_run_event(engine) {
        Event::RunFinished {
            run: RunId(r),
            outcome,
            ..
        } if r == run => assert_eq!(outcome, expected),
        other => panic!("expected run {run} to finish, got {other:?}"),
    }
}

const SETUP: [&str; 2] = [
    "CREATE TABLE numbers (n INTEGER)",
    "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 1200) \
     INSERT INTO numbers SELECT n FROM s",
];

#[test]
fn runs_a_script_and_pages_through_the_last_result() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());

    execute(
        &engine,
        id,
        1,
        &[SETUP[0], SETUP[1], "SELECT n FROM numbers ORDER BY n"],
    );

    expect_statement_ok(&mut engine, 1, 0);
    assert_eq!(
        expect_statement_ok(&mut engine, 1, 1),
        (Some(ROWS as u64), false)
    );
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { statement: 2, ref columns, .. } if columns[0].name == "n"
    ));
    assert_eq!(expect_rows(&mut engine, 1, 2), (500, true));
    expect_run_finished(&mut engine, 1, RunOutcome::Completed);

    engine.send(Command::FetchMore {
        console: CONSOLE,
        run: RunId(1),
    });
    assert_eq!(expect_rows(&mut engine, 1, 2), (500, true));
    engine.send(Command::FetchMore {
        console: CONSOLE,
        run: RunId(1),
    });
    assert_eq!(expect_rows(&mut engine, 1, 2), (200, false));
    let (_, truncated) = expect_statement_ok(&mut engine, 1, 2);
    assert!(!truncated);
}

#[test]
fn stops_at_the_first_failure_and_reports_where() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());

    execute(
        &engine,
        id,
        1,
        &["SELECT 1", "SELECT 2 FROM WHERE", "SELECT 3"],
    );

    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { statement: 0, .. }
    ));
    assert_eq!(expect_rows(&mut engine, 1, 0), (1, false));
    expect_statement_ok(&mut engine, 1, 0);
    match next_run_event(&mut engine) {
        Event::StatementFinished {
            statement: 1,
            result: Err(StatementFailure { message, position }),
            ..
        } => {
            assert!(message.contains("syntax error"), "{message}");
            assert_eq!(position, Some("SELECT 2 FROM ".len()));
        }
        other => panic!("expected statement 1 to fail, got {other:?}"),
    }
    expect_run_finished(&mut engine, 1, RunOutcome::Failed);
}

#[test]
fn cancels_a_running_statement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());

    execute(
        &engine,
        id,
        1,
        &["WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) SELECT count(*) FROM c"],
    );
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { .. }
    ));
    engine.send(Command::Cancel {
        console: CONSOLE,
        run: RunId(1),
    });

    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementFinished { result: Err(_), .. }
    ));
    expect_run_finished(&mut engine, 1, RunOutcome::Cancelled);
}

#[test]
fn a_new_run_discards_the_previous_unread_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(
        &engine,
        id,
        1,
        &[SETUP[0], SETUP[1], "SELECT n FROM numbers"],
    );
    expect_statement_ok(&mut engine, 1, 0);
    expect_statement_ok(&mut engine, 1, 1);
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { .. }
    ));
    assert_eq!(expect_rows(&mut engine, 1, 2), (500, true));
    expect_run_finished(&mut engine, 1, RunOutcome::Completed);

    execute(&engine, id, 2, &["SELECT count(*) FROM numbers"]);

    assert_eq!(expect_statement_ok(&mut engine, 1, 2), (None, true));
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { run: RunId(2), .. }
    ));
    assert_eq!(expect_rows(&mut engine, 2, 0), (1, false));
    expect_statement_ok(&mut engine, 2, 0);
    expect_run_finished(&mut engine, 2, RunOutcome::Completed);
}

#[test]
fn records_each_statement_in_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(&engine, id, 1, &["SELECT 1", "SELECT * FROM missing"]);
    while !matches!(next_run_event(&mut engine), Event::RunFinished { .. }) {}

    let query = HistoryQuery {
        text: "select".to_owned(),
        ..HistoryQuery::all(id)
    };
    engine.send(Command::SearchHistory(query.clone()));

    let Event::HistoryFound {
        query: answered,
        result: Ok(entries),
    } = next_run_event(&mut engine)
    else {
        panic!("expected history entries");
    };
    assert_eq!(answered, query);
    let found: Vec<(&str, HistoryOutcome, Option<u64>)> = entries
        .iter()
        .map(|entry| (entry.sql.as_str(), entry.outcome, entry.rows))
        .collect();
    assert_eq!(
        found,
        [
            ("SELECT * FROM missing", HistoryOutcome::Error, None),
            ("SELECT 1", HistoryOutcome::Ok, None),
        ]
    );
}

#[test]
fn manual_mode_keeps_changes_pending_until_rolled_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());

    execute_in(
        &engine,
        id,
        1,
        &["SELECT count(*) FROM t", "INSERT INTO t VALUES (1)"],
        TransactionMode::Manual,
    );
    // The transaction opens before the first statement, even one that only reads.
    expect_transaction(&mut engine, TransactionStatus::InTransaction);
    finish_run(&mut engine, 1);

    engine.send(Command::EndTransaction {
        console: CONSOLE,
        end: TransactionEnd::Rollback,
    });
    expect_transaction(&mut engine, TransactionStatus::Idle);
    assert_eq!(
        next_run_event(&mut engine),
        Event::TransactionEnded {
            console: CONSOLE,
            result: Ok(TransactionEnd::Rollback),
        }
    );

    execute(&engine, id, 2, &["SELECT count(*) FROM t"]);
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { .. }
    ));
    match next_run_event(&mut engine) {
        Event::RowsFetched { rows, .. } => assert_eq!(rows, [[Some("0".to_owned())]]),
        other => panic!("expected the count, got {other:?}"),
    }
}

#[test]
fn notices_a_transaction_the_user_begins_and_disconnecting_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());

    execute(&engine, id, 1, &["BEGIN", "INSERT INTO t VALUES (1)"]);
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementFinished {
            statement: 0,
            result: Ok(_),
            ..
        }
    ));
    expect_transaction(&mut engine, TransactionStatus::InTransaction);
    finish_run(&mut engine, 1);

    engine.send(Command::Disconnect(id));
    assert_eq!(
        engine.next_event(),
        Event::ConsoleSessionChanged {
            console: CONSOLE,
            state: SessionState::Disconnected,
        }
    );
    let rows: i64 = rusqlite::Connection::open(dir.path().join("app.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0, "the insert was rolled back");
}

/// Needs `DBROVER_TEST_POSTGRES_URL`; skipped without it.
#[test]
fn committing_a_failed_postgres_transaction_rolls_it_back() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(server.password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

    execute_in(&engine, id, 1, &["SELECT 1 / 0"], TransactionMode::Manual);
    let mut statuses = Vec::new();
    loop {
        match next_run_event(&mut engine) {
            Event::TransactionChanged { status, .. } => statuses.push(status),
            Event::RunFinished { outcome, .. } => {
                assert_eq!(outcome, RunOutcome::Failed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        statuses,
        [TransactionStatus::InTransaction, TransactionStatus::Failed]
    );

    engine.send(Command::EndTransaction {
        console: CONSOLE,
        end: TransactionEnd::Commit,
    });
    expect_transaction(&mut engine, TransactionStatus::Idle);
    assert_eq!(
        next_run_event(&mut engine),
        Event::TransactionEnded {
            console: CONSOLE,
            result: Ok(TransactionEnd::Rollback),
        }
    );
}

/// The next event that isn't a connection state change of the console or the explorer.
fn next_answer(engine: &mut TestEngine) -> Event {
    loop {
        match engine.next_event() {
            Event::ConsoleSessionChanged { .. } | Event::SessionChanged { .. } => {}
            event => return event,
        }
    }
}

fn names(dir: &std::path::Path) -> Vec<(i64, String)> {
    let connection = rusqlite::Connection::open(dir.join("app.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT id, name FROM people ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Sends the grid's statements, without the rows' changes the app adds for the history.
fn edits(engine: &TestEngine, statements: &[&str]) {
    let statements = statements
        .iter()
        .map(|sql| RowEdit {
            sql: (*sql).to_owned(),
            change: None,
        })
        .collect();
    engine.send(Command::ApplyEdits {
        console: CONSOLE,
        statements,
    });
}

fn expect_edits(engine: &mut TestEngine) -> Result<(), EditFailure> {
    loop {
        match next_run_event(engine) {
            Event::EditsApplied { console, result } if console == CONSOLE => return result,
            Event::TransactionChanged { .. } => {}
            other => panic!("expected the edits' outcome, got {other:?}"),
        }
    }
}

#[test]
fn describes_a_table_on_the_explorers_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    let table = RelationName {
        schema: None,
        name: Identifier {
            name: "T".to_owned(),
            quoted: false,
        },
    };

    engine.send(Command::DescribeTable {
        data_source: id,
        table: table.clone(),
    });

    match next_answer(&mut engine) {
        Event::TableDescribed {
            data_source,
            table: described_table,
            result: Ok(Some(description)),
        } => {
            assert_eq!((data_source, described_table), (id, table));
            assert_eq!(
                description.qualified_name(Quoting::STANDARD),
                "\"main\".\"t\""
            );
            assert_eq!(description.primary_key, ["id"]);
        }
        other => panic!("expected the table's description, got {other:?}"),
    }
}

#[test]
fn applies_edits_all_or_none_in_their_own_transaction_or_a_savepoint() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(
        &engine,
        id,
        1,
        &[
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)",
            "INSERT INTO people VALUES (1, 'ada'), (2, 'bob')",
        ],
    );
    finish_run(&mut engine, 1);

    edits(&engine, &["UPDATE people SET name = 'Ada' WHERE id = 1"]);
    assert_eq!(expect_edits(&mut engine), Ok(()));
    edits(
        &engine,
        &[
            "UPDATE people SET name = 'Bob' WHERE id = 2",
            "UPDATE people SET name = 'nobody' WHERE id = 3",
        ],
    );
    let failure = expect_edits(&mut engine).unwrap_err();
    assert_eq!(failure.statement, Some(1));
    assert!(
        failure.message.contains("changed 0 rows"),
        "{}",
        failure.message
    );
    assert_eq!(
        names(dir.path()),
        [(1, "Ada".to_owned()), (2, "bob".to_owned())]
    );

    // Inside the console's own transaction, a failure undoes only the edits.
    execute_in(
        &engine,
        id,
        2,
        &["UPDATE people SET name = 'pending' WHERE id = 1"],
        TransactionMode::Manual,
    );
    finish_run(&mut engine, 2);
    edits(
        &engine,
        &[
            "UPDATE people SET name = 'Bob' WHERE id = 2",
            "UPDATE nowhere SET x = 1",
        ],
    );
    assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));
    edits(&engine, &["UPDATE people SET name = 'Bob' WHERE id = 2"]);
    assert_eq!(expect_edits(&mut engine), Ok(()));
    engine.send(Command::EndTransaction {
        console: CONSOLE,
        end: TransactionEnd::Commit,
    });
    while !matches!(next_run_event(&mut engine), Event::TransactionEnded { .. }) {}

    assert_eq!(
        names(dir.path()),
        [(1, "pending".to_owned()), (2, "Bob".to_owned())]
    );
}

/// Needs `DBROVER_TEST_POSTGRES_URL`; skipped without it.
#[test]
fn applies_and_rolls_back_edits_on_postgres() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(server.password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    // A temporary table lives on the console's own connection, where edits run.
    execute(
        &engine,
        id,
        1,
        &[
            "CREATE TEMP TABLE people (id integer PRIMARY KEY, name text)",
            "INSERT INTO people VALUES (1, 'ada')",
        ],
    );
    finish_run(&mut engine, 1);

    edits(&engine, &["UPDATE people SET name = 'Ada' WHERE id = 1"]);
    assert_eq!(expect_edits(&mut engine), Ok(()));
    edits(
        &engine,
        &[
            "UPDATE people SET name = 'gone' WHERE id = 1",
            "UPDATE people SET name = 1 / 0 WHERE id = 1",
        ],
    );
    assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));
    execute(&engine, id, 2, &["SELECT name FROM people"]);

    loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            assert_eq!(rows, [[Some("Ada".to_owned())]]);
            break;
        }
    }
}

/// The `UPDATE`s grid editing writes for MySQL: backticked names, every value a string
/// literal with its backslashes doubled. Needs `DBROVER_TEST_MYSQL_URL` or
/// `DBROVER_TEST_MARIADB_URL`; skipped without them.
#[test]
fn applies_and_rolls_back_edits_on_mysql() {
    for (config, password) in mysql_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "test server".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Mysql(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password: PasswordUpdate::Set(password),
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
        execute(
            &engine,
            id,
            1,
            &[
                "CREATE TEMPORARY TABLE people (id int PRIMARY KEY, name text NOT NULL)",
                "INSERT INTO people VALUES (1, 'ada')",
            ],
        );
        finish_run(&mut engine, 1);

        edits(
            &engine,
            &[r"UPDATE `dbrover`.`people` SET `name` = 'C:\\ada''s' WHERE `id` = '1'"],
        );
        assert_eq!(expect_edits(&mut engine), Ok(()));
        edits(
            &engine,
            &[
                "UPDATE `people` SET `name` = 'gone' WHERE `id` = '1'",
                "UPDATE `people` SET `name` = NULL WHERE `id` = '1'",
            ],
        );
        assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));
        execute(&engine, id, 2, &["SELECT name FROM people"]);

        loop {
            if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
                assert_eq!(rows, [[Some(r"C:\ada's".to_owned())]]);
                break;
            }
        }
    }
}

/// The `UPDATE`s grid editing writes for SQL Server: bracketed names, every value an `N'…'`
/// literal, so text outside the database's code page survives. Inside a transaction the
/// console opened (manual mode), the edits are a `SAVE TRANSACTION` that a failure rolls back
/// to. Needs `DBROVER_TEST_SQLSERVER_URL`; skipped without it.
#[test]
fn applies_and_rolls_back_edits_on_sqlserver() {
    let Some((config, password)) = sqlserver_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::SqlServer(config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    execute(
        &engine,
        id,
        1,
        &[
            "CREATE TABLE #people (id int PRIMARY KEY, name nvarchar(50) NOT NULL)",
            "INSERT INTO #people VALUES (1, N'ada')",
        ],
    );
    finish_run(&mut engine, 1);

    edits(
        &engine,
        &["UPDATE [dbo].[#people] SET [name] = N'Zoë 李' WHERE [id] = N'1'"],
    );
    assert_eq!(expect_edits(&mut engine), Ok(()));
    edits(
        &engine,
        &[
            "UPDATE [#people] SET [name] = N'gone' WHERE [id] = N'1'",
            "UPDATE [#people] SET [name] = NULL WHERE [id] = N'1'",
        ],
    );
    assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));

    // The same inside a transaction the console opened itself.
    execute(&engine, id, 2, &["BEGIN TRANSACTION"]);
    finish_run(&mut engine, 2);
    edits(
        &engine,
        &[
            "UPDATE [#people] SET [name] = N'gone' WHERE [id] = N'1'",
            "UPDATE [#people] SET [name] = NULL WHERE [id] = N'1'",
        ],
    );
    assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));
    execute(&engine, id, 3, &["SELECT name FROM #people", "COMMIT"]);

    loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            assert_eq!(rows, [[Some("Zoë 李".to_owned())]]);
            break;
        }
    }
}

/// SQL Server explains through `SET SHOWPLAN_XML`, which the engine switches on in a batch of
/// its own and off again afterwards: the plan arrives as one XML value, and the next
/// statement runs as usual, even after an explain that failed. Needs
/// `DBROVER_TEST_SQLSERVER_URL`; skipped without it.
#[test]
fn explains_on_sqlserver_with_showplan_and_switches_it_off() {
    let Some((config, password)) = sqlserver_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::SqlServer(config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    let explain = |run: u64, sql: &str| Command::Execute {
        console: CONSOLE,
        data_source: id,
        run: RunId(run),
        statements: vec![sql.to_owned()],
        parameters: dbrover_sql::ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Explain,
        origin: RunOrigin::Console,
    };

    engine.send(explain(
        1,
        "SELECT name FROM sys.objects WHERE object_id = 1",
    ));
    let plan = loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            break rows;
        }
    };
    let xml = plan[0][0].clone().unwrap_or_default();
    assert!(xml.contains("<ShowPlanXML"), "{xml}");
    finish_run(&mut engine, 1);

    engine.send(explain(2, "SELECT * FROM dbrover_no_such_table"));
    finish_run(&mut engine, 2);

    execute(&engine, id, 3, &["SELECT 42"]);
    loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            assert_eq!(rows, [[Some("42".to_owned())]]);
            break;
        }
    }
}

/// A ClickHouse console keeps its session from run to run: a temporary table outlives the run
/// that made it, and `EXPLAIN` sees it. Needs
/// `DBROVER_TEST_CLICKHOUSE_URL`; skipped without it.
#[test]
fn keeps_a_clickhouse_session_across_runs() {
    let Some((config, password)) = clickhouse_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::ClickHouse(config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

    execute(
        &engine,
        id,
        1,
        &[
            "CREATE TEMPORARY TABLE people (id UInt64, name String)",
            "INSERT INTO people VALUES (1, 'ada')",
        ],
    );
    finish_run(&mut engine, 1);
    execute(&engine, id, 2, &["EXPLAIN SELECT name FROM people"]);
    let plan = loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            break rows;
        }
    };
    assert!(
        plan.iter()
            .flatten()
            .flatten()
            .any(|line| line.contains("ReadFrom")),
        "{plan:?}"
    );
    finish_run(&mut engine, 2);

    execute(&engine, id, 3, &["SELECT name FROM people"]);
    loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            assert_eq!(rows, [[Some("ada".to_owned())]]);
            break;
        }
    }
}

/// A Redis or Valkey console runs commands as typed and shows each reply by its shape: a hash
/// as field and value, a list as one column. Needs `DBROVER_TEST_REDIS_URL` or
/// `DBROVER_TEST_VALKEY_URL`; skipped without them.
#[test]
fn runs_redis_commands_and_shows_their_replies() {
    for (config, password) in redis_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "cache".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Redis(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password: PasswordUpdate::Set(password),
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

        let key = format!("dbrover:engine:{}", std::process::id());
        execute(
            &engine,
            id,
            1,
            &[
                &format!("DEL {key}"),
                &format!("HSET {key} name \"Ada Lovelace\""),
                &format!("HGETALL {key}"),
            ],
        );
        let (columns, rows) = loop {
            match next_run_event(&mut engine) {
                Event::StatementStarted {
                    statement: 2,
                    columns,
                    ..
                } => {
                    let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) else {
                        panic!("expected the hash's fields");
                    };
                    break (columns, rows);
                }
                Event::StatementFinished {
                    result: Err(error), ..
                } => panic!("{}", error.message),
                _ => {}
            }
        };
        let names: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["field", "value"]);
        assert_eq!(
            rows,
            [[Some("name".to_owned()), Some("Ada Lovelace".to_owned())]]
        );
        finish_run(&mut engine, 1);
        execute(&engine, id, 2, &[&format!("DEL {key}")]);
        finish_run(&mut engine, 2);
    }
}

/// Grid edits on Redis run one command at a time: those before a failing one stay, and the
/// failure says so. Needs `DBROVER_TEST_REDIS_URL` or `DBROVER_TEST_VALKEY_URL`.
#[test]
fn applies_redis_edits_one_command_at_a_time() {
    for (config, password) in redis_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "cache".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Redis(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password: PasswordUpdate::Set(password),
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

        let key = format!("dbrover:edits:{}", std::process::id());
        execute(&engine, id, 1, &[&format!("DEL {key}")]);
        finish_run(&mut engine, 1);

        edits(
            &engine,
            &[&format!("HSET {key} a 1"), &format!("HSET {key} b 2")],
        );
        assert_eq!(expect_edits(&mut engine), Ok(()));

        edits(
            &engine,
            &[
                &format!("HSET {key} c 3"),
                &format!("LPUSH {key} wrong"),
                &format!("HSET {key} d 4"),
            ],
        );
        let failure = expect_edits(&mut engine).unwrap_err();
        assert_eq!(failure.statement, Some(1));
        assert!(
            failure.message.starts_with("WRONGTYPE")
                && failure.message.ends_with("the change before it was made"),
            "{}",
            failure.message
        );

        execute(&engine, id, 2, &[&format!("HGETALL {key}")]);
        let mut fields = loop {
            if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
                break rows;
            }
        };
        fields.sort();
        let names: Vec<Option<String>> = fields.into_iter().map(|row| row[0].clone()).collect();
        assert_eq!(
            names,
            [
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned())
            ]
        );
        finish_run(&mut engine, 2);
        execute(&engine, id, 3, &[&format!("DEL {key}")]);
        finish_run(&mut engine, 3);
    }
}

/// A MongoDB console runs the shell's commands as typed: a `find` reaches the console as
/// documents, a write as what it changed, and `use` moves the console to another database.
/// Needs `DBROVER_TEST_MONGODB_URL`; skipped without it.
#[test]
fn runs_mongodb_commands_and_shows_documents() {
    let Some((config, password)) = mongo_test_server("dbrover_engine_console") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "shop".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Mongo(config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

    execute(
        &engine,
        id,
        1,
        &[
            "db.orders.drop()",
            "db.orders.insertOne({_id: 1, address: {city: 'Madrid'}})",
            "db.orders.find({}, {address: 1})",
        ],
    );
    let (shape, rows) = loop {
        match next_run_event(&mut engine) {
            Event::StatementStarted {
                statement: 2,
                shape,
                ..
            } => {
                let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) else {
                    panic!("expected the documents");
                };
                break (shape, rows);
            }
            Event::StatementFinished {
                statement,
                result: Err(error),
                ..
            } if statement > 0 => panic!("{}", error.message),
            _ => {}
        }
    };
    assert_eq!(shape, dbrover_core::ResultShape::Documents, "{rows:?}");
    assert_eq!(
        rows,
        [[Some(
            r#"{"_id":{"$numberInt":"1"},"address":{"city":"Madrid"}}"#.to_owned()
        )]]
    );
    finish_run(&mut engine, 1);
}

/// Run to file of a `find`: every document, past the first page, written as JSON Lines as the
/// cursor reads them, each line the document as the grid would have it. Needs
/// `DBROVER_TEST_MONGODB_URL`; skipped without it.
#[test]
fn runs_a_mongodb_find_straight_to_a_file() {
    let Some((config, password)) = mongo_test_server("dbrover_engine_file") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "shop".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Mongo(config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    let documents: Vec<String> = (0..1_200).map(|n| format!("{{n: {n}}}")).collect();
    let insert = format!("db.docs.insertMany([{}])", documents.join(", "));
    execute(&engine, id, 1, &["db.docs.drop()", &insert]);
    finish_run(&mut engine, 1);

    let path = dir.path().join("docs.jsonl");
    engine.send(Command::RunToFile {
        console: CONSOLE,
        data_source: id,
        export: dbrover_engine::ExportAll {
            run: RunId(2),
            sql: "db.docs.find({}, {_id: 0}).sort({n: 1})".to_owned(),
            parameters: dbrover_sql::ParameterValues::default(),
            path: path.clone(),
            spec: dbrover_export::ExportSpec::JsonLinesDocuments,
        },
    });
    let result = loop {
        if let Event::ExportFinished { result, .. } = next_run_event(&mut engine) {
            break result;
        }
    };
    assert_eq!(result, Ok(1_200));
    let written = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), 1_200);
    assert_eq!(lines[700], r#"{"n":{"$numberInt":"700"}}"#);
}

/// An Elasticsearch or OpenSearch console sends requests as typed: a search's hits reach the
/// console as documents, `_index`, `_id` and `_score` first, and a `_sql` statement as rows.
/// Needs `DBROVER_TEST_ELASTICSEARCH_URL` or `DBROVER_TEST_OPENSEARCH_URL`; skipped without.
#[test]
fn runs_search_requests_and_shows_hits_as_documents() {
    for (name, config, password) in search_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "search".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Elasticsearch(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password,
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

        execute(
            &engine,
            id,
            1,
            &[
                "DELETE /dbrover_engine_console?ignore_unavailable=true",
                "PUT /dbrover_engine_console/_doc/1?refresh=true\n{ \"address\": { \"city\": \"Madrid\" } }",
                "GET /dbrover_engine_console/_search",
            ],
        );
        let (shape, rows) = loop {
            match next_run_event(&mut engine) {
                Event::StatementStarted {
                    statement: 2,
                    shape,
                    ..
                } => {
                    let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) else {
                        panic!("{name}: expected the hits");
                    };
                    break (shape, rows);
                }
                Event::StatementFinished {
                    statement,
                    result: Err(error),
                    ..
                } if statement > 0 => panic!("{name}: {}", error.message),
                _ => {}
            }
        };
        assert_eq!(
            shape,
            dbrover_core::ResultShape::Documents,
            "{name}: {rows:?}"
        );
        assert_eq!(
            rows,
            [[Some(
                r#"{"_index":"dbrover_engine_console","_id":"1","_score":1.0,"address":{"city":"Madrid"}}"#
                    .to_owned()
            )]],
            "{name}"
        );
        finish_run(&mut engine, 1);
    }
}

/// Run to file of a search: every hit, past the first page, written as JSON Lines as the
/// driver pages them, each line the hit as the grid would have it. Needs
/// `DBROVER_TEST_ELASTICSEARCH_URL` or `DBROVER_TEST_OPENSEARCH_URL`; skipped without.
#[test]
fn runs_a_search_straight_to_a_file() {
    for (name, config, password) in search_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "search".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Elasticsearch(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password,
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
        let mut bulk = "POST /dbrover_engine_file/_bulk?refresh=true\n".to_owned();
        for n in 0..1_200 {
            bulk.push_str(&format!(
                "{{\"index\":{{\"_id\":\"{n}\"}}}}\n{{\"n\":{n}}}\n"
            ));
        }
        execute(
            &engine,
            id,
            1,
            &["DELETE /dbrover_engine_file?ignore_unavailable=true", &bulk],
        );
        finish_run(&mut engine, 1);

        let path = dir.path().join("hits.jsonl");
        engine.send(Command::RunToFile {
            console: CONSOLE,
            data_source: id,
            export: dbrover_engine::ExportAll {
                run: RunId(2),
                sql: "GET /dbrover_engine_file/_search\n{ \"sort\": [\"n\"] }".to_owned(),
                parameters: dbrover_sql::ParameterValues::default(),
                path: path.clone(),
                spec: dbrover_export::ExportSpec::JsonLinesDocuments,
            },
        });
        let result = loop {
            if let Event::ExportFinished { result, .. } = next_run_event(&mut engine) {
                break result;
            }
        };
        assert_eq!(result, Ok(1_200), "{name}");
        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 1_200, "{name}");
        assert_eq!(
            lines[700], r#"{"_index":"dbrover_engine_file","_id":"700","_score":null,"n":700}"#,
            "{name}"
        );
    }
}

/// Grid edits on DuckDB: standard quoting, and, since DuckDB has no savepoints, a refusal
/// inside a transaction the console opened rather than a failed edit only `ROLLBACK` could
/// undo.
#[test]
fn applies_edits_on_duckdb_and_refuses_them_inside_a_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "duck".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::InMemory,
        }),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    execute(
        &engine,
        id,
        1,
        &[
            "CREATE TABLE people (id integer PRIMARY KEY, name varchar NOT NULL)",
            "INSERT INTO people VALUES (1, 'ada')",
        ],
    );
    finish_run(&mut engine, 1);

    edits(
        &engine,
        &["UPDATE \"main\".\"people\" SET \"name\" = 'ada''s' WHERE \"id\" = '1'"],
    );
    assert_eq!(expect_edits(&mut engine), Ok(()));
    edits(
        &engine,
        &[
            "UPDATE \"people\" SET \"name\" = 'gone' WHERE \"id\" = '1'",
            "UPDATE \"people\" SET \"name\" = NULL WHERE \"id\" = '1'",
        ],
    );
    assert_eq!(expect_edits(&mut engine).unwrap_err().statement, Some(1));

    execute(&engine, id, 2, &["BEGIN"]);
    finish_run(&mut engine, 2);
    edits(
        &engine,
        &["UPDATE \"people\" SET \"name\" = 'x' WHERE \"id\" = '1'"],
    );
    let refused = expect_edits(&mut engine).unwrap_err();
    assert!(refused.message.contains("no savepoints"), "{refused:?}");
    execute(&engine, id, 3, &["ROLLBACK", "SELECT name FROM people"]);

    loop {
        if let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) {
            assert_eq!(rows, [[Some("ada's".to_owned())]]);
            break;
        }
    }
}

fn validate(engine: &TestEngine, statements: &[&str]) {
    engine.send(Command::ValidateStatements {
        console: CONSOLE,
        statements: statements.iter().map(|s| (*s).to_owned()).collect(),
    });
}

/// What the database said about each statement, in order.
fn expect_checks(engine: &mut TestEngine) -> Vec<StatementCheck> {
    match next_run_event(engine) {
        Event::StatementsValidated { console, results } if console == CONSOLE => {
            results.into_iter().map(|(_, check)| check).collect()
        }
        other => panic!("expected validation results, got {other:?}"),
    }
}

fn rejected(check: &StatementCheck) -> Option<RejectionKind> {
    match check {
        StatementCheck::Rejected(rejection) => Some(rejection.kind),
        _ => None,
    }
}

#[test]
fn validates_statements_without_running_them_or_disturbing_unread_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    validate(&engine, &["SELECT 1"]);
    assert_eq!(
        expect_checks(&mut engine),
        [StatementCheck::Skipped],
        "a console that never ran isn't connected, and validating doesn't connect it"
    );
    execute(&engine, id, 1, &[SETUP[0]]);
    finish_run(&mut engine, 1);

    validate(
        &engine,
        &[
            "INSERT INTO numbers VALUES (1)",
            "SELECT nope FROM numbers",
            "SELECT n FROM numbers WHERE",
            "PRAGMA user_version = 7",
        ],
    );
    let checks = expect_checks(&mut engine);
    assert_eq!(checks[0], StatementCheck::Accepted);
    assert_eq!(rejected(&checks[1]), Some(RejectionKind::UnknownName));
    assert_eq!(rejected(&checks[2]), Some(RejectionKind::Syntax));
    assert_eq!(
        checks[3],
        StatementCheck::Skipped,
        "pragmas are never prepared"
    );

    execute(&engine, id, 2, &[SETUP[1], "SELECT n FROM numbers"]);
    expect_statement_ok(&mut engine, 2, 0);
    assert!(matches!(
        next_run_event(&mut engine),
        Event::StatementStarted { .. }
    ));
    assert_eq!(expect_rows(&mut engine, 2, 1), (500, true));
    expect_run_finished(&mut engine, 2, RunOutcome::Completed);
    validate(&engine, &["SELECT n FROM numbers"]);
    assert_eq!(expect_checks(&mut engine), [StatementCheck::Skipped]);
    engine.send(Command::FetchMore {
        console: CONSOLE,
        run: RunId(2),
    });
    assert_eq!(
        expect_rows(&mut engine, 2, 1),
        (500, true),
        "the unread rows are still there, and only the 1200 inserted rows"
    );
}

#[test]
fn never_validates_inside_a_postgres_transaction() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "test server".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(server.config),
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(server.password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    execute(&engine, id, 1, &["SELECT 1"]);
    finish_run(&mut engine, 1);

    validate(
        &engine,
        &["SELECT nope FROM pg_class", "SELECT relname FROM pg_class"],
    );
    let checks = expect_checks(&mut engine);
    assert_eq!(rejected(&checks[0]), Some(RejectionKind::UnknownName));
    assert_eq!(checks[1], StatementCheck::Accepted);

    execute(&engine, id, 2, &["BEGIN"]);
    finish_run(&mut engine, 2);
    validate(&engine, &["SELECT nope FROM pg_class"]);
    assert_eq!(expect_checks(&mut engine), [StatementCheck::Skipped]);
}

/// A Cassandra or ScyllaDB console runs CQL, a batch as one statement, and shows a map column
/// as JSON, with each column's CQL type. Needs `DBROVER_TEST_SCYLLA_URL` or
/// `DBROVER_TEST_CASSANDRA_URL`; skipped without.
#[test]
fn runs_cql_and_shows_a_map_as_json() {
    for (name, config, password) in cql_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "cql".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Cassandra(config),
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password,
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

        execute(
            &engine,
            id,
            1,
            &[
                "DROP KEYSPACE IF EXISTS dbrover_engine_console",
                "CREATE KEYSPACE dbrover_engine_console WITH replication = \
                 {'class': 'NetworkTopologyStrategy', 'replication_factor': 1}",
                "CREATE TABLE dbrover_engine_console.t (id int PRIMARY KEY, tags map<text, int>)",
                "BEGIN BATCH\n\
                 INSERT INTO dbrover_engine_console.t (id, tags) VALUES (1, {'a': 1});\n\
                 UPDATE dbrover_engine_console.t SET tags['b'] = 2 WHERE id = 1;\n\
                 APPLY BATCH",
                "SELECT id, tags FROM dbrover_engine_console.t",
            ],
        );
        let (columns, rows) = loop {
            match next_run_event(&mut engine) {
                Event::StatementStarted {
                    statement: 4,
                    columns,
                    ..
                } => {
                    let Event::RowsFetched { rows, .. } = next_run_event(&mut engine) else {
                        panic!("{name}: expected the rows");
                    };
                    break (columns, rows);
                }
                Event::StatementFinished {
                    result: Err(error), ..
                } => panic!("{name}: {}", error.message),
                _ => {}
            }
        };
        let types: Vec<Option<&str>> = columns
            .iter()
            .map(|column| column.type_name.as_deref())
            .collect();
        assert_eq!(types, [Some("int"), Some("map<text, int>")], "{name}");
        assert_eq!(
            rows,
            [[Some("1".to_owned()), Some(r#"{"a": 1, "b": 2}"#.to_owned())]],
            "{name}"
        );
        finish_run(&mut engine, 1);
    }
}
