//! The grid's power features through the engine, on every engine with a test server: the
//! statements the app writes, built by `dbrover_sql`, run against real databases.

use dbrover_core::{Cell, Row, TableDescription};
use dbrover_engine::DatabaseFile;
use dbrover_sql::{ColumnValue, Dialect, RowSql};

use super::*;

/// A data source to run a test on, saved in the engine.
pub struct TestSource {
    pub id: DataSourceId,
    pub dialect: Dialect,
    pub name: &'static str,
}

/// Every engine a test can run on here: SQLite and DuckDB always, in files under `dir` (two
/// connections, the console's and the explorer's, must see the same tables), and the servers
/// whose `DBROVER_TEST_*_URL` is set. ClickHouse is left out: the grid never changes its rows.
pub fn test_sources(engine: &mut TestEngine, dir: &std::path::Path) -> Vec<TestSource> {
    let duckdb = dir.join("grid.duckdb");
    engine.send(Command::CreateDatabaseFile(DatabaseFile::DuckDb(
        duckdb.clone(),
    )));
    match engine.next_event() {
        Event::DatabaseFileCreated { result, .. } => result.unwrap(),
        other => panic!("expected the DuckDB file, got {other:?}"),
    }
    let mut configs = vec![
        (
            "sqlite",
            sqlite_data_source(sqlite_database(dir)).config,
            PasswordUpdate::Keep,
        ),
        (
            "duckdb",
            ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
                path: duckdb,
                read_only: false,
            })),
            PasswordUpdate::Keep,
        ),
    ];
    if let Some(server) = postgres_test_server() {
        configs.push((
            "postgres",
            ConnectionConfig::Postgres(server.config),
            PasswordUpdate::Set(server.password),
        ));
    }
    for (config, password) in mysql_test_servers() {
        configs.push((
            "mysql",
            ConnectionConfig::Mysql(config),
            PasswordUpdate::Set(password),
        ));
    }
    if let Some((config, password)) = sqlserver_test_server() {
        configs.push((
            "sqlserver",
            ConnectionConfig::SqlServer(config),
            PasswordUpdate::Set(password),
        ));
    }
    configs
        .into_iter()
        .map(|(name, config, password)| {
            let dialect = common::sql_dialect(config.driver());
            let data_source = DataSource {
                id: DataSourceId::generate(),
                name: name.to_owned(),
                color: None,
                safety: dbrover_core::SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config,
            };
            let id = data_source.id;
            engine.send(Command::SaveDataSource {
                data_source,
                password,
                ssh_secret: PasswordUpdate::Keep,
            });
            assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
            TestSource { id, dialect, name }
        })
        .collect()
}

/// The ClickHouse server, when `DBROVER_TEST_CLICKHOUSE_URL` is set, for the tests that don't
/// change rows.
pub fn clickhouse_source(engine: &mut TestEngine) -> Option<TestSource> {
    let (config, password) = clickhouse_test_server()?;
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "clickhouse".to_owned(),
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
    Some(TestSource {
        id,
        dialect: Dialect::ClickHouse,
        name: "clickhouse",
    })
}

/// A table name no other test, or another run sharing the test servers, uses at the same time.
pub fn scratch_table(purpose: &str) -> String {
    format!("w5_{purpose}_{}", std::process::id())
}

/// Runs statements in the console and waits for the run to end, failing the test if any
/// statement fails.
pub fn run_ok(engine: &mut TestEngine, source: &TestSource, run: u64, statements: &[&str]) {
    execute(engine, source.id, run, statements);
    loop {
        match next_run_event(engine) {
            Event::StatementFinished {
                result: Err(failure),
                ..
            } => panic!("{}: {statements:?} failed: {failure:?}", source.name),
            Event::RunFinished { run: RunId(r), .. } if r == run => return,
            _ => {}
        }
    }
}

