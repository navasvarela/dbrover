//! Drives the engine headlessly through its public API: send commands, assert on events.

// Test helpers panic on failure like assertions do; clippy's test exemption only covers
// `#[test]` functions themselves, not helpers in an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{
    FakeInstaller, FixedReleases, MemorySecrets, TestEngine, postgres_test_server,
    sqlite_data_source, sqlite_database,
};
use dbrover_core::ChildrenQuery;
use dbrover_core::{
    CatalogPath, ConnectionConfig, DataSource, DataSourceId, FolderKind, NodeKind, Password,
    PostgresConfig, ServerConfig, SshConfig, SshLogin, TlsMode,
};
use dbrover_engine::{
    Command, ConsoleId, DatabaseFile, Event, Export, InstalledUpdate, PasswordUpdate, Release,
    Restart, SessionState, WatchedFile,
};
use dbrover_export::ExportSpec;
use dbrover_store::{
    ConsoleKey, ConsoleName, CustomFile, DataSourceFile, FormatSettings, KeymapBindings,
    KeywordCase, Projects, SavedConsole, Secret, SecretStore, Settings,
};

#[test]
fn saves_connects_disconnects_and_deletes_a_data_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let secrets = Arc::new(MemorySecrets::default());
    let mut engine = TestEngine::start(&file, Arc::clone(&secrets));
    engine.expect_started(vec![]);
    let data_source = sqlite_data_source(sqlite_database(dir.path()));
    let id = data_source.id;
    secrets
        .set(id, Secret::McpLogin, &Password::new("left over"))
        .unwrap();
    secrets
        .set(id, Secret::Ssh, &Password::new("left over too"))
        .unwrap();

    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    assert_eq!(
        DataSourceFile::new(&file).load().unwrap(),
        vec![data_source]
    );

    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    let SessionState::Connected(server) = engine.expect_session(id) else {
        panic!("expected to connect");
    };
    assert_eq!(server.product, "SQLite");

    engine.send(Command::Disconnect(id));
    assert_eq!(engine.expect_session(id), SessionState::Disconnected);

    engine.send(Command::DeleteDataSource(id));
    engine.expect(Event::DataSourceDeleted(id));
    assert!(DataSourceFile::new(&file).load().unwrap().is_empty());
    assert_eq!(secrets.mcp(id), None, "the MCP login's password goes too");
    assert_eq!(secrets.saved(id, Secret::Ssh), None, "and the SSH secret");
    engine.shut_down();
}

#[test]
fn reloads_saved_data_sources_and_stores_passwords_in_the_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let secrets = Arc::new(MemorySecrets::default());
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "remote".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "db.internal".to_owned(),
                port: 5432.into(),
                database: "app".to_owned(),
                user: "juan".to_owned(),
                tls: TlsMode::Require,
                ssh: Some(SshConfig {
                    host: "bastion.internal".to_owned(),
                    port: 22.into(),
                    user: "deploy".to_owned(),
                    login: SshLogin::Password,
                }),
                password_command: None,
            },
        }),
    };

    let mut engine = TestEngine::start(&file, Arc::clone(&secrets));
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Set(Password::new("s3cret")),
        ssh_secret: PasswordUpdate::Set(Password::new("over ssh")),
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
    engine.shut_down();

    assert_eq!(
        secrets.database(data_source.id),
        Some(Password::new("s3cret"))
    );
    assert_eq!(
        secrets.saved(data_source.id, Secret::Ssh),
        Some(Password::new("over ssh"))
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(!written.contains("s3cret"), "{written}");
    assert!(!written.contains("over ssh"), "{written}");
    let mut restarted = TestEngine::start(&file, secrets);
    restarted.expect_started(vec![data_source]);
}

#[test]
fn saves_console_files_in_order_and_reloads_them_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let data_source = DataSourceId::generate();
    let key = |name: &str| ConsoleKey {
        data_source,
        name: ConsoleName::try_from(name.to_owned()).unwrap(),
    };
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect_started(vec![]);

    for text in ["select 1;", "select 2;"] {
        engine.send(Command::SaveConsole {
            key: key("console"),
            text: text.to_owned(),
        });
    }
    engine.send(Command::SaveConsole {
        key: key("console_2"),
        text: "gone".to_owned(),
    });
    engine.send(Command::DeleteConsoleFile(key("console_2")));
    engine.shut_down();

    let mut restarted = TestEngine::start(&file, Arc::default());
    restarted.expect(Event::Started);
    restarted.expect_keymap(Ok(KeymapBindings::new()));
    restarted.expect_settings(Ok(Settings::default()));
    restarted.expect_no_custom_files();
    restarted.expect_nothing_trusted();
    restarted.expect(Event::ProjectsLoaded(Projects::default()));
    restarted.expect(Event::DataSourcesLoaded(vec![]));
    restarted.expect(Event::ConsolesLoaded(vec![SavedConsole {
        key: key("console"),
        text: "select 2;".to_owned(),
    }]));
}

