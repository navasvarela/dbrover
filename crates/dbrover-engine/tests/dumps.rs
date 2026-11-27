//! Dumping and restoring a database with the engine's own programs (W17): the program chosen
//! for the server's version, its password in a file of its own, a `.partial` file renamed once
//! it has ended well, Cancel, a restore into a new database, all or nothing, and one history
//! entry per dump or restore.
//!
//! The PostgreSQL tests are skipped unless `DBROVER_TEST_POSTGRES_URL` is set and
//! `DBROVER_TEST_CLIENTS` names a folder with `pg_dump` and `pg_restore` in it: on CI the
//! scripts in docker/clients, which run them from the server's image; locally, wherever they
//! are installed (`/opt/homebrew/bin`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, ParsedUrl, TestEngine, postgres_test_server, ssh_test_server};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DumpFormat, DumpOptions, DumpRequest,
    DumpScope, Password, PostgresConfig, RestoreOptions, RestoreRequest, RestoreTarget, Route,
    SafetyLevel, ServerConfig, ServerInfo, SshLogin, TableName,
};
use dbrover_dump::Program;
use dbrover_engine::{
    Command, DumpDone, DumpFailure, DumpRun, DumpRunId, DumpStart, Event, PasswordUpdate,
    ProgramPaths, RestoreDone, RestoreRun,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistoryQuery, HistorySource, HistorySourceKind};

/// The folder `DBROVER_TEST_CLIENTS` names, or `None`, with a note, so the test skips.
fn clients() -> Option<PathBuf> {
    let Ok(folder) = std::env::var("DBROVER_TEST_CLIENTS") else {
        eprintln!("skipping: DBROVER_TEST_CLIENTS is not set");
        return None;
    };
    let folder = PathBuf::from(folder);
    Some(if folder.is_absolute() {
        folder
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(folder)
    })
}

fn program_paths(clients: &Path) -> ProgramPaths {
    ProgramPaths(
        [Program::PgDump, Program::PgRestore, Program::Psql]
            .into_iter()
            .map(|program| (program, clients.join(program.name())))
            .collect(),
    )
}

/// How many databases a test may restore into, named after the fixture's with `_r1` and on,
/// all dropped with it.
const RESTORED_DATABASES: u32 = 6;

/// A password every layer has to pass on as it is: a `.pgpass` separator, a backslash, a
/// comment sign, quotes and a space.
const AWKWARD_PASSWORD: &str = r#"pa:ss\wo#rd "q" 'x'"#;

/// A database and a login of the test's own on the PostgreSQL test server, dropped when it
/// ends, so it races no other test's catalog.
struct Fixture {
    runtime: tokio::runtime::Runtime,
    admin: Box<dyn Connection>,
    database: String,
    role: String,
    server: ServerInfo,
    config: PostgresConfig,
}

impl Fixture {
    fn create(server: &ParsedUrl, name: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut admin = connect(&runtime, &server.config, &server.password);
        let suffix = format!("{name}_{}", std::process::id());
        let database = format!("w17_{suffix}");
        let role = format!("w17_role_{suffix}");
        let password = AWKWARD_PASSWORD.replace('\'', "''");
        execute(
            &runtime,
            admin.as_mut(),
            &format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"),
        );
        execute(
            &runtime,
            admin.as_mut(),
            &format!("DROP ROLE IF EXISTS {role}"),
        );
        execute(
            &runtime,
            admin.as_mut(),
            &format!("CREATE ROLE {role} LOGIN CREATEDB PASSWORD '{password}'"),
        );
        execute(
            &runtime,
            admin.as_mut(),
            &format!("CREATE DATABASE {database} OWNER {role}"),
        );
        let config = PostgresConfig {
            server: ServerConfig {
                database: database.clone(),
                user: role.clone(),
                ..server.config.server.clone()
            },
        };
        let mut owner = connect(&runtime, &config, &Password::new(AWKWARD_PASSWORD));
        let server_info = owner.server_info().clone();
        execute(
            &runtime,
            owner.as_mut(),
            "CREATE TABLE customers (id integer PRIMARY KEY, name text);
             INSERT INTO customers VALUES (1, 'Kate O''Brien'), (2, 'Liam Hale');
             CREATE TABLE \"Order Items\" (id integer PRIMARY KEY, sku text);
             INSERT INTO \"Order Items\" VALUES (7, 'ORD-7731-K');",
        );
        Self {
            runtime,
            admin,
            database,
            role,
            server: server_info,
            config,
        }
    }

