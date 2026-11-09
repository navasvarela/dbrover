//! Changes other programs (a `git pull`, a synced folder) make to the settings folder while the
//! engine runs: never undone by the next save (VC1b in `docs/version-control.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use common::TestEngine;
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, SafetyLevel, SqliteConfig};
use dbrover_engine::{Command, Document, Event, Noticed, PasswordUpdate};
use dbrover_store::{
    ConsoleKey, ConsoleName, DataSourceFile, KeywordCase, Projects, SavedConsole, Settings,
    SettingsFile,
};

fn sqlite(name: &str) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: format!("/data/{name}.db").into(),
            read_only: false,
        }),
    }
}

fn save(engine: &TestEngine, data_source: &DataSource) {
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
}

fn renamed(data_source: &DataSource, name: &str) -> DataSource {
    DataSource {
        name: name.to_owned(),
        ..data_source.clone()
    }
}

#[test]
fn a_save_keeps_the_data_sources_another_program_added_or_changed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data_sources.toml");
    let file = DataSourceFile::new(&path);
    let (first, second) = (sqlite("first"), sqlite("second"));
    file.save(&[first.clone(), second.clone()]).unwrap();
    let mut engine = TestEngine::start(&path, Arc::default());
    engine.expect_started(vec![first.clone(), second.clone()]);

    let pulled_first = renamed(&first, "first, pulled");
    let pulled_new = sqlite("pulled");
    file.save(&[pulled_first.clone(), second.clone(), pulled_new.clone()])
        .unwrap();
    let mine = renamed(&second, "second, mine");
    save(&engine, &mine);

    engine.expect(Event::DataSourceSaved(mine.clone()));
    engine.expect(Event::DataSourceSaved(pulled_first.clone()));
    engine.expect(Event::DataSourceSaved(pulled_new.clone()));
    assert_eq!(file.load().unwrap(), [pulled_first, mine, pulled_new]);
    engine.shut_down();
}

#[test]
fn the_window_coming_to_the_front_reads_changed_data_sources_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data_sources.toml");
    let file = DataSourceFile::new(&path);
    let (kept, removed) = (sqlite("kept"), sqlite("removed"));
    file.save(&[kept.clone(), removed.clone()]).unwrap();
    let mut engine = TestEngine::start(&path, Arc::default());
    engine.expect_started(vec![kept.clone(), removed.clone()]);

    engine.send(Command::LookForOutsideChanges);
    // Commands run in order but apart from the test: until the settings come back, the first
    // look may not have run yet. Without this, it could find the pulled file and the second
    // look could then read the file half rewritten with the conflict markers below.
    engine.send(Command::ReloadSettings);
    engine.expect_settings(Ok(Settings::default()));
    let pulled = sqlite("pulled");
    file.save(&[kept.clone(), pulled.clone()]).unwrap();
    engine.send(Command::LookForOutsideChanges);

    engine.expect(Event::DataSourceDeleted(removed.id));
    engine.expect(Event::DataSourceSaved(pulled.clone()));

    fs::write(&path, "<<<<<<< HEAD\n").unwrap();
    engine.send(Command::LookForOutsideChanges);
    save(&engine, &renamed(&kept, "kept, mine"));
    let Event::DataSourceSaveFailed { error, .. } = engine.next_event() else {
        panic!("a file with a merge's conflict markers is never written over");
    };
    assert!(error.contains("not a valid data sources file"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "<<<<<<< HEAD\n");
    engine.shut_down();
}

fn console_key(name: &str) -> ConsoleKey {
    ConsoleKey {
        data_source: DataSourceId::generate(),
        name: ConsoleName::try_from(name.to_owned()).unwrap(),
    }
}

fn console_path(dir: &Path, key: &ConsoleKey) -> std::path::PathBuf {
    dir.join("consoles")
        .join(key.data_source.to_string())
        .join(format!("{}.sql", key.name))
}

#[test]
fn a_console_changed_on_disk_isnt_saved_over_until_the_app_has_seen_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = console_key("console");
    let path = console_path(dir.path(), &key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "select 1;").unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect(Event::Started);
    engine.expect_keymap(Ok(dbrover_store::KeymapBindings::new()));
    engine.expect_settings(Ok(Settings::default()));
    engine.expect_no_custom_files();
    engine.expect_nothing_trusted();
    engine.expect(Event::ProjectsLoaded(Projects::default()));
    engine.expect(Event::DataSourcesLoaded(vec![]));
    engine.expect(Event::ConsolesLoaded(vec![SavedConsole {
        key: key.clone(),
        text: "select 1;".to_owned(),
    }]));
    engine.expect(Event::DiagramsLoaded(vec![]));

    fs::write(&path, "select 'pulled';").unwrap();
    let mine = |text: &str| Command::SaveConsole {
        key: key.clone(),
        text: text.to_owned(),
    };
    engine.send(mine("select 2;"));
    engine.expect(Event::ConsoleChangedOnDisk {
        key: key.clone(),
        text: "select 'pulled';".to_owned(),
        noticed: Noticed::Saving,
    });
    engine.send(mine("select 3;"));
    engine.send(Command::LookForOutsideChanges);
    engine.send(Command::ReloadSettings);
    engine.expect_settings(Ok(Settings::default()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "select 'pulled';");

    engine.send(Command::SeenOnDisk(Document::Console(key.clone())));
    engine.send(mine("select 4;"));
    engine.send(Command::LookForOutsideChanges);
    engine.send(Command::ReloadSettings);
    engine.expect_settings(Ok(Settings::default()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "select 4;");

    fs::write(&path, "select 'pulled again';").unwrap();
    engine.send(Command::LookForOutsideChanges);
    engine.expect(Event::ConsoleChangedOnDisk {
        key,
        text: "select 'pulled again';".to_owned(),
        noticed: Noticed::Looking,
    });
    engine.shut_down();
}

#[test]
fn settings_changed_on_disk_are_reported_rather_than_saved_over() {
    let dir = tempfile::tempdir().unwrap();
    let data_sources = dir.path().join("data_sources.toml");
    let settings_path = dir.path().join("settings.toml");
    let mut engine = TestEngine::start(&data_sources, Arc::default());
    engine.expect_started(vec![]);
    let mut mine = Settings::default();
    mine.format.indent_width = 2;
    engine.send(Command::SaveSettings(mine.clone()));
    engine.expect_settings(Ok(mine.clone()));

    fs::write(&settings_path, "[format]\nkeyword_case = \"lower\"\n").unwrap();
    let mut pulled = Settings::default();
    pulled.format.keyword_case = KeywordCase::Lower;
    mine.format.keyword_case = KeywordCase::Preserve;
    engine.send(Command::SaveSettings(mine.clone()));
    engine.expect(Event::SettingsChangedOnDisk {
        settings: pulled.clone(),
        noticed: Noticed::Saving,
    });
    assert_eq!(SettingsFile::new(&settings_path).load().unwrap(), pulled);

    engine.send(Command::SeenOnDisk(Document::Settings));
    engine.send(Command::SaveSettings(mine.clone()));
    engine.expect_settings(Ok(mine.clone()));
    assert_eq!(SettingsFile::new(&settings_path).load().unwrap(), mine);
    engine.shut_down();
}