/// Runs one query in the console and returns all its rows.
pub fn query(engine: &mut TestEngine, source: &TestSource, run: u64, sql: &str) -> Vec<Row> {
    execute(engine, source.id, run, &[sql]);
    let mut rows = Vec::new();
    loop {
        match next_run_event(engine) {
            Event::RowsFetched {
                run: RunId(r),
                rows: page,
                ..
            } if r == run => rows.extend(page),
            Event::StatementFinished {
                result: Err(failure),
                ..
            } => panic!("{}: {sql} failed: {failure:?}", source.name),
            Event::RunFinished { run: RunId(r), .. } if r == run => return rows,
            _ => {}
        }
    }
}

/// Describes a table on the explorer's connection, as the grid does.
pub fn describe(engine: &mut TestEngine, source: &TestSource, table: &str) -> TableDescription {
    let relation = RelationName {
        schema: None,
        name: Identifier {
            name: table.to_owned(),
            quoted: false,
        },
    };
    engine.send(Command::DescribeTable {
        data_source: source.id,
        table: relation,
    });
    loop {
        match next_answer(engine) {
            Event::TableDescribed {
                result: Ok(Some(description)),
                ..
            } => return description,
            Event::TableDescribed { result, .. } => {
                panic!("{}: couldn't describe {table}: {result:?}", source.name)
            }
            Event::TransactionChanged { .. } => {}
            other => panic!("expected the table's description, got {other:?}"),
        }
    }
}

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

fn texts(rows: &[(&str, &str)]) -> Vec<Row> {
    rows.iter()
        .map(|(id, name)| vec![text(id), text(name)])
        .collect()
}

/// A submit as the grid builds it: a delete, an update, an insert that takes the key the
/// delete freed, and an insert of nothing but defaults (`DEFAULT VALUES`, or `() VALUES ()` on
/// MySQL). Then a submit whose insert collides with a key, which undoes its delete too. Every
/// statement sent is in the history.
#[test]
fn inserts_updates_and_deletes_rows_all_or_none_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let table = scratch_table("rows");
    for source in test_sources(&mut engine, dir.path()) {
        run_ok(
            &mut engine,
            &source,
            1,
            &[
                &format!(
                    "CREATE TABLE {table} (id int NOT NULL DEFAULT 9 PRIMARY KEY, \
                     name varchar(40) NOT NULL DEFAULT 'anon')"
                ),
                &format!("INSERT INTO {table} (id, name) VALUES (1, 'ada'), (2, 'bob'), (3, 'cy')"),
            ],
        );
        let described = describe(&mut engine, &source, &table);
        let sql = RowSql::new(source.dialect, &described);
        let value = |column, value| ColumnValue { column, value };
        let (one, two, three, ada, bo) = (text("1"), text("2"), text("3"), text("Ada"), text("Bo"));

        edits(
            &engine,
            &[
                &sql.delete(&[value("id", &two)]),
                &sql.update(&[value("name", &ada)], &[value("id", &one)]),
                &sql.insert(&[value("id", &two), value("name", &bo)]),
                &sql.insert(&[]),
            ],
        );
        assert_eq!(expect_edits(&mut engine), Ok(()), "{}", source.name);
        let select = format!("SELECT id, name FROM {table} ORDER BY id");
        let expected = texts(&[("1", "Ada"), ("2", "Bo"), ("3", "cy"), ("9", "anon")]);
        assert_eq!(
            query(&mut engine, &source, 2, &select),
            expected,
            "{}",
            source.name
        );

        edits(
            &engine,
            &[
                &sql.delete(&[value("id", &three)]),
                &sql.insert(&[value("id", &one)]),
            ],
        );
        let failure = expect_edits(&mut engine).unwrap_err();
        assert_eq!(failure.statement, Some(1), "{}: {failure:?}", source.name);
        assert_eq!(
            query(&mut engine, &source, 3, &select),
            expected,
            "{}",
            source.name
        );

        engine.send(Command::SearchHistory(HistoryQuery {
            text: table.clone(),
            ..HistoryQuery::all(source.id)
        }));
        match next_answer(&mut engine) {
            Event::HistoryFound {
                result: Ok(found), ..
            } => {
                let sent = |start: &str, outcome: HistoryOutcome| {
                    found
                        .iter()
                        .any(|entry| entry.sql.starts_with(start) && entry.outcome == outcome)
                };
                assert!(
                    sent("DELETE", HistoryOutcome::Ok) && sent("INSERT", HistoryOutcome::Error),
                    "{}: {found:?}",
                    source.name
                );
            }
            other => panic!("expected the history, got {other:?}"),
        }
        run_ok(&mut engine, &source, 4, &[&format!("DROP TABLE {table}")]);
    }
    engine.shut_down();
}

