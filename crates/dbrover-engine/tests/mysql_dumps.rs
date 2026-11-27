//! Dumping and restoring MySQL and MariaDB (W17f) with each family's own programs: a database
//! with a view and a trigger its administrator defined, dumped as plain SQL and as gzipped SQL
//! by a login of its own with an awkward password, then restored by that login into a new
//! database, the `DEFINER` clauses left out so it may.
//!
//! Skipped unless `DBROVER_TEST_MYSQL_URL` or `DBROVER_TEST_MARIADB_URL` is set and
//! `DBROVER_TEST_CLIENTS` names a folder with `mysqldump` and `mysql`, or `mariadb-dump` and
//! `mariadb`: on CI the scripts in docker/clients, which run them from the servers' images.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, mysql_test_servers};
use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, DumpEngine, DumpFormat, DumpOptions,
    DumpRequest, DumpScope, MysqlConfig, Password, RestoreOptions, RestoreRequest, RestoreTarget,
    Route, SafetyLevel, ServerConfig, ServerInfo,
};
use dbrover_dump::Program;
use dbrover_engine::{
    Command, DumpDone, DumpFailure, DumpRun, DumpRunId, Event, PasswordUpdate, ProgramPaths,
    RestoreDone, RestoreRun,
};
use dbrover_store::TrustedCommand;

/// The compose servers' root password (`MYSQL_ROOT_PASSWORD`, `MARIADB_ROOT_PASSWORD` in
/// docker-compose.yml), which creates the test's own login and the objects it defines.
const ROOT_PASSWORD: &str = "dbrover";

/// A password the option file has to carry as it is: quotes, a comment sign, a backslash,
/// spaces at either end.
const AWKWARD_PASSWORD: &str = r#" p"a#ss\w'd "#;

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

/// Each family's own programs, so neither falls back on the other's.
fn programs(clients: &Path, engine: DumpEngine) -> ProgramPaths {
    let own = match engine {
        DumpEngine::MariaDb => [Program::MariadbDump, Program::Mariadb],
        _ => [Program::Mysqldump, Program::Mysql],
    };
    ProgramPaths(
        own.into_iter()
            .map(|program| (program, clients.join(program.name())))
            .collect(),
    )
}

