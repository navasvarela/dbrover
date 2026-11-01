//! What choosing a folder in Settings would do, checked before anything changes.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::SETTINGS_FOLDER_ENTRIES;
use crate::previous_install::InstallFolders;

/// Finder writes it into any folder it shows; it holds nothing of the user's.
const FINDER_FILE: &str = ".DS_Store";

/// Created and removed again to learn whether a folder can be written.
const WRITE_CHECK_FILE: &str = ".dbrover-write-check";

/// What choosing a folder would do.
#[derive(Debug)]
pub enum FolderCheck {
    /// It holds DbRover's settings, which would be used as they are.
    HasSettings,
    /// It is empty, or not there yet in a folder that is: the settings can be copied there, or
    /// started afresh.
    Empty,
    /// It is the settings folder in use.
    InUse,
    Refused(FolderRefusal),
}

#[derive(Debug, thiserror::Error)]
pub enum FolderRefusal {
    #[error("it holds other files: choose an empty folder, or one that has DbRover's files")]
    OtherFiles,
    #[error("it is inside the settings or data folder DbRover uses now")]
    InsideCurrent,
    #[error("it isn't a folder")]
    NotAFolder,
    #[error("it can't be read")]
    Unreadable(#[source] io::Error),
    #[error("it can't be written")]
    NotWritable(#[source] io::Error),
}

/// What choosing `candidate` instead of `current`'s settings folder would do.
pub fn check_folder(candidate: &Path, current: &InstallFolders) -> FolderCheck {
    let candidate = canonical(candidate);
    if candidate == canonical(&current.settings) {
        return FolderCheck::InUse;
    }
    if candidate.starts_with(canonical(&current.settings))
        || candidate.starts_with(canonical(&current.data))
    {
        return FolderCheck::Refused(FolderRefusal::InsideCurrent);
    }
    match contents(&candidate) {
        Ok(Contents::Missing) if candidate.parent().is_some_and(Path::is_dir) => FolderCheck::Empty,
        Ok(Contents::Missing | Contents::NotAFolder) => {
            FolderCheck::Refused(FolderRefusal::NotAFolder)
        }
        Ok(Contents::Other) => FolderCheck::Refused(FolderRefusal::OtherFiles),
        Ok(found @ (Contents::Empty | Contents::Settings)) => match check_writable(&candidate) {
            Err(error) => FolderCheck::Refused(FolderRefusal::NotWritable(error)),
            Ok(()) if found == Contents::Empty => FolderCheck::Empty,
            Ok(()) => FolderCheck::HasSettings,
        },
        Err(error) => FolderCheck::Refused(FolderRefusal::Unreadable(error)),
    }
}

/// Removes `folder` if nothing of anyone's is in it, so a copy can be renamed into its place.
pub(super) fn remove_if_empty(folder: &Path) -> io::Result<()> {
    if contents(folder)? != Contents::Empty {
        return Ok(());
    }
    match fs::remove_file(folder.join(FINDER_FILE)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::remove_dir(folder)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Contents {
    Missing,
    NotAFolder,
    /// Nothing, or only Finder's file.
    Empty,
    /// At least one of DbRover's files or folders.
    Settings,
    Other,
}

fn contents(folder: &Path) -> io::Result<Contents> {
    if !folder.exists() {
        return Ok(Contents::Missing);
    }
    if !folder.is_dir() {
        return Ok(Contents::NotAFolder);
    }
    let mut found = Contents::Empty;
    for entry in fs::read_dir(folder)? {
        let name = entry?.file_name();
        if SETTINGS_FOLDER_ENTRIES
            .iter()
            .any(|entry| name == OsStr::new(entry))
        {
            return Ok(Contents::Settings);
        }
        if name != FINDER_FILE {
            found = Contents::Other;
        }
    }
    Ok(found)
}

fn check_writable(folder: &Path) -> io::Result<()> {
    let probe = folder.join(WRITE_CHECK_FILE);
    File::create_new(&probe)?;
    fs::remove_file(probe)
}

/// `path` with its links resolved, as far as it exists, so a folder reached through a link
/// (`/tmp` and `/private/tmp` on macOS) compares equal to itself.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).join(name),
        _ => path.to_owned(),
    }
}