/// A table's foreign key, in both tables' descriptions: as the child's own key, and as a key
/// pointing at the parent. Column names are listed in key order, and tables by their stored
/// names.
#[test]
fn describes_foreign_keys_both_ways_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let (parent, child) = (scratch_table("customers"), scratch_table("orders"));
    for source in test_sources(&mut engine, dir.path()) {
        run_ok(
            &mut engine,
            &source,
            1,
            &[
                &format!(
                    "CREATE TABLE {parent} (id int NOT NULL, region int NOT NULL, \
                     name varchar(40), PRIMARY KEY (region, id))"
                ),
                &format!(
                    "CREATE TABLE {child} (id int NOT NULL PRIMARY KEY, customer int, \
                     customer_region int, \
                     FOREIGN KEY (customer_region, customer) REFERENCES {parent} (region, id))"
                ),
            ],
        );
        let described_parent = describe(&mut engine, &source, &parent);
        let described_child = describe(&mut engine, &source, &child);
        let key = dbrover_core::ForeignKey {
            table: described_child.table_name(),
            columns: vec!["customer_region".to_owned(), "customer".to_owned()],
            referenced: described_parent.table_name(),
            referenced_columns: vec!["region".to_owned(), "id".to_owned()],
        };
        assert_eq!(
            described_child.foreign_keys,
            std::slice::from_ref(&key),
            "{}",
            source.name
        );
        assert!(described_child.referenced_by.is_empty(), "{}", source.name);
        assert_eq!(described_parent.referenced_by, [key], "{}", source.name);
        assert!(described_parent.foreign_keys.is_empty(), "{}", source.name);
        run_ok(
            &mut engine,
            &source,
            2,
            &[
                &format!("DROP TABLE {child}"),
                &format!("DROP TABLE {parent}"),
            ],
        );
    }
    engine.shut_down();
}

/// The query the grid runs to sort and filter in the database: rows where any value contains
/// the text ignoring case, `%` and `'` taken literally, `NULL` matching nothing, sorted by
/// position.
#[test]
fn sorts_and_filters_rows_in_the_database_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let table = scratch_table("arranged");
    let mut sources = test_sources(&mut engine, dir.path());
    sources.extend(clickhouse_source(&mut engine));
    for source in sources {
        let create = match source.dialect {
            Dialect::ClickHouse => {
                format!("CREATE TABLE {table} (id Int32, note Nullable(String)) ENGINE = Memory")
            }
            _ => format!("CREATE TABLE {table} (id int, note varchar(40))"),
        };
        run_ok(
            &mut engine,
            &source,
            1,
            &[
                &create,
                &format!(
                    "INSERT INTO {table} (id, note) VALUES (1, 'Ada'), (2, 'bob'), (3, 'ADAM'), \
                     (4, NULL), (5, '100% it''s')"
                ),
            ],
        );
        let typed = format!("SELECT id, note FROM {table};");
        let columns = ["id".to_owned(), "note".to_owned()];
        let ids = |engine: &mut TestEngine, run, filter, sort| -> Vec<String> {
            let arranged = dbrover_sql::arrange(
                &typed,
                source.dialect,
                &dbrover_sql::Arrangement {
                    columns: &columns,
                    sort,
                    filter,
                },
            )
            .unwrap();
            query(engine, &source, run, &arranged.sql)
                .into_iter()
                .map(|row| row[0].clone().unwrap_or_default())
                .collect()
        };
        let by_id_descending = Some(dbrover_sql::SortOrder {
            column: 0,
            descending: true,
        });
        assert_eq!(
            ids(&mut engine, 2, "aD", by_id_descending),
            ["3", "1"],
            "{}",
            source.name
        );
        assert_eq!(
            ids(&mut engine, 3, "0% IT'", None),
            ["5"],
            "{}",
            source.name
        );
        assert_eq!(
            ids(&mut engine, 4, "4", None),
            ["4"],
            "{}: an id matches",
            source.name
        );
        let all = ids(&mut engine, 5, "", by_id_descending);
        assert_eq!(all, ["5", "4", "3", "2", "1"], "{}", source.name);
        run_ok(&mut engine, &source, 6, &[&format!("DROP TABLE {table}")]);
    }
    engine.shut_down();
}

