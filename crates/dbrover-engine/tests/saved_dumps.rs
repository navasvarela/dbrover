//! Saved dumps (W17h): saved in `dumps.toml`, run now as their schedule would, the newest
//! `keep` files kept and older ones removed, a schedule turned on per machine, and `dbrover
//! dump run`'s headless run. On SQLite, so they need no server and run on every system.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::num::NonZeroU8;
use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, sqlite_data_source};
use dbrover_core::{DataSource, DumpContent, DumpFormat};
use dbrover_engine::{Command, Event, HeadlessDump, PasswordUpdate, run_saved_dump};
use dbrover_store::{
    DataSourceFile, HistoryFile, HistoryQuery, HistorySourceKind, SavedDump, SavedDumpsFile,
    Schedule, ScheduleState, SettingsFile, TrustedCommandsFile,
};

fn shop(dir: &Path) -> DataSource {
    let database = dir.join("shop.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('kept');")
        .unwrap();
    sqlite_data_source(database)
}

fn saved(data_source: &DataSource, folder: &Path, keep: u32) -> SavedDump {
    SavedDump {
        id: "nightly-shop".to_owned(),
        name: "Nightly shop".to_owned(),
        data_source: data_source.id,
        database: "shop".to_owned(),
        schemas: Vec::new(),
        tables: Vec::new(),
        content: DumpContent::SchemaAndData,
        format: DumpFormat::DatabaseFile,
        leave_out_owners: false,
        leave_out_privileges: false,
        folder: folder.to_owned(),
        keep,
        schedule: Some(Schedule::Hours {
            hours: NonZeroU8::new(1).unwrap(),
        }),
    }
}

fn until<T>(engine: &mut TestEngine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(engine.next_event()) {
            return found;
        }
    }
}

fn loaded(engine: &mut TestEngine) -> (Vec<SavedDump>, Vec<ScheduleState>) {
    until(engine, |event| match event {
        Event::SavedDumpsLoaded { dumps, states } => Some((dumps, states)),
        _ => None,
    })
}

fn files_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn runs_a_saved_dump_now_and_keeps_its_newest_files() {
    let dir = tempfile::tempdir().unwrap();
    let data_source = shop(dir.path());
    let mut engine = TestEngine::start(
        &dir.path().join("data_sources.toml"),
        Arc::new(MemorySecrets::default()),
    );
    engine.expect_started(vec![]);
    engine.send(Command::LoadSavedDumps);
    let (dumps, _) = loaded(&mut engine);
    assert!(dumps.is_empty());
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));

    let folder = dir.path().join("nightly");
    let dump = saved(&data_source, &folder, 1);
    engine.send(Command::SaveDump(Box::new(dump.clone())));
    let (dumps, _) = loaded(&mut engine);
    assert_eq!(dumps, std::slice::from_ref(&dump));
    assert_eq!(
        SavedDumpsFile::new(dir.path().join("dumps.toml"))
            .load()
            .unwrap(),
        std::slice::from_ref(&dump)
    );

    for _ in 0..2 {
        engine.send(Command::RunSavedDump("nightly-shop".to_owned()));
        let name = until(&mut engine, |event| match event {
            Event::SavedDumpStarted { name, .. } => Some(name),
            _ => None,
        });
        assert_eq!(name, "Nightly shop");
        until(&mut engine, |event| match event {
            Event::DumpEnded { result, .. } => Some(result.unwrap()),
            _ => None,
        });
        // Names are to the second: the next run is a new file.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    let (_, states) = loaded(&mut engine);
    let state = states
        .iter()
        .find(|state| state.id == "nightly-shop")
        .unwrap();
    assert!(state.last_ok.is_some());
    assert_eq!(state.files.len(), 1, "keep = 1");
    assert_eq!(files_in(&folder).len(), 1, "{:?}", files_in(&folder));
    assert!(state.files[0].is_file());

    // Turned on, a schedule's first run is due one interval later, not at once.
    engine.send(Command::SetDumpSchedule {
        id: "nightly-shop".to_owned(),
        on: true,
    });
    let (_, states) = loaded(&mut engine);
    assert!(states[0].on);
    engine.send(Command::CheckDumpSchedules);
    engine.send(Command::DeleteSavedDump("nightly-shop".to_owned()));
    let (dumps, states) = loaded(&mut engine);
    assert!(dumps.is_empty() && states.is_empty());
    assert_eq!(
        files_in(&folder).len(),
        1,
        "a check before the schedule's time ran nothing"
    );
    engine.shut_down();
}

#[test]
fn dbrover_dump_run_runs_a_saved_dump_without_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let data_source = shop(dir.path());
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&data_source))
        .unwrap();
    let folder = dir.path().join("cron");
    SavedDumpsFile::new(dir.path().join("dumps.toml"))
        .save(&[saved(&data_source, &folder, 7)])
        .unwrap();
    let headless = || HeadlessDump {
        data_sources: DataSourceFile::new(dir.path().join("data_sources.toml")),
        settings: SettingsFile::new(dir.path().join("settings.toml")),
        trusted_commands: TrustedCommandsFile::new(dir.path().join("trusted_commands.toml")),
        known_hosts: dir.path().join("known_hosts"),
        history: HistoryFile::new(dir.path().join("history.sqlite")),
        secrets: Arc::new(MemorySecrets::default()),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let done = runtime
        .block_on(run_saved_dump("Nightly shop", headless()))
        .unwrap();
    assert!(done.file.starts_with(&folder) && done.file.is_file());
    let unknown = runtime.block_on(run_saved_dump("weekly", headless()));
    assert_eq!(
        unknown.unwrap_err(),
        "There is no saved dump named \u{201c}weekly\u{201d}."
    );

    let states = dbrover_store::ScheduleStatesFile::new(dir.path().join("dump_schedules.toml"))
        .load()
        .unwrap();
    assert_eq!(states[0].files, std::slice::from_ref(&done.file));
    let history = HistoryFile::new(dir.path().join("history.sqlite"))
        .open()
        .unwrap()
        .search(
            &HistoryQuery {
                kind: Some(HistorySourceKind::Dump),
                ..HistoryQuery::all(data_source.id)
            },
            10,
        )
        .unwrap();
    assert_eq!(history.len(), 1);
}
