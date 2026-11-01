use std::collections::BTreeMap;

use tempfile::TempDir;

use super::*;

/// Every file under `root`, by its path relative to `root`, with its contents. SQLite's
/// shared-memory index is left out: reading a database may create or touch it, and it holds
/// nothing of the user's.
fn files_under(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if !path.to_string_lossy().ends_with("-shm") {
                let relative = path.strip_prefix(root).unwrap().to_owned();
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// The Linux and Windows layout: settings and window state in separate folders.
fn separate_folders(root: &Path, name: &str) -> InstallFolders {
    InstallFolders {
        settings: root.join("config").join(name),
        data: root.join("data").join(name),
    }
}

/// The macOS layout: one folder for both.
fn one_folder(root: &Path, name: &str) -> InstallFolders {
    let folder = root.join("Application Support").join(name);
    InstallFolders {
        settings: folder.clone(),
        data: folder,
    }
}

/// Today's layout, as in `~/.config/dbrover` and `~/.local/share/dbrover`.
fn dot_folders(root: &Path) -> InstallFolders {
    InstallFolders {
        settings: root.join(".config/dbrover"),
        data: root.join(".local/share/dbrover"),
    }
}

/// A previous install's settings, with a console and the user's own files, but no history.
fn previous_settings(folders: &InstallFolders) {
    let settings = &folders.settings;
    write(&settings.join("data_sources.toml"), "[[data_source]]\n");
    write(&settings.join("projects.toml"), "[[project]]\n");
    write(&settings.join("settings.toml"), "[format]\n");
    write(&settings.join("keymap.toml"), "[bindings]\n");
    write(&settings.join("actions.toml"), "[[action]]\n");
    write(&settings.join("snippets.toml"), "[[snippet]]\n");
    write(&settings.join("theme.toml"), "[colors]\n");
    write(
        &settings.join("consoles/6f1c0d9e/scratch.sql"),
        "SELECT 1;\n",
    );
    write(&folders.data.join("app.ron"), "(layout)\n");
    write(&folders.data.join("trusted_commands.toml"), "[[command]]\n");
    write(
        &folders.data.join("logs/dbrover.2026-10-28"),
        "a log line\n",
    );
}

fn adopt(previous: &InstallFolders, current: &InstallFolders) -> CopiedInstall {
    match adopt_previous_install(previous, current).unwrap() {
        Adoption::Copied(copied) => copied,
        Adoption::NotNeeded => panic!("expected a copy"),
    }
}

#[test]
fn copies_every_file_but_the_logs_in_either_layout() {
    for layout in [separate_folders, one_folder] {
        let dir = TempDir::new().unwrap();
        let previous = layout(dir.path(), "old-name");
        let current = layout(dir.path(), "new-name");
        previous_settings(&previous);

        let copied = adopt(&previous, &current);

        assert_eq!(copied.from, previous.settings);
        assert!(copied.history_read_as_files.is_none());
        assert!(copied.note_not_written.is_none());
        let mut expected = files_under(&previous.settings);
        expected.retain(|path, _| !path.starts_with("logs") && !path.ends_with(note_file_name()));
        assert_eq!(files_under(&current.settings), expected);
        assert_eq!(
            fs::read_to_string(current.data.join("app.ron")).unwrap(),
            "(layout)\n"
        );
        assert!(!current.data.join("logs").exists());
        assert!(!staging_folder(&current.settings).exists());
        assert!(!staging_folder(&current.data).exists());
    }
}

#[test]
fn leaves_the_previous_folders_as_they_were_apart_from_the_note() {
    let dir = TempDir::new().unwrap();
    let previous = separate_folders(dir.path(), "old-name");
    let current = separate_folders(dir.path(), "new-name");
    previous_settings(&previous);
    let settings_before = files_under(&previous.settings);
    let data_before = files_under(&previous.data);

    adopt(&previous, &current);

    let mut settings_after = files_under(&previous.settings);
    let note = settings_after
        .remove(Path::new(&note_file_name()))
        .expect("a note in the previous settings folder");
    assert_eq!(settings_after, settings_before);
    assert_eq!(files_under(&previous.data), data_before);
    let note = String::from_utf8(note).unwrap();
    assert!(
        note.contains(&current.settings.display().to_string()),
        "{note}"
    );
}

#[test]
fn copies_the_history_with_what_is_still_in_its_write_ahead_log() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "old-name");
    let current = one_folder(dir.path(), "new-name");
    previous_settings(&previous);
    // Kept open, with automatic checkpoints off, as an older copy still running would: the
    // rows stay in the write-ahead log, not in the database file.
    let writer = Connection::open(previous.settings.join(HISTORY_FILE_NAME)).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0; PRAGMA user_version = 7;
             CREATE TABLE history (sql TEXT);
             INSERT INTO history VALUES ('SELECT 1'), ('SELECT 2');",
        )
        .unwrap();
    assert!(with_suffix(&previous.settings.join(HISTORY_FILE_NAME), WAL_SUFFIX).exists());

    let copied = adopt(&previous, &current);

    assert!(copied.history_read_as_files.is_none());
    let copy = Connection::open(current.settings.join(HISTORY_FILE_NAME)).unwrap();
    let statements: Vec<String> = copy
        .prepare("SELECT sql FROM history ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(statements, ["SELECT 1", "SELECT 2"]);
    let version: i64 = copy
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    drop(writer);
}