/// Exports every row of a statement and returns the progress reported and the outcome.
pub(super) fn export_all(
    engine: &mut TestEngine,
    run: u64,
    sql: &str,
    target: (&std::path::Path, dbrover_export::ExportSpec),
) -> (Vec<usize>, Result<usize, String>) {
    let (path, spec) = target;
    engine.send(Command::ExportAllRows {
        console: CONSOLE,
        export: dbrover_engine::ExportAll {
            run: RunId(run),
            sql: sql.to_owned(),
            parameters: dbrover_sql::ParameterValues::default(),
            path: path.to_owned(),
            spec,
        },
    });
    let mut progress = Vec::new();
    loop {
        match next_run_event(engine) {
            Event::ExportProgress { rows, .. } => progress.push(rows),
            Event::ExportFinished { result, .. } => return (progress, result),
            _ => {}
        }
    }
}

/// Every row of a statement that returns more rows than a page, written as they come, as CSV,
/// JSON and Parquet.
#[test]
fn exports_every_row_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let digits = scratch_table("digits");
    let mut sources = test_sources(&mut engine, dir.path());
    sources.extend(clickhouse_source(&mut engine));
    for source in sources {
        let create = match source.dialect {
            Dialect::ClickHouse => format!("CREATE TABLE {digits} (d Int32) ENGINE = Memory"),
            _ => format!("CREATE TABLE {digits} (d int)"),
        };
        run_ok(
            &mut engine,
            &source,
            1,
            &[
                &create,
                &format!(
                    "INSERT INTO {digits} (d) VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)"
                ),
            ],
        );
        let n = "a.d + 10 * b.d + 100 * c.d + 1000 * e.d";
        let sql = format!(
            "SELECT {n} AS n FROM {digits} a, {digits} b, {digits} c, {digits} e \
             WHERE {n} < 1200 ORDER BY n"
        );
        let csv = dir.path().join(format!("{}.csv", source.name));
        let (progress, result) = export_all(
            &mut engine,
            2,
            &sql,
            (&csv, dbrover_export::ExportSpec::Csv(Default::default())),
        );
        assert_eq!(result, Ok(1200), "{}", source.name);
        assert_eq!(progress, [500, 1000, 1200], "{}", source.name);
        let text = std::fs::read_to_string(&csv).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1201, "{}", source.name);
        assert_eq!(
            (lines[0], lines[1], lines[1200]),
            ("n", "0", "1199"),
            "{}",
            source.name
        );

        let json = dir.path().join(format!("{}.json", source.name));
        let (_, result) = export_all(
            &mut engine,
            3,
            &sql,
            (&json, dbrover_export::ExportSpec::Json),
        );
        assert_eq!(result, Ok(1200), "{}", source.name);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(parsed[1199]["n"], "1199", "{}", source.name);

        let parquet = dir.path().join(format!("{}.parquet", source.name));
        let (_, result) = export_all(
            &mut engine,
            4,
            &sql,
            (
                &parquet,
                dbrover_export::ExportSpec::Parquet(Default::default()),
            ),
        );
        assert_eq!(result, Ok(1200), "{}", source.name);
        assert_eq!(
            read_parquet(
                &parquet,
                "SELECT count(*), min(n), max(n), typeof(min(n)) FROM rows"
            ),
            [["1200", "0", "1199", "BIGINT"].map(|value| Some(value.to_owned()))],
            "{}",
            source.name
        );

        // The same rows as a table, exported on the explorer's connection. ClickHouse's tables
        // aren't described, so it has no grid edits and no table to name this way here.
        if source.dialect != Dialect::ClickHouse {
            let numbers = scratch_table("numbers");
            run_ok(
                &mut engine,
                &source,
                5,
                &[
                    &format!("CREATE TABLE {numbers} (n int)"),
                    &format!(
                        "INSERT INTO {numbers} (n) SELECT {n} FROM {digits} a, {digits} b, \
                         {digits} c, {digits} e WHERE {n} < 1200"
                    ),
                ],
            );
            let described = describe(&mut engine, &source, &numbers);
            let table = dbrover_export::SqlTable {
                schema: Some(described.schema),
                name: described.name,
            };
            let csv = dir.path().join(format!("{}-table.csv", source.name));
            let (progress, result) = export_table(&mut engine, source.id, table, &csv);
            assert_eq!(result, Ok(1200), "{}", source.name);
            assert_eq!(progress, [500, 1000, 1200], "{}", source.name);
            let text = std::fs::read_to_string(&csv).unwrap();
            assert_eq!(text.lines().count(), 1201, "{}", source.name);
            assert!(text.starts_with("n\r\n"), "{}", source.name);
            run_ok(&mut engine, &source, 6, &[&format!("DROP TABLE {numbers}")]);
        }

        run_ok(&mut engine, &source, 7, &[&format!("DROP TABLE {digits}")]);
    }
    engine.shut_down();
}