    fn data_source(&self, config: PostgresConfig) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Postgres(config),
        }
    }

    /// A connection of the test's own, as the fixture's login, to lock a table with.
    fn owner(&self) -> Box<dyn Connection> {
        connect(
            &self.runtime,
            &self.config,
            &Password::new(AWKWARD_PASSWORD),
        )
    }

    fn execute(&self, connection: &mut dyn Connection, sql: &str) {
        execute(&self.runtime, connection, sql);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let database = &self.database;
        let role = &self.role;
        self.runtime.block_on(async {
            let restored = (1..=RESTORED_DATABASES)
                .map(|n| format!("DROP DATABASE IF EXISTS {database}_r{n} WITH (FORCE)"));
            let fixture = [
                format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"),
                format!("DROP ROLE IF EXISTS {role}"),
            ];
            for sql in restored.chain(fixture) {
                if let Ok(cursor) = self.admin.execute(&sql).await {
                    let _ = cursor.finish().await;
                }
            }
        });
    }
}

fn connect(
    runtime: &tokio::runtime::Runtime,
    config: &PostgresConfig,
    password: &Password,
) -> Box<dyn Connection> {
    Box::new(
        runtime
            .block_on(dbrover_driver_postgres::connect(
                config,
                Some(password),
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

/// An engine with `data_source` saved, with the fixture's password.
fn start_engine(data_source: &DataSource) -> (TestEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(
        &dir.path().join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Set(Password::new(AWKWARD_PASSWORD)),
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

fn started(engine: &mut TestEngine, run: DumpRunId) -> DumpStart {
    until(engine, |event| match event {
        Event::DumpStarted {
            run: started,
            start,
        } if started == run => Some(start),
        Event::DumpEnded { run: ended, result } if ended == run => {
            panic!("the dump ended before its program started: {result:?}")
        }
        _ => None,
    })
}

fn ended(engine: &mut TestEngine, run: DumpRunId) -> Result<DumpDone, DumpFailure> {
    until(engine, |event| match event {
        Event::DumpEnded { run: ended, result } if ended == run => Some(result),
        _ => None,
    })
}

fn dumps(engine: &mut TestEngine, data_source: DataSourceId) -> Vec<HistoryEntry> {
    engine.send(Command::SearchHistory(HistoryQuery {
        kind: Some(HistorySourceKind::Dump),
        ..HistoryQuery::all(data_source)
    }));
    until(engine, |event| match event {
        Event::HistoryFound { result, .. } => Some(result.unwrap()),
        _ => None,
    })
}

fn dump_run(
    run: u64,
    data_source: &DataSource,
    fixture: &Fixture,
    scope: DumpScope,
    format: DumpFormat,
    file: PathBuf,
    programs: ProgramPaths,
) -> Box<DumpRun> {
    Box::new(DumpRun {
        run: DumpRunId(run),
        request: DumpRequest {
            data_source: data_source.id,
            database: fixture.database.clone(),
            scope,
            format,
            options: DumpOptions::default(),
            file,
        },
        server: fixture.server.clone(),
        programs,
    })
}

/// What `pg_restore --list` says an archive holds.
fn archive_contents(clients: &Path, file: &Path) -> String {
    let output = std::process::Command::new(clients.join("pg_restore"))
        .arg("--list")
        .arg(file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn files_in(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A whole database dumped in the custom format, by the program DbRover was given, with the
/// password in a file: never on its command line, never left behind; History lists it once.
#[test]
fn dumps_a_database_with_no_password_on_its_command_line() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "custom");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let file = dir.path().join("shop.dump");

    engine.send(Command::StartDump(dump_run(
        1,
        &data_source,
        &fixture,
        DumpScope::Database,
        DumpFormat::Custom,
        file.clone(),
        program_paths(&clients),
    )));
    let start = started(&mut engine, DumpRunId(1));
    assert_eq!(start.program, clients.join("pg_dump"));
    assert!(
        start.command.contains("--format=custom"),
        "{}",
        start.command
    );
    assert!(!start.command.contains("wo#rd"), "{}", start.command);
    let done = ended(&mut engine, DumpRunId(1)).unwrap();
    assert_eq!(done.file, file);
    assert!(done.bytes > 0);

    let contents = archive_contents(&clients, &file);
    assert!(contents.contains("TABLE public customers"), "{contents}");
    assert!(contents.contains("TABLE public Order Items"), "{contents}");
    assert_eq!(
        files_in(dir.path())
            .iter()
            .filter(|name| name.ends_with(".partial"))
            .count(),
        0
    );
    assert!(
        files_in(&dir.path().join("dump-runs")).is_empty(),
        "the password file is gone"
    );

    let entries = dumps(&mut engine, data_source.id);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, HistoryOutcome::Ok);
    assert_eq!(entries[0].sql, start.command);
    let HistorySource::Dump { dumped } = &entries[0].source else {
        panic!("{:?}", entries[0].source);
    };
    assert!(
        dumped.starts_with(&format!("Dumped {} (custom)", fixture.database)),
        "{dumped}"
    );
    engine.shut_down();
}

/// One table, as gzipped SQL: only that table, its odd name quoted.
#[test]
fn dumps_a_chosen_table_as_gzipped_sql() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "gzip");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let file = dir.path().join("items.sql.gz");
    let items = DumpScope::Tables(vec![TableName {
        schema: "public".to_owned(),
        name: "Order Items".to_owned(),
    }]);
    engine.send(Command::StartDump(dump_run(
        2,
        &data_source,
        &fixture,
        items,
        DumpFormat::SqlGz,
        file.clone(),
        program_paths(&clients),
    )));
    ended(&mut engine, DumpRunId(2)).unwrap();

    let mut sql = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&file).unwrap())
        .read_to_string(&mut sql)
        .unwrap();
    assert!(sql.contains("CREATE TABLE public.\"Order Items\""), "{sql}");
    assert!(sql.contains("ORD-7731-K"), "{sql}");
    assert!(!sql.contains("CREATE TABLE public.customers"), "{sql}");
    engine.shut_down();
}

/// A dump waiting on a table another session holds locked is cancelled: its program stops,
/// nothing is left where the dump was going, and History lists it as cancelled.
#[test]
fn cancel_stops_a_dump_and_leaves_no_file() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "cancel");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let mut locker = fixture.owner();
    fixture.execute(locker.as_mut(), "BEGIN");
    fixture.execute(
        locker.as_mut(),
        "LOCK TABLE customers IN ACCESS EXCLUSIVE MODE",
    );

    let file = dir.path().join("locked.dump");
    engine.send(Command::StartDump(dump_run(
        3,
        &data_source,
        &fixture,
        DumpScope::Database,
        DumpFormat::Custom,
        file.clone(),
        program_paths(&clients),
    )));
    started(&mut engine, DumpRunId(3));
    engine.send(Command::CancelDump(DumpRunId(3)));
    assert_eq!(
        ended(&mut engine, DumpRunId(3)),
        Err(DumpFailure::Cancelled)
    );
    fixture.execute(locker.as_mut(), "ROLLBACK");

    assert!(!file.exists());
    assert!(!dir.path().join("locked.dump.partial").exists());
    let entries = dumps(&mut engine, data_source.id);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, HistoryOutcome::Cancelled);
    engine.shut_down();
}