#[test]
fn opens_saves_and_watches_a_sql_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let path = dir.path().join("report.sql");
    let console = ConsoleKey {
        data_source: DataSourceId::generate(),
        name: ConsoleName::try_from("file_1".to_owned()).unwrap(),
    };

    engine.send(Command::OpenSqlFile(path.clone()));
    assert!(matches!(
        engine.next_event(),
        Event::SqlFileRead { result: Err(error), .. } if error.contains("could not read")
    ));

    engine.send(Command::SaveSqlFile {
        console: console.clone(),
        path: path.clone(),
        text: "select 1;".to_owned(),
    });
    let Event::SqlFileSaved {
        result: Ok(modified),
        ..
    } = engine.next_event()
    else {
        panic!("the file wasn't saved");
    };
    engine.send(Command::OpenSqlFile(path.clone()));
    assert!(matches!(
        engine.next_event(),
        Event::SqlFileRead { result: Ok(contents), .. }
            if contents.text == "select 1;" && contents.modified == modified
    ));

    // Changed by another program, then removed.
    let watched = |modified| {
        vec![WatchedFile {
            path: path.clone(),
            modified,
        }]
    };
    std::fs::write(&path, "select 2;").unwrap();
    engine.send(Command::CheckSqlFiles(watched(None)));
    assert!(matches!(
        engine.next_event(),
        Event::SqlFileChanged { contents: Some(contents), .. } if contents.text == "select 2;"
    ));
    std::fs::remove_file(&path).unwrap();
    engine.send(Command::CheckSqlFiles(watched(modified)));
    engine.expect(Event::SqlFileChanged {
        path: path.clone(),
        contents: None,
    });

    // Checks are answered in the order sent, saves included: a check sent before a save sees
    // the file as it was, and is answered first. Answered after the save instead (as when
    // checks ran in the background, which Linux showed), it would tell a console the file
    // went back to its old text. A check of a file already known to be gone says nothing:
    // the read sent after it is answered next.
    engine.send(Command::CheckSqlFiles(watched(None)));
    engine.send(Command::OpenSqlFile(path.clone()));
    assert!(matches!(
        engine.next_event(),
        Event::SqlFileRead { result: Err(_), .. }
    ));
    std::fs::write(&path, "select 3;").unwrap();
    engine.send(Command::CheckSqlFiles(watched(None)));
    engine.send(Command::SaveSqlFile {
        console,
        path: path.clone(),
        text: "select 4;".to_owned(),
    });
    let Event::SqlFileChanged {
        contents: Some(before_the_save),
        ..
    } = engine.next_event()
    else {
        panic!("the check sent before the save wasn't answered first");
    };
    assert_eq!(before_the_save.text, "select 3;");
    let Event::SqlFileSaved {
        result: Ok(modified),
        ..
    } = engine.next_event()
    else {
        panic!("the file wasn't saved again");
    };
    engine.send(Command::CheckSqlFiles(watched(modified)));
    engine.send(Command::OpenSqlFile(path));
    assert!(matches!(
        engine.next_event(),
        Event::SqlFileRead { result: Ok(contents), .. } if contents.text == "select 4;"
    ));
}

#[test]
fn exports_rows_to_a_file_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let export = |path: std::path::PathBuf| Export {
        path,
        spec: ExportSpec::Csv(Default::default()),
        columns: vec![dbrover_export::ExportColumn::text("n")],
        rows: vec![vec![Some("1".to_owned())], vec![None]],
    };
    let console = ConsoleId(1);
    let written = dir.path().join("rows.csv");
    let unwritable = dir.path().join("missing/rows.csv");

    engine.send(Command::ExportRows {
        console,
        export: export(written.clone()),
    });
    engine.expect(Event::ExportFinished {
        exporter: dbrover_engine::Exporter::Console(console),
        path: written.clone(),
        result: Ok(2),
    });
    engine.send(Command::ExportRows {
        console,
        export: export(unwritable.clone()),
    });

    assert!(matches!(
        engine.next_event(),
        Event::ExportFinished { result: Err(error), .. } if error.contains("missing/rows.csv")
    ));
    assert_eq!(std::fs::read_to_string(written).unwrap(), "n\r\n1\r\n\r\n");
}