/// Runs `sql` on the Parquet file at `path`, as a table named `rows`, through DuckDB.
fn read_parquet(path: &std::path::Path, sql: &str) -> Vec<Vec<Option<String>>> {
    use dbrover_core::Connection as _;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let config = dbrover_core::DuckDbConfig::new(dbrover_core::DuckDbDatabase::InMemory);
        let mut connection = dbrover_driver_duckdb::connect(&config).await.unwrap();
        let file = path.to_string_lossy().replace('\'', "''");
        let sql = format!("WITH rows AS (SELECT * FROM '{file}') {sql}");
        let mut cursor = connection.execute(&sql).await.unwrap();
        let rows = cursor.next_page(100).await.unwrap();
        cursor.finish().await.unwrap();
        rows
    })
}

/// Exports every row of a table on the explorer's connection, as Export data… asks, and
/// returns the progress reported and the outcome.
fn export_table(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    table: dbrover_export::SqlTable,
    path: &std::path::Path,
) -> (Vec<usize>, Result<usize, String>) {
    engine.send(Command::ExportTable {
        data_source,
        export: dbrover_engine::TableExport {
            run: RunId(900),
            table,
            path: path.to_owned(),
            spec: dbrover_export::ExportSpec::Csv(Default::default()),
        },
    });
    let mut progress = Vec::new();
    loop {
        match engine.next_event() {
            Event::ExportProgress { rows, .. } => progress.push(rows),
            Event::ExportFinished { result, .. } => return (progress, result),
            _ => {}
        }
    }
}

/// A table export stops when cancelled, leaving no file, and runs on a read-only data source,
/// since it only reads.
#[test]
fn cancels_a_table_export_and_runs_one_on_a_read_only_data_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let file = sqlite_database(dir.path());
    let data_source = DataSource {
        safety: dbrover_core::SafetyLevel::ReadOnly,
        ..sqlite_data_source(file.clone())
    };
    let id = data_source.id;
    let database = rusqlite::Connection::open(&file).unwrap();
    database
        .execute_batch(
            "CREATE TABLE big (n INTEGER);
             WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 100000)
             INSERT INTO big SELECT n FROM r;",
        )
        .unwrap();
    drop(database);
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    let table = dbrover_export::SqlTable {
        schema: None,
        name: "big".to_owned(),
    };
    let path = dir.path().join("big.csv");

    engine.send(Command::ExportTable {
        data_source: id,
        export: dbrover_engine::TableExport {
            run: RunId(1),
            table: table.clone(),
            path: path.clone(),
            spec: dbrover_export::ExportSpec::Csv(Default::default()),
        },
    });
    loop {
        if let Event::ExportProgress { .. } = engine.next_event() {
            break;
        }
    }
    engine.send(Command::CancelTableExport {
        data_source: id,
        run: RunId(1),
    });
    loop {
        if let Event::ExportFinished { result, .. } = engine.next_event() {
            assert_eq!(result, Err("Export cancelled".to_owned()));
            break;
        }
    }
    assert!(!path.exists());

    let (_, written) = export_table(&mut engine, id, table, &path);
    assert_eq!(written, Ok(100_000));
    engine.shut_down();
}

