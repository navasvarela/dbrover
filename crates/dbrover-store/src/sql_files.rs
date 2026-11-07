//! `.sql` files the user opens from anywhere on disk, unlike console files, which live in the
//! config folder. Read whole, as UTF-8, and written back atomically.

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

use crate::atomic::write_atomically;

/// A `.sql` file's text, and when it was last modified, to notice later changes made by other
/// programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlFileContents {
    pub text: String,
    /// `None` where the file system doesn't keep modification times.
    pub modified: Option<SystemTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum SqlFileError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Shown rather than read with replacement characters, which saving would then write over
    /// the file's own bytes.
    #[error("{} isn't UTF-8 text", path.display())]
    NotUtf8 { path: PathBuf },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is already there", path.display())]
    Exists { path: PathBuf },
}

/// Reads the file at `path`.
///
/// # Errors
///
/// [`SqlFileError::Read`] if it can't be read, [`SqlFileError::NotUtf8`] if it isn't text in
/// UTF-8.
pub fn read_sql_file(path: &Path) -> Result<SqlFileContents, SqlFileError> {
    let read_error = |source| SqlFileError::Read {
        path: path.to_owned(),
        source,
    };
    let bytes = fs::read(path).map_err(read_error)?;
    let text = String::from_utf8(bytes).map_err(|_| SqlFileError::NotUtf8 {
        path: path.to_owned(),
    })?;
    Ok(SqlFileContents {
        text,
        modified: modified(path).map_err(read_error)?,
    })
}

/// Writes `text` to `path`, replacing the file in one step, and returns the file's new
/// modification time.
///
/// # Errors
///
/// [`SqlFileError::Write`] if the file or its folder can't be written.
pub fn write_sql_file(path: &Path, text: &str) -> Result<Option<SystemTime>, SqlFileError> {
    let write_error = |source| SqlFileError::Write {
        path: path.to_owned(),
        source,
    };
    write_atomically(path, text).map_err(write_error)?;
    modified(path).map_err(write_error)
}

/// Creates an empty file at `path`, for **New query here** (VC2). An existing file is never
/// replaced, even one made between asking for the name and creating it.
///
/// # Errors
///
/// [`SqlFileError::Exists`] if there is a file at `path` already, [`SqlFileError::Write`] if
/// it can't be created.
pub fn create_sql_file(path: &Path) -> Result<(), SqlFileError> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => Err(SqlFileError::Exists {
            path: path.to_owned(),
        }),
        Err(source) => Err(SqlFileError::Write {
            path: path.to_owned(),
            source,
        }),
    }
}

/// When the file at `path` was last modified: `Ok(None)` where the file system doesn't say.
///
/// # Errors
///
/// The file system's error, `NotFound` if there is no file.
pub fn modified(path: &Path) -> io::Result<Option<SystemTime>> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_wrote_with_its_modification_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/monthly.sql");

        let written = write_sql_file(&path, "select 1;\r\nselect 'é';\n").unwrap();
        let read = read_sql_file(&path).unwrap();

        assert_eq!(read.text, "select 1;\r\nselect 'é';\n");
        assert_eq!(read.modified, written);
        assert_eq!(modified(&path).unwrap(), written);
        assert!(!dir.path().join("reports/monthly.sql.tmp").exists());
    }

    #[test]
    fn creates_an_empty_file_but_never_over_another() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refunds.sql");

        create_sql_file(&path).unwrap();
        assert_eq!(read_sql_file(&path).unwrap().text, "");

        fs::write(&path, "select 1;").unwrap();
        assert!(matches!(
            create_sql_file(&path),
            Err(SqlFileError::Exists { .. })
        ));
        assert_eq!(read_sql_file(&path).unwrap().text, "select 1;");
        assert!(matches!(
            create_sql_file(&dir.path().join("gone").join("x.sql")),
            Err(SqlFileError::Write { .. })
        ));
    }

    #[test]
    fn refuses_a_missing_file_and_one_that_isnt_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let latin1 = dir.path().join("latin1.sql");
        fs::write(&latin1, b"select 'caf\xe9';").unwrap();

        assert!(matches!(
            read_sql_file(&dir.path().join("gone.sql")),
            Err(SqlFileError::Read { .. })
        ));
        assert!(matches!(
            read_sql_file(&latin1),
            Err(SqlFileError::NotUtf8 { .. })
        ));
        assert_eq!(
            modified(&dir.path().join("gone.sql")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
