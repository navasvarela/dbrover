use std::fs;

use tempfile::TempDir;

use super::*;

/// DbRover's default folders under a temporary home, with one data source saved.
fn home_with_settings() -> (TempDir, AppPaths) {
    let home = TempDir::new().unwrap();
    let paths = AppPaths::under_home(home.path());
    fs::create_dir_all(&paths.folders().settings).unwrap();
    fs::write(paths.data_sources_file(), "[[data_source]]\n").unwrap();
    (home, paths)
}

fn choose(paths: &AppPaths, folder: &Path, copy_from: Option<&Path>) {
    paths
        .chosen_folder_file()
        .save(&ChosenFolder {
            folder: folder.to_owned(),
            copy_from: copy_from.map(Path::to_owned),
        })
        .unwrap();
}

fn decide_without_overrides(paths: &AppPaths) -> Decision {
    decide_with(paths.clone(), FolderOverrides::default()).unwrap()
}

#[test]
fn copies_into_the_chosen_folder_at_the_next_start_and_then_forgets_the_copy() {
    let (home, paths) = home_with_settings();
    let chosen = home.path().join("Dropbox/dbrover");
    fs::create_dir_all(&chosen).unwrap();
    choose(&paths, &chosen, Some(&paths.folders().settings));

    let session = decide_without_overrides(&paths).prepare(Vec::new());

    assert_eq!(
        session.paths().data_sources_file(),
        chosen.join("data_sources.toml")
    );
    assert_eq!(
        fs::read_to_string(chosen.join("data_sources.toml")).unwrap(),
        "[[data_source]]\n"
    );
    assert_eq!(session.paths().folders().data, paths.folders().data);
    assert!(matches!(
        session.notice(),
        Some(FolderNotice::Copied { .. })
    ));
    assert_eq!(session.folder_for_mcp_clients(), None);
    let kept = paths.chosen_folder_file().load().unwrap().unwrap();
    assert_eq!(kept.folder, chosen);
    assert_eq!(kept.copy_from, None);
}

#[test]
fn switching_back_to_the_default_folder_forgets_the_choice_once_copied() {
    let (home, paths) = home_with_settings();
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("keymap.toml"), "[bindings]\n").unwrap();
    fs::remove_dir_all(&paths.folders().settings).unwrap();
    choose(&paths, &paths.folders().settings, Some(&elsewhere));

    let session = decide_without_overrides(&paths).prepare(Vec::new());

    assert_eq!(session.paths().keymap_file(), paths.keymap_file());
    assert!(paths.keymap_file().exists());
    assert_eq!(paths.chosen_folder_file().load().unwrap(), None);
}

#[test]
fn uses_a_chosen_folder_with_nothing_to_copy_as_it_is() {
    let (home, paths) = home_with_settings();
    let chosen = home.path().join("work");
    fs::create_dir(&chosen).unwrap();
    choose(&paths, &chosen, None);

    let session = decide_without_overrides(&paths).prepare(Vec::new());

    assert_eq!(session.paths().folders().settings, chosen);
    assert!(session.notice().is_none());
    assert_eq!(fs::read_dir(&chosen).unwrap().count(), 0);
}

#[test]
fn falls_back_to_the_default_folder_when_the_chosen_one_is_missing() {
    let (home, paths) = home_with_settings();
    let unmounted = home.path().join("Volumes/Backup/dbrover");
    choose(&paths, &unmounted, None);

    let decision = decide_without_overrides(&paths);
    assert!(matches!(
        decision.unavailable(),
        Some(Unavailable::Missing(_))
    ));
    let session = decision.prepare(Vec::new());

    assert_eq!(session.paths().folders(), paths.folders());
    assert!(!unmounted.exists(), "a missing folder isn't created");
    let Some(FolderNotice::ChoiceUnavailable { problem, default }) = session.notice() else {
        panic!("expected the choice to be reported unavailable");
    };
    assert!(
        problem.contains(&unmounted.display().to_string()),
        "{problem}"
    );
    assert_eq!(default, paths.folders().settings);
    assert!(
        paths.chosen_folder_file().load().unwrap().is_some(),
        "the choice is kept"
    );
}

#[test]
fn falls_back_to_the_default_folder_when_the_choice_cannot_be_read() {
    let (_home, paths) = home_with_settings();
    let file = paths.chosen_folder_file();
    fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    fs::write(file.path(), "not = [toml").unwrap();

    let decision = decide_without_overrides(&paths);

    assert!(matches!(
        decision.unavailable(),
        Some(Unavailable::Unreadable(_))
    ));
    assert_eq!(
        decision.prepare(Vec::new()).paths().folders(),
        paths.folders()
    );
}

#[test]
fn the_flag_wins_and_is_passed_on_to_mcp_clients() {
    let (home, paths) = home_with_settings();
    let unmounted = home.path().join("Volumes/Backup/dbrover");
    choose(&paths, &unmounted, None);
    let flag = home.path().join("profile");
    let overrides = FolderOverrides {
        flag: Some(flag.as_os_str()),
        environment: None,
    };

    let decision = decide_with(paths.clone(), overrides).unwrap();
    assert!(decision.unavailable().is_none(), "the choice isn't used");
    let session = decision.prepare(Vec::new());

    assert_eq!(session.paths().folders().settings, flag);
    assert_eq!(session.folder_for_mcp_clients(), Some(flag));
    assert!(session.notice().is_none());
}

/// Unix only: an unreadable file is the simplest way to make the copy fail.
#[cfg(unix)]
#[test]
fn a_failed_copy_keeps_the_previous_folder_and_tries_again_next_time() {
    use std::os::unix::fs::PermissionsExt as _;

    let (home, paths) = home_with_settings();
    let chosen = home.path().join("Dropbox/dbrover");
    fs::create_dir_all(&chosen).unwrap();
    choose(&paths, &chosen, Some(&paths.folders().settings));
    let unreadable = paths.data_sources_file();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let session = decide_without_overrides(&paths).prepare(Vec::new());
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(session.paths().folders(), paths.folders());
    assert!(matches!(
        session.notice(),
        Some(FolderNotice::Failed { .. })
    ));
    let kept = paths.chosen_folder_file().load().unwrap().unwrap();
    assert_eq!(
        kept.copy_from.as_deref(),
        Some(paths.folders().settings.as_path())
    );
}