/// Through the data source's SSH tunnel: the program connects to the tunnel's local port.
#[test]
fn dumps_through_an_ssh_tunnel() {
    let (Some(ssh), Some(server), Some(clients)) =
        (ssh_test_server(), postgres_test_server(), clients())
    else {
        return;
    };
    let fixture = Fixture::create(&server, "ssh");
    let through_ssh = PostgresConfig {
        server: ServerConfig {
            ssh: Some(ssh.ssh(SshLogin::Password)),
            database: fixture.database.clone(),
            user: fixture.role.clone(),
            ..ssh.postgres.config.server.clone()
        },
    };
    let data_source = fixture.data_source(through_ssh);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("known_hosts"), ssh.trusted_line()).unwrap();
    let mut engine = TestEngine::start(
        &dir.path().join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Set(Password::new(AWKWARD_PASSWORD)),
        ssh_secret: PasswordUpdate::Set(ssh.password.clone()),
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));

    let file = dir.path().join("tunnelled.dump");
    engine.send(Command::StartDump(dump_run(
        4,
        &data_source,
        &fixture,
        DumpScope::Database,
        DumpFormat::Custom,
        file.clone(),
        program_paths(&clients),
    )));
    let start = started(&mut engine, DumpRunId(4));
    assert!(
        start.command.contains("hostaddr=127.0.0.1"),
        "{}",
        start.command
    );
    ended(&mut engine, DumpRunId(4)).unwrap();
    assert!(archive_contents(&clients, &file).contains("TABLE public customers"));
    engine.shut_down();
}