#[test]
fn copies_a_history_sqlite_cannot_read_as_files() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "old-name");
    let current = one_folder(dir.path(), "new-name");
    previous_settings(&previous);
    write(&previous.settings.join(HISTORY_FILE_NAME), "not a database");

    let copied = adopt(&previous, &current);

    assert!(copied.history_read_as_files.is_some());
    assert_eq!(
        fs::read_to_string(current.settings.join(HISTORY_FILE_NAME)).unwrap(),
        "not a database"
    );
}

#[test]
fn does_nothing_when_there_is_nothing_to_bring_over() {
    let dir = TempDir::new().unwrap();
    let previous = separate_folders(dir.path(), "old-name");
    let current = separate_folders(dir.path(), "new-name");
    let elsewhere = separate_folders(dir.path(), "never-installed");
    previous_settings(&previous);
    write(
        &current.settings.join("data_sources.toml"),
        "# already here\n",
    );

    let cases = [
        ("the current settings folder exists", &previous, &current),
        ("no previous install", &elsewhere, &current),
        ("the same name", &previous, &previous),
    ];
    for (case, from, to) in cases {
        let before = files_under(dir.path());
        let adoption = adopt_previous_install(from, to).unwrap();
        assert!(matches!(adoption, Adoption::NotNeeded), "{case}");
        assert_eq!(files_under(dir.path()), before, "{case}");
    }
}

#[test]
fn replaces_a_staging_folder_left_by_an_interrupted_copy() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "old-name");
    let current = one_folder(dir.path(), "new-name");
    previous_settings(&previous);
    write(
        &staging_folder(&current.settings).join("data_sources.toml"),
        "half a file",
    );

    adopt(&previous, &current);

    assert_eq!(
        fs::read_to_string(current.settings.join("data_sources.toml")).unwrap(),
        "[[data_source]]\n"
    );
    assert!(!staging_folder(&current.settings).exists());
}

/// Unix only: an unreadable file is the simplest way to make the copy fail half-way.
#[cfg(unix)]
#[test]
fn a_copy_that_fails_leaves_no_settings_folder_and_is_tried_again() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "old-name");
    let current = one_folder(dir.path(), "new-name");
    previous_settings(&previous);
    let unreadable = previous.settings.join("consoles/6f1c0d9e/scratch.sql");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let failed = adopt_previous_install(&previous, &current);
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(failed.is_err());
    assert!(!current.settings.exists());
    assert!(!previous.settings.join(note_file_name()).exists());
    adopt(&previous, &current);
    assert!(
        current
            .settings
            .join("consoles/6f1c0d9e/scratch.sql")
            .exists()
    );
}

