use std::ffi::OsString;
use std::fs;

use tempfile::TempDir;

use super::*;
use crate::Adoption;

fn chosen(folder: &Path, copy_from: Option<&Path>) -> ChosenFolder {
    ChosenFolder {
        folder: folder.to_owned(),
        copy_from: copy_from.map(Path::to_owned),
    }
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// An absolute path on every platform: `/srv` alone has no drive on Windows.
fn absolute(path: &str) -> PathBuf {
    std::env::temp_dir().join(path)
}

#[test]
fn takes_the_flag_then_the_variable_then_the_choice_then_the_default() {
    let home = absolute("home/ada");
    let flag = absolute("flag").into_os_string();
    let variable = OsString::from("~/synced/dbrover");
    let empty = OsString::new();
    let choice = chosen(&absolute("chosen"), None);
    let overrides = |flag, environment| FolderOverrides { flag, environment };

    let cases = [
        (
            overrides(Some(flag.as_os_str()), Some(variable.as_os_str())),
            Some(choice.clone()),
            SettingsFolder::Flag(absolute("flag")),
        ),
        (
            overrides(None, Some(variable.as_os_str())),
            Some(choice.clone()),
            SettingsFolder::Environment(home.join("synced").join("dbrover")),
        ),
        (
            overrides(None, Some(empty.as_os_str())),
            Some(choice.clone()),
            SettingsFolder::Chosen(choice.clone()),
        ),
        (overrides(None, None), None, SettingsFolder::Default),
    ];
    for (overrides, chosen, expected) in cases {
        assert_eq!(
            settings_folder(overrides, chosen, &home).unwrap(),
            expected,
            "{overrides:?}"
        );
    }
}

#[test]
fn expands_the_home_folder_and_refuses_relative_paths() {
    let home = absolute("home/ada");
    let elsewhere = absolute("etc/dbrover");
    let cases = [
        (OsString::from("~"), Ok(home.clone())),
        (OsString::from("~/dbrover"), Ok(home.join("dbrover"))),
        (elsewhere.clone().into_os_string(), Ok(elsewhere)),
        (OsString::from("dbrover"), Err(())),
        (OsString::from("./dbrover"), Err(())),
        (OsString::from("~ada/dbrover"), Err(())),
    ];
    for (given, expected) in cases {
        let overrides = FolderOverrides {
            flag: Some(&given),
            environment: None,
        };
        let folder = settings_folder(overrides, None, &home);
        match expected {
            Ok(path) => assert_eq!(folder, Ok(SettingsFolder::Flag(path)), "{given:?}"),
            Err(()) => {
                let error = folder.unwrap_err().to_string();
                assert!(error.contains("--config-dir"), "{error}");
            }
        }
    }
}

#[test]
fn reads_writes_and_forgets_the_choice() {
    let dir = TempDir::new().unwrap();
    let file = ChosenFolderFile::new(dir.path().join("data/settings_folder.toml"));
    assert_eq!(file.load().unwrap(), None);
    file.forget().unwrap();

    let choice = chosen(
        Path::new("/mnt/chosen"),
        Some(Path::new("/home/ada/.config/x")),
    );
    file.save(&choice).unwrap();
    assert_eq!(file.load().unwrap(), Some(choice.clone()));
    file.save(&choice.copied()).unwrap();
    let text = fs::read_to_string(file.path()).unwrap();
    assert!(!text.contains("copy_from"), "{text}");
    assert_eq!(file.load().unwrap(), Some(choice.copied()));

    file.forget().unwrap();
    assert_eq!(file.load().unwrap(), None);

    fs::write(file.path(), "folder = \"/x\"\nfolders = []\n").unwrap();
    assert!(matches!(file.load(), Err(ChosenFolderError::Parse { .. })));
}

#[test]
fn a_chosen_folder_is_available_when_it_is_there_or_a_copy_will_make_it() {
    let dir = TempDir::new().unwrap();
    let there = dir.path().join("there");
    fs::create_dir(&there).unwrap();
    let not_yet = dir.path().join("not-yet");
    let unmounted = dir.path().join("Volumes/Backup/dbrover");
    let from = Some(dir.path());

    assert!(chosen(&there, None).is_available());
    assert!(chosen(&there, from).is_available());
    assert!(!chosen(&not_yet, None).is_available());
    assert!(chosen(&not_yet, from).is_available());
    assert!(!chosen(&unmounted, from).is_available());
}

/// The settings folder and data folder in use, with a few settings.
fn current(root: &Path) -> InstallFolders {
    let folders = InstallFolders {
        settings: root.join(".config/dbrover"),
        data: root.join(".local/share/dbrover"),
    };
    write(
        &folders.settings.join("data_sources.toml"),
        "[[data_source]]\n",
    );
    write(
        &folders.settings.join("consoles/6f1c/scratch.sql"),
        "SELECT 1;\n",
    );
    write(&folders.data.join("trusted_commands.toml"), "[[command]]\n");
    folders
}

#[test]
fn copies_into_the_chosen_folder_once_and_leaves_the_data_folder() {
    let dir = TempDir::new().unwrap();
    let current = current(dir.path());
    let target = dir.path().join("Dropbox/dbrover");
    fs::create_dir_all(&target).unwrap();
    write(&target.join(".DS_Store"), "");
    let choice = chosen(&target, Some(&current.settings));

    let adoption = choice.copy_pending(&current.data).unwrap();

    assert!(matches!(adoption, Adoption::Copied(_)));
    assert_eq!(
        fs::read_to_string(target.join("consoles/6f1c/scratch.sql")).unwrap(),
        "SELECT 1;\n"
    );
    assert!(!target.join("trusted_commands.toml").exists());
    assert!(current.data.join("trusted_commands.toml").exists());
    assert!(current.settings.join("data_sources.toml").exists());

    // A start that crashed before forgetting `copy_from` finds nothing left to do.
    write(
        &current.settings.join("data_sources.toml"),
        "# changed since\n",
    );
    let again = choice.copy_pending(&current.data).unwrap();
    assert!(matches!(again, Adoption::NotNeeded));
    assert_eq!(
        fs::read_to_string(target.join("data_sources.toml")).unwrap(),
        "[[data_source]]\n"
    );
}

#[test]
fn copies_nothing_without_a_pending_copy() {
    let dir = TempDir::new().unwrap();
    let current = current(dir.path());
    let target = dir.path().join("empty");
    fs::create_dir(&target).unwrap();

    let adoption = chosen(&target, None).copy_pending(&current.data).unwrap();

    assert!(matches!(adoption, Adoption::NotNeeded));
    assert!(target.is_dir());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn checks_a_folder_before_it_is_chosen() {
    let dir = TempDir::new().unwrap();
    let current = current(dir.path());
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let finder_only = dir.path().join("finder-only");
    write(&finder_only.join(".DS_Store"), "");
    let other_install = dir.path().join("other-install");
    write(&other_install.join("keymap.toml"), "");
    let documents = dir.path().join("Documents");
    write(&documents.join("letter.txt"), "");
    let file = dir.path().join("a-file");
    write(&file, "");

    let check = |folder: &Path| check_folder(folder, &current);
    assert!(matches!(check(&empty), FolderCheck::Empty));
    assert!(matches!(check(&finder_only), FolderCheck::Empty));
    assert!(matches!(check(&dir.path().join("new")), FolderCheck::Empty));
    assert!(matches!(check(&other_install), FolderCheck::HasSettings));
    assert!(matches!(check(&current.settings), FolderCheck::InUse));
    assert!(matches!(
        check(&documents),
        FolderCheck::Refused(FolderRefusal::OtherFiles)
    ));
    for inside in [current.settings.join("consoles"), current.data.join("new")] {
        assert!(
            matches!(
                check(&inside),
                FolderCheck::Refused(FolderRefusal::InsideCurrent)
            ),
            "{}",
            inside.display()
        );
    }
    for not_a_folder in [file, dir.path().join("Volumes/Backup/dbrover")] {
        assert!(
            matches!(
                check(&not_a_folder),
                FolderCheck::Refused(FolderRefusal::NotAFolder)
            ),
            "{}",
            not_a_folder.display()
        );
    }
    assert_eq!(
        fs::read_dir(&empty).unwrap().count(),
        0,
        "the write check left nothing"
    );
}

/// Unix only: a folder without write permission is the simplest unwritable folder.
#[cfg(unix)]
#[test]
fn refuses_a_folder_that_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new().unwrap();
    let current = current(dir.path());
    let read_only = dir.path().join("read-only");
    fs::create_dir(&read_only).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();

    let check = check_folder(&read_only, &current);
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        matches!(check, FolderCheck::Refused(FolderRefusal::NotWritable(_))),
        "{check:?}"
    );
}