fn restored(engine: &mut TestEngine, run: DumpRunId) -> Result<RestoreDone, DumpFailure> {
    until(engine, |event| match event {
        Event::RestoreEnded { run: ended, result } if ended == run => Some(result),
        _ => None,
    })
}

fn restore_run(
    run: u64,
    data_source: &DataSource,
    fixture: &Fixture,
    file: PathBuf,
    target: RestoreTarget,
    programs: ProgramPaths,
) -> Box<RestoreRun> {
    Box::new(RestoreRun {
        run: DumpRunId(run),
        request: RestoreRequest {
            data_source: data_source.id,
            file,
            target,
            tables: None,
            options: RestoreOptions::default(),
        },
        server: fixture.server.clone(),
        programs,
    })
}

/// What `sql` gives in `database`, one row a line, read by `psql` as the test server's admin.
fn query(clients: &Path, server: &ParsedUrl, database: &str, sql: &str) -> String {
    let url = std::env::var("DBROVER_TEST_POSTGRES_URL").unwrap();
    let base = &url[..url.rfind('/').unwrap()];
    let _ = server;
    let output = std::process::Command::new(clients.join("psql"))
        .arg(format!("--dbname={base}/{database}"))
        .args([
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "--command",
            sql,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

const ROWS: &str = "SELECT string_agg(name, ',' ORDER BY id) FROM customers \
                    UNION ALL SELECT string_agg(sku, ',') FROM \"Order Items\"";

/// Each format dumped, then restored into a new database the restore creates: the same rows
/// come back, owned by the login that restored them. The archive is read first.
#[test]
fn restores_each_format_into_a_new_database() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "restore");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let formats = [
        (DumpFormat::Custom, "shop.dump"),
        (DumpFormat::Directory, "shop-folder"),
        (DumpFormat::Sql, "shop.sql"),
        (DumpFormat::SqlGz, "shop.sql.gz"),
    ];
    let original = query(&clients, &server, &fixture.database, ROWS);
    assert_eq!(original, "Kate O'Brien,Liam Hale\nORD-7731-K");
    for (n, (format, name)) in (1_u64..).zip(formats) {
        let file = dir.path().join(name);
        engine.send(Command::StartDump(dump_run(
            10 + n,
            &data_source,
            &fixture,
            DumpScope::Database,
            format,
            file.clone(),
            program_paths(&clients),
        )));
        ended(&mut engine, DumpRunId(10 + n)).unwrap();

        if format == DumpFormat::Custom {
            engine.send(Command::InspectDumpFile {
                file: file.clone(),
                programs: program_paths(&clients),
            });
            let found = until(&mut engine, |event| match event {
                Event::DumpFileInspected { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
            let contents = found.contents.unwrap();
            assert_eq!(
                contents.database.as_deref(),
                Some(fixture.database.as_str())
            );
            assert_eq!(contents.tables.len(), 2, "{contents:?}");
            assert!(contents.made_by.is_some());
        }

        let database = format!("{}_r{n}", fixture.database);
        engine.send(Command::StartRestore(restore_run(
            20 + n,
            &data_source,
            &fixture,
            file,
            RestoreTarget::NewDatabase(database.clone()),
            program_paths(&clients),
        )));
        let done = restored(&mut engine, DumpRunId(20 + n)).unwrap();
        assert_eq!(done.database, database);
        assert!(done.created);
        assert_eq!(
            query(&clients, &server, &database, ROWS),
            original,
            "{format:?}"
        );
    }
    engine.send(Command::SearchHistory(HistoryQuery {
        kind: Some(HistorySourceKind::Restore),
        ..HistoryQuery::all(data_source.id)
    }));
    let restores = until(&mut engine, |event| match event {
        Event::HistoryFound { result, .. } => Some(result.unwrap()),
        _ => None,
    });
    assert_eq!(restores.len(), 4);
    assert!(
        restores
            .iter()
            .all(|entry| entry.outcome == HistoryOutcome::Ok)
    );
    engine.shut_down();
}

/// Some tables of an archive only.
#[test]
fn restores_chosen_tables_of_an_archive() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "chosen");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let file = dir.path().join("shop.dump");
    engine.send(Command::StartDump(dump_run(
        1,
        &data_source,
        &fixture,
        DumpScope::Database,
        DumpFormat::Custom,
        file.clone(),
        program_paths(&clients),
    )));
    ended(&mut engine, DumpRunId(1)).unwrap();
    let database = format!("{}_r1", fixture.database);
    let mut run = restore_run(
        2,
        &data_source,
        &fixture,
        file,
        RestoreTarget::NewDatabase(database.clone()),
        program_paths(&clients),
    );
    run.request.tables = Some(vec![TableName {
        schema: "public".to_owned(),
        name: "customers".to_owned(),
    }]);
    engine.send(Command::StartRestore(run));
    restored(&mut engine, DumpRunId(2)).unwrap();
    let tables = query(
        &clients,
        &server,
        &database,
        "SELECT string_agg(relname, ',') FROM pg_class WHERE relkind = 'r' AND relnamespace = 'public'::regnamespace",
    );
    assert_eq!(tables, "customers");
    engine.shut_down();
}

/// A plain dump that fails half way, restored all or nothing, leaves nothing behind it.
#[test]
fn a_failing_restore_all_or_nothing_leaves_the_database_empty() {
    let (Some(server), Some(clients)) = (postgres_test_server(), clients()) else {
        return;
    };
    let fixture = Fixture::create(&server, "failing");
    let data_source = fixture.data_source(fixture.config.clone());
    let (mut engine, dir) = start_engine(&data_source);
    let file = dir.path().join("broken.sql");
    std::fs::write(
        &file,
        "--\n-- PostgreSQL database dump\n--\n\
         CREATE TABLE half (id integer);\n\
         INSERT INTO half VALUES (1);\n\
         SELECT 1 / 0;\n",
    )
    .unwrap();
    let database = format!("{}_r1", fixture.database);
    engine.send(Command::StartRestore(restore_run(
        1,
        &data_source,
        &fixture,
        file,
        RestoreTarget::NewDatabase(database.clone()),
        program_paths(&clients),
    )));
    let failure = restored(&mut engine, DumpRunId(1)).unwrap_err();
    let DumpFailure::Exited {
        program, stderr, ..
    } = &failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(program, "psql");
    assert!(stderr.contains("division by zero"), "{stderr}");
    let tables = query(
        &clients,
        &server,
        &database,
        "SELECT count(*) FROM pg_class WHERE relname = 'half'",
    );
    assert_eq!(tables, "0");
    engine.shut_down();
}

/// Nothing is restored into a read-only data source, and a dump for another engine is
/// refused before anything is created.
#[test]
fn refuses_a_read_only_data_source_and_another_engines_dump() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mysql.sql");
    std::fs::write(
        &file,
        "-- MySQL dump 10.13  Distrib 8.4.3, for Linux (x86_64)\n",
    )
    .unwrap();
    for (safety, expected) in [
        (
            SafetyLevel::ReadOnly,
            DumpFailure::ReadOnly("never reached".to_owned()),
        ),
        (
            SafetyLevel::Normal,
            DumpFailure::Arguments(dbrover_dump::ArgumentError::OtherEngine {
                file: dbrover_core::DumpEngine::Mysql,
                server: dbrover_core::DumpEngine::Postgres,
            }),
        ),
    ] {
        let mut data_source = unreachable_source();
        data_source.safety = safety;
        let data_dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(
            &data_dir.path().join("data_sources.toml"),
            Arc::new(MemorySecrets::default()),
        );
        engine.expect_started(vec![]);
        engine.send(Command::SaveDataSource {
            data_source: data_source.clone(),
            password: PasswordUpdate::Keep,
            ssh_secret: PasswordUpdate::Keep,
        });
        engine.expect(Event::DataSourceSaved(data_source.clone()));
        engine.send(Command::StartRestore(Box::new(RestoreRun {
            run: DumpRunId(1),
            request: RestoreRequest {
                data_source: data_source.id,
                file: file.clone(),
                target: RestoreTarget::NewDatabase("shop_restored".to_owned()),
                tables: None,
                options: RestoreOptions::default(),
            },
            server: ServerInfo::new("PostgreSQL", "17.4"),
            programs: ProgramPaths::default(),
        })));
        assert_eq!(restored(&mut engine, DumpRunId(1)), Err(expected));
        engine.shut_down();
    }
}

/// A data source the refusals in these tests are decided before reaching.
fn unreachable_source() -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "never reached".to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_owned(),
                port: 5432.into(),
                database: "shop".to_owned(),
                user: "ada".to_owned(),
                tls: dbrover_core::TlsMode::Disable,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        }),
    }
}