/// Cancelling stops the statement and removes the partial file; a failing statement leaves no
/// file either; and a read-only data source refuses to export a statement that writes.
#[test]
fn cancels_fails_and_refuses_exports_without_leaving_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(&engine, id, 1, &["SELECT 1"]);
    finish_run(&mut engine, 1);
    let path = dir.path().join("rows.csv");

    engine.send(Command::ExportAllRows {
        console: CONSOLE,
        export: dbrover_engine::ExportAll {
            run: RunId(2),
            sql: "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT n FROM r"
                .to_owned(),
            parameters: dbrover_sql::ParameterValues::default(),
            path: path.clone(),
            spec: dbrover_export::ExportSpec::Csv(Default::default()),
        },
    });
    match next_run_event(&mut engine) {
        Event::ExportProgress { .. } => {}
        other => panic!("expected progress, got {other:?}"),
    }
    engine.send(Command::Cancel {
        console: CONSOLE,
        run: RunId(2),
    });
    loop {
        match next_run_event(&mut engine) {
            Event::ExportProgress { .. } => {}
            Event::ExportFinished { result, .. } => {
                assert_eq!(result, Err("Export cancelled".to_owned()));
                break;
            }
            other => panic!("expected the export to end, got {other:?}"),
        }
    }
    let (_, failed) = export_all(
        &mut engine,
        3,
        "SELECT * FROM no_such_table",
        (&path, dbrover_export::ExportSpec::Csv(Default::default())),
    );
    assert!(failed.unwrap_err().contains("no_such_table"));
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("rows"))
        .collect();
    assert!(files.is_empty(), "{files:?}");
    engine.shut_down();

    let read_only_dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(
        &read_only_dir.path().join("data_sources.toml"),
        Arc::default(),
    );
    engine.expect_started(vec![]);
    let data_source = DataSource {
        safety: dbrover_core::SafetyLevel::ReadOnly,
        ..sqlite_data_source(sqlite_database(read_only_dir.path()))
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    execute(&engine, id, 1, &["SELECT 1"]);
    finish_run(&mut engine, 1);
    let (_, refused) = export_all(
        &mut engine,
        2,
        "DELETE FROM t RETURNING id",
        (&path, dbrover_export::ExportSpec::Csv(Default::default())),
    );
    assert!(refused.unwrap_err().contains("read-only"));
    engine.shut_down();
}

/// Imports a CSV file into a table, as the import dialog asks, and returns the progress
/// reported and the outcome.
pub(super) fn import_rows(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    import: dbrover_engine::RowsImport,
) -> (Vec<u64>, Result<u64, dbrover_engine::ImportFailure>) {
    engine.send(Command::ImportRows {
        data_source,
        import,
    });
    let mut progress = Vec::new();
    loop {
        match next_answer(engine) {
            Event::ImportProgress { rows, .. } => progress.push(rows),
            Event::RowsImported { result, .. } => return (progress, result),
            Event::TransactionChanged { .. } => {}
            other => panic!("expected the import's progress, got {other:?}"),
        }
    }
}

/// A CSV file with a header and `count` rows from `first` on, with one more row repeating the
/// first id at `repeat_at` (a line of the file) if given.
fn csv_file(path: &std::path::Path, first: u32, count: u32, repeat_at: Option<usize>) {
    let mut text = "ID,Name,Qty\r\n".to_owned();
    for id in first..first + count {
        let line = text.lines().count() + 1;
        if Some(line) == repeat_at {
            text.push_str(&format!("{first},again,1\r\n"));
        }
        let qty = if id % 2 == 0 {
            String::new()
        } else {
            id.to_string()
        };
        text.push_str(&format!("{id},\"name, {id}\",{qty}\r\n"));
    }
    std::fs::write(path, text).unwrap();
}