#[test]
fn waits_for_another_process_copying_and_then_finds_the_copy_done() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "old-name");
    let current = one_folder(dir.path(), "new-name");
    previous_settings(&previous);
    let other_process = take_lock(&lock_file(&current.settings)).unwrap();

    let adopting = {
        let (previous, current) = (previous.clone(), current.clone());
        std::thread::spawn(move || adopt_previous_install(&previous, &current))
    };
    // However long this takes, the copy can't have started: the lock is held.
    std::thread::sleep(Duration::from_millis(100));
    assert!(!adopting.is_finished());
    assert!(!staging_folder(&current.settings).exists());
    // What the other process would have committed before letting go.
    fs::create_dir_all(&current.settings).unwrap();
    drop(other_process);

    let adoption = adopting.join().unwrap().unwrap();
    assert!(matches!(adoption, Adoption::NotNeeded));
    assert!(!previous.settings.join(note_file_name()).exists());
}

#[test]
fn splits_a_one_folder_install_so_no_trust_lands_in_the_settings_folder() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "dbrover");
    let current = dot_folders(dir.path());
    previous_settings(&previous);
    write(&previous.settings.join(HISTORY_FILE_NAME), "not a database");

    adopt(&previous, &current);

    let settings = files_under(&current.settings);
    let data = files_under(&current.data);
    for machine_file in MACHINE_FILES {
        assert!(
            !settings.contains_key(Path::new(machine_file)),
            "{machine_file}"
        );
        assert!(data.contains_key(Path::new(machine_file)), "{machine_file}");
    }
    let mut expected_settings = files_under(&previous.settings);
    expected_settings.retain(|path, _| {
        !path.starts_with("logs")
            && !path.ends_with(note_file_name())
            && !MACHINE_FILES.iter().any(|file| path == Path::new(file))
    });
    assert_eq!(settings, expected_settings);
    assert_eq!(data.len(), MACHINE_FILES.len(), "{:?}", data.keys());
    assert!(!staging_folder(&current.data).exists());
}

#[test]
fn copies_a_two_folder_install_to_the_dot_folders() {
    let dir = TempDir::new().unwrap();
    let previous = separate_folders(dir.path(), "dbrover");
    let current = dot_folders(dir.path());
    previous_settings(&previous);

    adopt(&previous, &current);

    assert_eq!(
        files_under(&current.settings),
        files_under(&previous.settings)
            .into_iter()
            .filter(|(path, _)| !path.ends_with(note_file_name()))
            .collect()
    );
    let mut expected_data = files_under(&previous.data);
    expected_data.retain(|path, _| !path.starts_with("logs"));
    assert_eq!(files_under(&current.data), expected_data);
}

#[test]
fn keeps_a_data_folder_that_exists_and_still_copies_the_settings() {
    let dir = TempDir::new().unwrap();
    let previous = one_folder(dir.path(), "dbrover");
    let current = dot_folders(dir.path());
    previous_settings(&previous);
    write(&current.data.join("app.ron"), "(newer layout)\n");

    adopt(&previous, &current);

    assert_eq!(
        fs::read_to_string(current.data.join("app.ron")).unwrap(),
        "(newer layout)\n"
    );
    assert!(!current.data.join("trusted_commands.toml").exists());
    assert!(!current.settings.join("trusted_commands.toml").exists());
    assert!(current.settings.join("data_sources.toml").exists());
}

#[test]
fn finds_the_newest_previous_install_that_exists() {
    let dir = TempDir::new().unwrap();
    let current = dot_folders(dir.path());
    let platform = one_folder(dir.path(), "dbrover");
    let renamed = one_folder(dir.path(), "dbdesk");
    let candidates = || [current.clone(), platform.clone(), renamed.clone()];

    assert_eq!(find_previous_install(candidates(), &current), None);

    previous_settings(&renamed);
    assert_eq!(
        find_previous_install(candidates(), &current),
        Some(renamed.clone())
    );

    previous_settings(&platform);
    previous_settings(&current);
    assert_eq!(
        find_previous_install(candidates(), &current),
        Some(platform.clone())
    );
}