#[test]
fn creates_and_reloads_the_keymap_but_never_overwrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let bindings = |shortcut: &str| {
        KeymapBindings::from([("console.run".to_owned(), vec![shortcut.to_owned()])])
    };

    engine.send(Command::CreateKeymap {
        text: "[bindings]\n\"console.run\" = \"cmd+r\"\n".to_owned(),
    });
    engine.expect_keymap(Ok(bindings("cmd+r")));
    std::fs::write(
        dir.path().join("keymap.toml"),
        "[bindings]\n\"console.run\" = \"f5\"\n",
    )
    .unwrap();
    engine.send(Command::ReloadKeymap);
    engine.expect_keymap(Ok(bindings("f5")));
    engine.send(Command::CreateKeymap {
        text: "[bindings]\n".to_owned(),
    });

    engine.expect_keymap(Err(String::new()));
    engine.send(Command::ReloadKeymap);
    engine.expect_keymap(Ok(bindings("f5")));
}

#[test]
fn reloads_the_customisation_files_with_their_problems() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    std::fs::write(
        dir.path().join("snippets.toml"),
        "[[snippet]]\ntrigger = \"sel\"\nbody = \"SELECT $1\"\n\n[[snippet]]\nbody = \"x\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("theme.toml"), "not toml").unwrap();

    engine.send(Command::ReloadCustomFiles);

    let [actions, snippets, theme] = engine.expect_custom_files().try_into().unwrap();
    assert!(actions.problems().is_empty());
    let CustomFile::Snippets(snippets) = snippets else {
        panic!("{snippets:?}");
    };
    assert_eq!(snippets.value.len(), 1);
    assert_eq!(snippets.problems, ["snippet 2: missing field `trigger`"]);
    assert_eq!(theme.problems().len(), 1, "{:?}", theme.problems());
}

#[test]
fn saves_and_reloads_the_settings_and_reports_a_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let lower = Settings {
        format: FormatSettings {
            keyword_case: KeywordCase::Lower,
            indent_width: 2,
        },
        ..Settings::default()
    };

    engine.send(Command::SaveSettings(lower.clone()));
    engine.expect_settings(Ok(lower.clone()));
    let settings_path = dir.path().join("settings.toml");
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("lower")
    );

    std::fs::write(&settings_path, "[format]\nkeyword_case = \"shouting\"\n").unwrap();
    engine.send(Command::ReloadSettings);
    engine.expect_settings(Err(String::new()));
}

#[test]
fn reports_the_newest_release_or_why_it_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let release = Release {
        tag: "v0.2.0".to_owned(),
        page: "https://example.com/releases/v0.2.0".to_owned(),
    };
    let data_sources = dir.path().join("data_sources.toml");
    let mut engine = TestEngine::start_with_releases(
        &data_sources,
        Arc::default(),
        FixedReleases(Some(release.clone())),
    );
    engine.expect_started(vec![]);
    engine.send(Command::CheckForUpdates);
    engine.expect(Event::LatestRelease(Ok(release)));
    drop(engine);

    let mut engine = TestEngine::start(&data_sources, Arc::default());
    engine.expect_started(vec![]);
    engine.send(Command::CheckForUpdates);
    engine.expect(Event::LatestRelease(Err(
        "no release has been published yet".to_owned(),
    )));
}

#[test]
fn installs_an_update_reporting_progress_or_why_it_failed() {
    let dir = tempfile::tempdir().unwrap();
    let data_sources = dir.path().join("data_sources.toml");
    let cases = [
        (
            FakeInstaller::Installs("9.9.9"),
            Ok(InstalledUpdate {
                version: "9.9.9".to_owned(),
                restart: Restart::Relaunch,
            }),
        ),
        (
            FakeInstaller::Fails("not from an app bundle"),
            Err("not from an app bundle".to_owned()),
        ),
    ];
    for (installer, expected) in cases {
        let mut engine = TestEngine::start_with_installer(&data_sources, installer);
        engine.expect_started(vec![]);
        engine.send(Command::InstallUpdate);
        // The engine reports each whole percent once.
        for downloaded in [0, 50, 100] {
            engine.expect(Event::UpdateProgress {
                downloaded,
                total: Some(100),
            });
        }
        engine.expect(Event::UpdateInstalled(expected));
    }
}

