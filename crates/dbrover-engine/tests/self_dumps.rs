//! SQLite and DuckDB dump and restore themselves (W17g): a consistent copy of a SQLite file
//! with `VACUUM INTO`, and DuckDB's `EXPORT DATABASE` read back by `IMPORT DATABASE`, each
//! restored into a new file with the same rows. No program and no server, so these run on
//! every system.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, sqlite_data_source};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    DumpFormat, DumpOptions, DumpRequest, DumpScope, RestoreOptions, RestoreRequest, RestoreTarget,
    SafetyLevel, ServerInfo,
};
use dbrover_engine::{
    Command, DumpDone, DumpFailure, DumpRun, DumpRunId, Event, PasswordUpdate, ProgramPaths,
    RestoreDone, RestoreRun,
};

fn until<T>(engine: &mut TestEngine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(engine.next_event()) {
            return found;
        }
    }
}

fn start(data_source: &DataSource, dir: &Path) -> TestEngine {
    let mut engine = TestEngine::start(
        &dir.join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    engine
}

fn dump(
    engine: &mut TestEngine,
    data_source: &DataSource,
    server: &ServerInfo,
    format: DumpFormat,
    file: PathBuf,
) -> Result<DumpDone, DumpFailure> {
    engine.send(Command::StartDump(Box::new(DumpRun {
        run: DumpRunId(1),
        request: DumpRequest {
            data_source: data_source.id,
            database: "main".to_owned(),
            scope: DumpScope::Database,
            format,
            options: DumpOptions::default(),
            file,
        },
        server: server.clone(),
        programs: ProgramPaths::default(),
    })));
    until(engine, |event| match event {
        Event::DumpEnded { result, .. } => Some(result),
        _ => None,
    })
}

fn restore(
    engine: &mut TestEngine,
    data_source: &DataSource,
    server: &ServerInfo,
    file: PathBuf,
    into: &Path,
) -> Result<RestoreDone, DumpFailure> {
    engine.send(Command::StartRestore(Box::new(RestoreRun {
        run: DumpRunId(2),
        request: RestoreRequest {
            data_source: data_source.id,
            file,
            target: RestoreTarget::NewDatabase(into.display().to_string()),
            tables: None,
            options: RestoreOptions::default(),
        },
        server: server.clone(),
        programs: ProgramPaths::default(),
    })));
    until(engine, |event| match event {
        Event::RestoreEnded { result, .. } => Some(result),
        _ => None,
    })
}

fn first_value(
    runtime: &tokio::runtime::Runtime,
    connection: &mut dyn Connection,
    sql: &str,
) -> Option<String> {
    runtime.block_on(async {
        let mut cursor = connection.execute(sql).await.unwrap();
        let rows = cursor.next_page(1).await.unwrap();
        cursor.finish().await.unwrap();
        rows.first().and_then(|row| row.first().cloned().flatten())
    })
}

#[test]
fn copies_a_sqlite_database_and_restores_it_into_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("shop.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO customers VALUES (1, 'Kate O''Brien'), (2, 'Zoë');",
        )
        .unwrap();
    let data_source = sqlite_data_source(database);
    let server = ServerInfo::new("SQLite", "3.46.0");
    let mut engine = start(&data_source, dir.path());

    let file = dir.path().join("dumps").join("shop.sqlite");
    let done = dump(
        &mut engine,
        &data_source,
        &server,
        DumpFormat::DatabaseFile,
        file.clone(),
    )
    .unwrap();
    assert_eq!(done.file, file);
    assert!(!dir.path().join("dumps/shop.sqlite.partial").exists());

    let restored_file = dir.path().join("restored.db");
    restore(&mut engine, &data_source, &server, file, &restored_file).unwrap();
    let names: String = rusqlite::Connection::open(&restored_file)
        .unwrap()
        .query_row(
            "SELECT group_concat(name, ',') FROM customers ORDER BY id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(names, "Kate O'Brien,Zoë");

    // A restore never writes over a file.
    let again = restore(
        &mut engine,
        &data_source,
        &server,
        dir.path().join("dumps/shop.sqlite"),
        &restored_file,
    );
    assert_eq!(again, Err(DumpFailure::Exists(restored_file)));
    engine.shut_down();
}

#[test]
fn exports_a_duckdb_database_and_imports_it_into_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let database = dir.path().join("lake.duckdb");
    runtime
        .block_on(dbrover_driver_duckdb::create_database(&database))
        .unwrap();
    let config = DuckDbConfig::new(DuckDbDatabase::File {
        path: database.clone(),
        read_only: false,
    });
    {
        let mut connection = runtime
            .block_on(dbrover_driver_duckdb::connect(&config))
            .unwrap();
        for sql in [
            "CREATE TABLE trips (id INTEGER PRIMARY KEY, city VARCHAR, fare DECIMAL(8, 2))",
            "INSERT INTO trips VALUES (1, 'Kraków', 12.50), (2, 'Lyon', 7.25)",
        ] {
            first_value(&runtime, &mut connection, sql);
        }
    }
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "lake".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::DuckDb(config),
    };
    let server = ServerInfo::new("DuckDB", "1.4.0");
    let mut engine = start(&data_source, dir.path());

    for (format, name) in [
        (DumpFormat::Parquet, "lake-parquet"),
        (DumpFormat::Csv, "lake-csv"),
    ] {
        let folder = dir.path().join(name);
        dump(&mut engine, &data_source, &server, format, folder.clone()).unwrap();
        assert!(folder.join("load.sql").is_file(), "{format:?}");

        let restored_file = dir.path().join(format!("{name}.duckdb"));
        restore(&mut engine, &data_source, &server, folder, &restored_file).unwrap();
        let restored = DuckDbConfig::new(DuckDbDatabase::File {
            path: restored_file,
            read_only: true,
        });
        let mut connection = runtime
            .block_on(dbrover_driver_duckdb::connect(&restored))
            .unwrap();
        assert_eq!(
            first_value(
                &runtime,
                &mut connection,
                "SELECT string_agg(city || ':' || fare, ',' ORDER BY id) FROM trips"
            )
            .as_deref(),
            Some("Kraków:12.50,Lyon:7.25"),
            "{format:?}"
        );
    }
    engine.shut_down();
}