fn connect(
    runtime: &tokio::runtime::Runtime,
    config: &MysqlConfig,
    password: &str,
) -> Box<dyn Connection> {
    Box::new(
        runtime
            .block_on(dbrover_driver_mysql::connect(
                config,
                Some(&Password::new(password)),
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

/// A database and a login of the test's own, dropped when it ends.
struct Fixture {
    runtime: tokio::runtime::Runtime,
    root: Box<dyn Connection>,
    database: String,
    user: String,
    config: MysqlConfig,
    server: ServerInfo,
    engine: DumpEngine,
}

impl Fixture {
    fn create(server: &MysqlConfig, test: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root_config = MysqlConfig {
            server: ServerConfig {
                user: "root".to_owned(),
                ..server.server.clone()
            },
        };
        let mut root = connect(&runtime, &root_config, ROOT_PASSWORD);
        let info = root.server_info().clone();
        let engine = if info.product == "MariaDB" {
            DumpEngine::MariaDb
        } else {
            DumpEngine::Mysql
        };
        let family = engine.to_string().to_lowercase();
        let suffix = format!("{family}_{test}_{}", std::process::id());
        let database = format!("w17_{suffix}");
        let user = format!("w17_{suffix}");
        let password = AWKWARD_PASSWORD.replace('\\', "\\\\").replace('\'', "''");
        for sql in [
            format!("DROP DATABASE IF EXISTS {database}"),
            format!("DROP USER IF EXISTS '{user}'@'%'"),
            format!("CREATE USER '{user}'@'%' IDENTIFIED BY '{password}'"),
            // The pattern takes in the databases restored into, `<database>_r1` and on.
            format!("GRANT ALL PRIVILEGES ON `{database}%`.* TO '{user}'@'%'"),
            format!("CREATE DATABASE {database}"),
            format!(
                "CREATE TABLE {database}.customers \
                 (id int PRIMARY KEY, name varchar(50), photo varbinary(4))"
            ),
            format!(
                "INSERT INTO {database}.customers VALUES \
                 (1, 'Kate O''Brien', 0x00FF0A0D), (2, 'Zoë', NULL)"
            ),
            // Defined by root, in the database itself so their definitions don't name it:
            // restored by the test's login only once DEFINER is left out.
            format!("USE {database}"),
            "CREATE DEFINER = 'root'@'%' VIEW named AS SELECT name FROM customers".to_owned(),
            "CREATE DEFINER = 'root'@'%' TRIGGER upper_name BEFORE INSERT ON customers \
             FOR EACH ROW SET NEW.name = UPPER(NEW.name)"
                .to_owned(),
        ] {
            execute(&runtime, root.as_mut(), &sql);
        }
        let config = MysqlConfig {
            server: ServerConfig {
                user: user.clone(),
                database: database.clone(),
                ..server.server.clone()
            },
        };
        Self {
            runtime,
            root,
            database,
            user,
            config,
            server: info,
            engine,
        }
    }

    fn data_source(&self) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Mysql(self.config.clone()),
        }
    }

    fn value(&mut self, sql: &str) -> Option<String> {
        first_value(&self.runtime, self.root.as_mut(), sql)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (database, user) = (self.database.clone(), self.user.clone());
        self.runtime.block_on(async {
            for sql in [
                format!("DROP DATABASE IF EXISTS {database}"),
                format!("DROP DATABASE IF EXISTS {database}_r1"),
                format!("DROP DATABASE IF EXISTS {database}_r2"),
                format!("DROP USER IF EXISTS '{user}'@'%'"),
            ] {
                if let Ok(cursor) = self.root.execute(&sql).await {
                    let _ = cursor.finish().await;
                }
            }
        });
    }
}

/// An engine with the fixture's data source saved, with its password, and `programs` trusted
/// to run, as typing their paths in Settings › Dumps does.
fn start_engine(
    fixture: &Fixture,
    programs: &ProgramPaths,
) -> (TestEngine, DataSource, tempfile::TempDir) {
    let data_source = fixture.data_source();
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
    for (program, path) in &programs.0 {
        engine.send(Command::TrustCommand(TrustedCommand::dump_program(
            program.setting_key(),
            path,
        )));
        until(&mut engine, |event| {
            matches!(event, Event::TrustedCommandsLoaded(_)).then_some(())
        });
    }
    (engine, data_source, dir)
}

fn until<T>(engine: &mut TestEngine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(engine.next_event()) {
            return found;
        }
    }
}

fn dumped(engine: &mut TestEngine, run: DumpRunId) -> Result<DumpDone, DumpFailure> {
    until(engine, |event| match event {
        Event::DumpEnded { run: ended, result } if ended == run => Some(result),
        _ => None,
    })
}

fn restored(engine: &mut TestEngine, run: DumpRunId) -> Result<RestoreDone, DumpFailure> {
    until(engine, |event| match event {
        Event::RestoreEnded { run: ended, result } if ended == run => Some(result),
        _ => None,
    })
}

/// What the test reads back of a database: its rows, the photos' bytes, the view and what the
/// trigger does to a new row.
const CHECK: &str = "SELECT CONCAT_WS('|', \
     (SELECT GROUP_CONCAT(name ORDER BY id) FROM {db}.customers), \
     (SELECT HEX(photo) FROM {db}.customers WHERE id = 1), \
     (SELECT COUNT(*) FROM {db}.named))";

#[test]
fn dumps_and_restores_each_family_with_its_own_programs() {
    let Some(clients) = clients() else {
        return;
    };
    for (server, _) in mysql_test_servers() {
        let mut fixture = Fixture::create(&server, "both");
        let (mut engine, data_source, dir) =
            start_engine(&fixture, &programs(&clients, fixture.engine));
        let original = fixture
            .value(&CHECK.replace("{db}", &fixture.database))
            .unwrap();
        assert_eq!(original, "Kate O'Brien,Zoë|00FF0A0D|2");

        for (n, format) in [(1_u64, DumpFormat::Sql), (2, DumpFormat::SqlGz)] {
            let file = dir
                .path()
                .join(format!("shop-{n}.{}", format.extension().unwrap()));
            engine.send(Command::StartDump(Box::new(DumpRun {
                run: DumpRunId(n),
                request: DumpRequest {
                    data_source: data_source.id,
                    database: fixture.database.clone(),
                    scope: DumpScope::Database,
                    format,
                    options: DumpOptions::default(),
                    file: file.clone(),
                },
                server: fixture.server.clone(),
                programs: programs(&clients, fixture.engine),
            })));
            dumped(&mut engine, DumpRunId(n)).unwrap();

            let database = format!("{}_r{n}", fixture.database);
            engine.send(Command::StartRestore(Box::new(RestoreRun {
                run: DumpRunId(10 + n),
                request: RestoreRequest {
                    data_source: data_source.id,
                    file,
                    target: RestoreTarget::NewDatabase(database.clone()),
                    tables: None,
                    options: RestoreOptions::default(),
                },
                server: fixture.server.clone(),
                programs: programs(&clients, fixture.engine),
            })));
            let done = restored(&mut engine, DumpRunId(10 + n)).unwrap();
            assert_eq!(done.database, database);
            assert_eq!(
                fixture.value(&CHECK.replace("{db}", &database)).as_deref(),
                Some(original.as_str()),
                "{} {format:?}",
                fixture.engine
            );
            let definer = fixture.value(&format!(
                "SELECT DEFINER FROM information_schema.VIEWS WHERE TABLE_SCHEMA = '{database}'"
            ));
            assert_eq!(
                definer,
                Some(format!("{}@%", fixture.user)),
                "the restoring login"
            );
        }
        engine.shut_down();
    }
}

#[test]
fn a_restore_that_keeps_definers_fails_without_the_privilege_to() {
    let Some(clients) = clients() else {
        return;
    };
    for (server, _) in mysql_test_servers() {
        let fixture = Fixture::create(&server, "definer");
        let (mut engine, data_source, dir) =
            start_engine(&fixture, &programs(&clients, fixture.engine));
        let file = dir.path().join("shop.sql");
        engine.send(Command::StartDump(Box::new(DumpRun {
            run: DumpRunId(1),
            request: DumpRequest {
                data_source: data_source.id,
                database: fixture.database.clone(),
                scope: DumpScope::Database,
                format: DumpFormat::Sql,
                options: DumpOptions::default(),
                file: file.clone(),
            },
            server: fixture.server.clone(),
            programs: programs(&clients, fixture.engine),
        })));
        dumped(&mut engine, DumpRunId(1)).unwrap();
        engine.send(Command::StartRestore(Box::new(RestoreRun {
            run: DumpRunId(2),
            request: RestoreRequest {
                data_source: data_source.id,
                file,
                target: RestoreTarget::NewDatabase(format!("{}_r1", fixture.database)),
                tables: None,
                options: RestoreOptions {
                    leave_out_owners: false,
                    ..RestoreOptions::default()
                },
            },
            server: fixture.server.clone(),
            programs: programs(&clients, fixture.engine),
        })));
        let failure = restored(&mut engine, DumpRunId(2)).unwrap_err();
        let DumpFailure::Exited { stderr, .. } = &failure else {
            panic!("{failure:?}");
        };
        assert!(
            stderr.contains("SUPER") || stderr.contains("SET_USER_ID") || stderr.contains("denied"),
            "{stderr}"
        );
        engine.shut_down();
    }
}