#[test]
fn reports_failed_tests_and_connections() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let missing = sqlite_data_source(dir.path().join("missing.db"));
    let id = missing.id;

    engine.send(Command::TestConnection {
        data_source: missing.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    match engine.next_event() {
        Event::ConnectionTested {
            id: tested,
            result: Err(error),
        } if tested == id => assert!(error.starts_with("could not connect"), "{error}"),
        other => panic!("expected a failed test, got {other:?}"),
    }

    engine.send(Command::SaveDataSource {
        data_source: missing,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    engine.send(Command::Connect(id));
    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert!(matches!(engine.expect_session(id), SessionState::Failed(_)));
}

/// Never set anywhere, so the placeholder that names it can't resolve.
const UNSET_VARIABLE: &str = "DBROVER_TEST_NEVER_SET";

#[test]
fn stops_a_connection_whose_placeholder_is_unset_saying_which_setting_uses_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let shared = sqlite_data_source(format!("${{{UNSET_VARIABLE}}}/shop.db").into());
    let id = shared.id;
    engine.send(Command::SaveDataSource {
        data_source: shared,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

    engine.send(Command::Connect(id));

    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert_eq!(
        engine.expect_session(id),
        SessionState::Failed(format!(
            "could not resolve the file: ${{{UNSET_VARIABLE}}} is not set"
        ))
    );
    assert!(!dir.path().join(format!("${{{UNSET_VARIABLE}}}")).exists());
}

#[test]
fn never_overwrites_an_unreadable_data_sources_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let broken = "[[data_source]]\nname = 3\n";
    std::fs::write(&file, broken).unwrap();
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect(Event::Started);
    engine.expect_keymap(Ok(KeymapBindings::new()));
    engine.expect_settings(Ok(Settings::default()));
    engine.expect_no_custom_files();
    engine.expect_nothing_trusted();
    engine.expect(Event::ProjectsLoaded(Projects::default()));
    assert!(matches!(
        engine.next_event(),
        Event::DataSourcesUnavailable { .. }
    ));
    engine.expect(Event::ConsolesLoaded(vec![]));
    engine.expect(Event::DiagramsLoaded(vec![]));

    let data_source = sqlite_data_source(dir.path().join("app.db"));
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });

    assert!(matches!(
        engine.next_event(),
        Event::DataSourceSaveFailed { id, .. } if id == data_source.id
    ));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
}

#[test]
fn loading_children_connects_first_and_answers_catalog_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let database = sqlite_database(dir.path());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("CREATE VIEW v AS SELECT id FROM t")
        .unwrap();
    let data_source = sqlite_data_source(database);
    let id = data_source.id;
    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));

    engine.send(Command::LoadChildren {
        id,
        path: CatalogPath::root(),
        query: ChildrenQuery::default(),
    });

    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    assert!(matches!(
        engine.expect_session(id),
        SessionState::Connected(_)
    ));
    match engine.next_event() {
        Event::ChildrenLoaded {
            id: loaded,
            path,
            result: Ok(page),
            ..
        } if loaded == id && path.is_root() => {
            let labels: Vec<_> = page.nodes.iter().map(|node| node.label.as_str()).collect();
            assert_eq!(labels, ["main"]);
        }
        other => panic!("expected the root's children, got {other:?}"),
    }

    let view = CatalogPath::root()
        .child(NodeKind::Schema, "main")
        .child(NodeKind::Folder(FolderKind::Views), "views")
        .child(NodeKind::View, "v");
    engine.send(Command::LoadDefinition {
        id,
        path: view.clone(),
    });
    engine.expect(Event::DefinitionLoaded {
        id,
        path: view,
        result: Ok("CREATE VIEW v AS SELECT id FROM t;".to_owned()),
    });
}

#[test]
fn creates_sqlite_and_duckdb_databases_but_never_overwrites_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    let files = [
        DatabaseFile::Sqlite(dir.path().join("new.db")),
        DatabaseFile::DuckDb(dir.path().join("new.duckdb")),
    ];

    for file in files {
        engine.send(Command::CreateDatabaseFile(file.clone()));
        engine.expect(Event::DatabaseFileCreated {
            file: file.clone(),
            result: Ok(()),
        });
        engine.send(Command::CreateDatabaseFile(file.clone()));

        match engine.next_event() {
            Event::DatabaseFileCreated {
                result: Err(error), ..
            } => assert!(error.contains("already exists"), "{error}"),
            other => panic!("expected a refusal to overwrite, got {other:?}"),
        }
    }
}

/// Needs `DBROVER_TEST_POSTGRES_URL`, like the Postgres driver tests; skipped without it.
#[test]
fn connects_to_postgres_with_the_saved_password() {
    let Some(parsed) = postgres_test_server() else {
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
        config: ConnectionConfig::Postgres(parsed.config),
    };
    let id = data_source.id;

    engine.send(Command::SaveDataSource {
        data_source,
        password: PasswordUpdate::Set(parsed.password),
        ssh_secret: PasswordUpdate::Keep,
    });
    assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
    engine.send(Command::Connect(id));

    assert_eq!(engine.expect_session(id), SessionState::Connecting);
    let state = engine.expect_session(id);
    assert!(
        matches!(&state, SessionState::Connected(server) if server.product == "PostgreSQL"),
        "{state:?}"
    );
}
