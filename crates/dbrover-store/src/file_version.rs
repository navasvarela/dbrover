//! Which version of a file DbRover last read or wrote, so a rewrite can tell whether another
//! program (a `git pull`, a synced folder) changed the file since (VC1b in
//! `docs/version-control.md`).

use std::path::Path;
use std::{fs, io};

use dbrover_core::BlobId;

use crate::atomic::write_atomically;

/// What a file held when DbRover last read or wrote it: its bytes' [`BlobId`], or no file.
///
/// The bytes are read and hashed before every check, so a change is noticed whatever the
/// file's modification time says: some synced folders and network drives keep the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileVersion {
    Missing,
    Blob(BlobId),
}

impl FileVersion {
    pub fn of(contents: Option<&[u8]>) -> Self {
        contents.map_or(Self::Missing, |bytes| Self::Blob(BlobId::of(bytes)))
    }
}

/// A file's text as read just now, and its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedText {
    /// `None` if there is no file.
    pub text: Option<String>,
    pub version: FileVersion,
}

/// Reads the file at `path` as UTF-8 text with its version. A missing file is no text.
///
/// # Errors
///
/// The file system's error, or [`io::ErrorKind::InvalidData`] if the file isn't UTF-8.
pub fn read_versioned(path: &Path) -> io::Result<VersionedText> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(VersionedText {
                text: None,
                version: FileVersion::Missing,
            });
        }
        Err(error) => return Err(error),
    };
    let version = FileVersion::of(Some(&bytes));
    let text = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.utf8_error()))?;
    Ok(VersionedText {
        text: Some(text),
        version,
    })
}

/// What a file held when it was read, ready to be changed and written back over: its value,
/// its version, and its text, which later saves edit in place.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot<T> {
    pub value: T,
    pub version: FileVersion,
    /// `None` if there was no file.
    pub(crate) text: Option<String>,
}

impl<T> Snapshot<T> {
    /// `value`, read from `read`'s text.
    pub(crate) fn new(read: VersionedText, value: T) -> Self {
        Self {
            value,
            version: read.version,
            text: read.text,
        }
    }
}

/// How a write that first checks the file went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedWrite {
    /// Written; the file's new version.
    Written(FileVersion),
    /// Not written: the file isn't the version expected. What it holds now.
    ChangedOnDisk(VersionedText),
}

/// Writes `contents` to `path` atomically, unless the file is no longer at `expected`, the
/// version DbRover last read or wrote.
///
/// Between the check and the rename another program can still write the file; the window is
/// one write of a small file, against minutes between a `git pull` and the next save.
///
/// # Errors
///
/// The file system's error, from reading the file or writing it.
pub fn write_if_unchanged(
    path: &Path,
    expected: FileVersion,
    contents: &str,
) -> io::Result<CheckedWrite> {
    let now = read_versioned(path)?;
    if now.version != expected {
        return Ok(CheckedWrite::ChangedOnDisk(now));
    }
    write_versioned(path, contents).map(CheckedWrite::Written)
}

/// Writes `contents` to `path` atomically, whatever it holds, and returns the new version.
///
/// # Errors
///
/// The file system's error.
pub(crate) fn write_versioned(path: &Path, contents: &str) -> io::Result<FileVersion> {
    write_atomically(path, contents)?;
    Ok(FileVersion::of(Some(contents.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_over_the_version_it_expects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");

        let written = write_if_unchanged(&path, FileVersion::Missing, "a = 1\n").unwrap();
        let CheckedWrite::Written(first) = written else {
            panic!("a missing file is the version expected: {written:?}");
        };
        assert_eq!(read_versioned(&path).unwrap().version, first);

        fs::write(&path, "a = 2\n").unwrap();
        let refused = write_if_unchanged(&path, first, "a = 3\n").unwrap();
        assert_eq!(
            refused,
            CheckedWrite::ChangedOnDisk(VersionedText {
                text: Some("a = 2\n".to_owned()),
                version: FileVersion::of(Some(b"a = 2\n")),
            })
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");

        fs::remove_file(&path).unwrap();
        let refused = write_if_unchanged(&path, first, "a = 3\n").unwrap();
        assert!(matches!(
            refused,
            CheckedWrite::ChangedOnDisk(VersionedText { text: None, .. })
        ));
    }

    #[test]
    fn a_file_that_isnt_utf8_is_invalid_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.toml");
        fs::write(&path, b"name = \"caf\xe9\"").unwrap();

        let error = read_versioned(&path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