/// Rows from a CSV file in batches, all in one transaction: the columns mapped from the file,
/// an empty field as NULL, an unmapped column at its default. A repeated key rolls the whole
/// import back and names its line, found by retrying the batch a row at a time; DuckDB, with no
/// savepoints, names the batch's lines.
#[test]
fn imports_a_csv_file_all_or_none_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let table = scratch_table("imported");
    let (good, bad) = (dir.path().join("good.csv"), dir.path().join("bad.csv"));
    csv_file(&good, 1, 1200, None);
    csv_file(&bad, 2000, 1000, Some(703));
    for source in test_sources(&mut engine, dir.path()) {
        run_ok(
            &mut engine,
            &source,
            1,
            &[&format!(
                "CREATE TABLE {table} (id int NOT NULL PRIMARY KEY, name varchar(40) NOT NULL, \
                 qty int, note varchar(40) DEFAULT 'none')"
            )],
        );
        let described = describe(&mut engine, &source, &table);
        let import = |file: &std::path::Path| dbrover_engine::RowsImport {
            table: described.table_name(),
            file: file.to_owned(),
            separator: dbrover_store::Separator::Comma,
            header: true,
            mapping: vec![
                ("id".to_owned(), 0),
                ("name".to_owned(), 1),
                ("qty".to_owned(), 2),
            ],
        };

        let (progress, imported) = import_rows(&mut engine, source.id, import(&good));
        assert_eq!(imported, Ok(1200), "{}", source.name);
        assert_eq!(progress, [500, 1000, 1200], "{}", source.name);
        let summary = format!(
            "SELECT count(*), count(qty), min(note), max(note) FROM {table} WHERE name LIKE 'name, %'"
        );
        let expected = vec![vec![
            Some("1200".to_owned()),
            Some("600".to_owned()),
            Some("none".to_owned()),
            Some("none".to_owned()),
        ]];
        assert_eq!(
            query(&mut engine, &source, 2, &summary),
            expected,
            "{}",
            source.name
        );

        let (_, failed) = import_rows(&mut engine, source.id, import(&bad));
        let failure = failed.unwrap_err();
        let lines = match source.dialect {
            Dialect::DuckDb => Some((502, 1001)),
            _ => Some((703, 703)),
        };
        assert_eq!(failure.lines, lines, "{}: {failure:?}", source.name);
        let count = format!("SELECT count(*) FROM {table}");
        assert_eq!(
            query(&mut engine, &source, 3, &count),
            [[Some("1200".to_owned())]],
            "{}: nothing of the failed import stays",
            source.name
        );
        run_ok(&mut engine, &source, 4, &[&format!("DROP TABLE {table}")]);
    }
    engine.shut_down();
}

/// A line with too few fields stops the import at that line; a read-only data source and
/// ClickHouse refuse to import at all.
#[test]
fn refuses_imports_it_cant_undo_or_complete() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    let file = dir.path().join("short.csv");
    std::fs::write(&file, "1,a\n2\n").unwrap();
    let import = |table: dbrover_core::TableName| dbrover_engine::RowsImport {
        table,
        file: file.clone(),
        separator: dbrover_store::Separator::Comma,
        header: false,
        mapping: vec![("id".to_owned(), 0), ("name".to_owned(), 1)],
    };
    let people = dbrover_core::TableName {
        schema: "main".to_owned(),
        name: "t".to_owned(),
    };
    let (_, short) = import_rows(&mut engine, id, import(people.clone()));
    let failure = short.unwrap_err();
    assert_eq!(failure.lines, Some((2, 2)), "{failure:?}");
    assert!(failure.message.contains("1 fields"), "{failure:?}");

    if let Some(clickhouse) = clickhouse_source(&mut engine) {
        let (_, refused) = import_rows(&mut engine, clickhouse.id, import(people.clone()));
        assert!(refused.unwrap_err().message.contains("no transactions"));
    }
    engine.shut_down();

    let read_only_dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(
        &read_only_dir.path().join("data_sources.toml"),
        Arc::default(),
    );
    engine.expect_started(vec![]);
    let data_source = DataSource {
        safety: dbrover_core::SafetyLevel::ReadOnly,
        ..sqlite_data_source(sqlite_database(read_only_dir.path()))
    };
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    let (_, refused) = import_rows(&mut engine, id, import(people));
    assert!(refused.unwrap_err().message.contains("read-only"));
    engine.shut_down();
}