#[cfg(unix)]
mod without_a_server {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A program that only says it is `pg_dump` of `version`.
    fn fake_pg_dump(folder: &Path, version: &str) -> ProgramPaths {
        let path = folder.join("pg_dump");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho 'pg_dump (PostgreSQL) {version}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        ProgramPaths([(Program::PgDump, path)].into_iter().collect())
    }

    fn run_on(
        data_source: &DataSource,
        file: PathBuf,
        server_version: &str,
        programs: ProgramPaths,
    ) -> Box<DumpRun> {
        Box::new(DumpRun {
            run: DumpRunId(9),
            request: DumpRequest {
                data_source: data_source.id,
                database: "shop".to_owned(),
                scope: DumpScope::Database,
                format: DumpFormat::Custom,
                options: DumpOptions::default(),
                file,
            },
            server: ServerInfo::new("PostgreSQL", server_version),
            programs,
        })
    }

    fn engine_with(data_source: &DataSource) -> (TestEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(
            &dir.path().join("data_sources.toml"),
            Arc::new(MemorySecrets::default()),
        );
        engine.expect_started(vec![]);
        engine.send(Command::SaveDataSource {
            data_source: data_source.clone(),
            password: PasswordUpdate::Set(Password::new("secret")),
            ssh_secret: PasswordUpdate::Keep,
        });
        engine.expect(Event::DataSourceSaved(data_source.clone()));
        (engine, dir)
    }

    #[test]
    fn refuses_a_pg_dump_older_than_the_server() {
        let data_source = unreachable_source();
        let (mut engine, dir) = engine_with(&data_source);
        let programs = fake_pg_dump(dir.path(), "16.4");
        engine.send(Command::StartDump(run_on(
            &data_source,
            dir.path().join("shop.dump"),
            "17.4 (Debian 17.4-1.pgdg120+2)",
            programs,
        )));
        let Err(DumpFailure::NoProgram(no_fit)) = ended(&mut engine, DumpRunId(9)) else {
            panic!("the dump wasn't refused");
        };
        assert_eq!(no_fit.reason, "pg_dump 16.4 can't dump PostgreSQL 17.4.");
        assert!(dumps(&mut engine, data_source.id).is_empty(), "nothing ran");
        engine.shut_down();
    }

    #[test]
    fn never_writes_over_a_file() {
        let data_source = unreachable_source();
        let (mut engine, dir) = engine_with(&data_source);
        let file = dir.path().join("shop.dump");
        std::fs::write(&file, "yesterday's dump").unwrap();
        let programs = fake_pg_dump(dir.path(), "17.2");
        engine.send(Command::StartDump(run_on(
            &data_source,
            file.clone(),
            "17.4",
            programs,
        )));
        assert_eq!(
            ended(&mut engine, DumpRunId(9)),
            Err(DumpFailure::Exists(file.clone()))
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "yesterday's dump");
        engine.shut_down();
    }
}
