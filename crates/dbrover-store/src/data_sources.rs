use std::io;
use std::path::{Path, PathBuf};

use dbrover_core::DataSource;
use serde::{Deserialize, Serialize};

use crate::atomic::write_atomically;
use crate::file_version::{
    CheckedWrite, FileVersion, Snapshot, read_versioned, write_if_unchanged,
};
use crate::in_place::{Header, InPlaceError, edit_in_place};

const HEADER: Header = Header {
    current: "\
# DbRover data sources. Edit by hand or keep in Git: DbRover picks up changes when its
# window comes back to the front, and its saves keep your comments and order.
# Passwords are stored in the OS keychain, never in this file.

",
    earlier: &["\
# DbRover data sources. Safe to edit by hand while DbRover is closed.
# Passwords are stored in the OS keychain, never in this file.
# Comments are not preserved when DbRover saves this file.

"],
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a valid data sources file", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize the data sources")]
    Serialize(#[source] toml::ser::Error),
    #[error("could not write the data sources into {}", path.display())]
    Edit {
        path: PathBuf,
        #[source]
        source: InPlaceError,
    },
    /// Another program wrote the file between DbRover reading it and writing it back.
    #[error("{} changed on disk while it was being saved; try again", path.display())]
    ChangedWhileSaving { path: PathBuf },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The `data_sources.toml` file: one `[[data_source]]` table per data source.
#[derive(Debug, Clone)]
pub struct DataSourceFile {
    path: PathBuf,
}

#[derive(Deserialize)]
struct FileContents {
    #[serde(default, rename = "data_source")]
    data_sources: Vec<DataSource>,
}

#[derive(Serialize)]
struct FileContentsRef<'a> {
    #[serde(rename = "data_source")]
    data_sources: &'a [DataSource],
}

impl DataSourceFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all data sources. A missing file means none have been saved yet.
    ///
    /// # Errors
    ///
    /// [`StoreError::Read`] or [`StoreError::Parse`] if the file exists but can't be used.
    pub fn load(&self) -> Result<Vec<DataSource>, StoreError> {
        self.read().map(|snapshot| snapshot.value)
    }

    /// Reads all data sources with the file's version, to [`write_over`](Self::write_over)
    /// later.
    ///
    /// # Errors
    ///
    /// [`StoreError::Read`] or [`StoreError::Parse`] if the file exists but can't be used.
    pub fn read(&self) -> Result<Snapshot<Vec<DataSource>>, StoreError> {
        let read = read_versioned(&self.path).map_err(|source| StoreError::Read {
            path: self.path.clone(),
            source,
        })?;
        let data_sources = match &read.text {
            Some(text) => {
                let contents: FileContents =
                    toml::from_str(text).map_err(|source| StoreError::Parse {
                        path: self.path.clone(),
                        source,
                    })?;
                contents.data_sources
            }
            None => Vec::new(),
        };
        Ok(Snapshot::new(read, data_sources))
    }

    /// Writes `data_sources` over the file `read` came from, and returns its new version.
    ///
    /// # Errors
    ///
    /// [`StoreError::ChangedWhileSaving`] if the file is no longer what `read` holds, and
    /// [`StoreError::Write`] if it can't be written.
    pub fn write_over(
        &self,
        read: &Snapshot<Vec<DataSource>>,
        data_sources: &[DataSource],
    ) -> Result<FileVersion, StoreError> {
        let text = edit_in_place(
            read.text.as_deref(),
            &FileContentsRef {
                data_sources: &read.value,
            },
            &FileContentsRef { data_sources },
            &HEADER,
        )
        .map_err(|source| StoreError::Edit {
            path: self.path.clone(),
            source,
        })?;
        let written = write_if_unchanged(&self.path, read.version, &text).map_err(|source| {
            StoreError::Write {
                path: self.path.clone(),
                source,
            }
        })?;
        match written {
            CheckedWrite::Written(version) => Ok(version),
            CheckedWrite::ChangedOnDisk(_) => Err(StoreError::ChangedWhileSaving {
                path: self.path.clone(),
            }),
        }
    }

    fn text_of(&self, data_sources: &[DataSource]) -> Result<String, StoreError> {
        let body =
            toml::to_string(&FileContentsRef { data_sources }).map_err(StoreError::Serialize)?;
        Ok(format!("{}{body}", HEADER.current))
    }

    /// Replaces the file's contents with `data_sources`.
    ///
    /// Written atomically: a crash mid-write leaves either the old or the new file.
    ///
    /// # Errors
    ///
    /// [`StoreError::Write`] if the directory or file can't be written.
    pub fn save(&self, data_sources: &[DataSource]) -> Result<(), StoreError> {
        write_atomically(&self.path, &self.text_of(data_sources)?).map_err(|source| {
            StoreError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use dbrover_core::{ConnectionConfig, DataSourceId, SqliteConfig};

    use super::*;

    fn sqlite(name: &str) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: format!("/data/{name}.db").into(),
                read_only: false,
            }),
        }
    }

    #[test]
    fn missing_file_has_no_data_sources() {
        let dir = tempfile::tempdir().unwrap();

        let loaded = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();

        assert!(loaded.is_empty());
    }

    #[test]
    fn saves_and_loads_creating_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = DataSourceFile::new(dir.path().join("nested/data_sources.toml"));
        let data_sources = vec![sqlite("one"), sqlite("two")];

        file.save(&data_sources).unwrap();

        assert_eq!(file.load().unwrap(), data_sources);
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.starts_with("# DbRover data sources."), "{text}");
        assert!(!file.path().with_extension("toml.tmp").exists());
    }

    #[test]
    fn invalid_file_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = DataSourceFile::new(dir.path().join("data_sources.toml"));
        fs::write(file.path(), "[[data_source]]\nname = 3").unwrap();

        assert!(matches!(file.load(), Err(StoreError::Parse { .. })));
    }

    #[test]
    fn writes_only_over_the_file_as_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = DataSourceFile::new(dir.path().join("data_sources.toml"));
        let read = file.read().unwrap();
        assert_eq!(read.version, FileVersion::Missing);
        let one = vec![sqlite("one")];
        let written = file.write_over(&read, &one).unwrap();

        let reread = file.read().unwrap();
        assert_eq!((&reread.value, reread.version), (&one, written));
        assert!(matches!(
            file.write_over(&read, &[]),
            Err(StoreError::ChangedWhileSaving { .. })
        ));
        assert_eq!(file.load().unwrap(), one);
    }

    #[test]
    fn a_save_changes_only_the_lines_of_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = DataSourceFile::new(dir.path().join("data_sources.toml"));
        let (local, remote) = (sqlite("local"), sqlite("remote"));
        file.save(&[local.clone(), remote.clone()]).unwrap();
        let written = fs::read_to_string(file.path()).unwrap();
        let by_hand = written
            .replace(
                "[[data_source]]\nid = \"",
                "# Kept in our team's repository.\n[[data_source]]\nid = \"",
            )
            .replace("name = \"remote\"", "name = \"remote\" # the shared one");
        fs::write(file.path(), &by_hand).unwrap();

        let write = |data_sources: &[DataSource]| {
            let read = file.read().unwrap();
            file.write_over(&read, data_sources).unwrap();
            fs::read_to_string(file.path()).unwrap()
        };
        let renamed = DataSource {
            name: "remote eu".to_owned(),
            ..remote.clone()
        };
        assert_eq!(
            write(&[local.clone(), renamed.clone()]),
            by_hand.replace(
                "\"remote\" # the shared one",
                "\"remote eu\" # the shared one"
            )
        );

        let added = sqlite("added");
        let with_added = write(&[local.clone(), renamed.clone(), added.clone()]);
        assert!(
            with_added.starts_with(&by_hand.replace("\"remote\" #", "\"remote eu\" #")),
            "{with_added}"
        );
        assert!(
            with_added.ends_with("path = \"/data/added.db\"\nread_only = false\n"),
            "{with_added}"
        );

        let without_local = write(&[renamed.clone(), added.clone()]);
        assert!(!without_local.contains("local"), "{without_local}");
        assert_eq!(
            without_local
                .matches("# Kept in our team's repository.")
                .count(),
            1
        );
        assert_eq!(file.load().unwrap(), [renamed, added]);
    }

    #[test]
    fn replaces_the_header_earlier_versions_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let file = DataSourceFile::new(dir.path().join("data_sources.toml"));
        let one = sqlite("one");
        file.save(std::slice::from_ref(&one)).unwrap();
        let body = fs::read_to_string(file.path())
            .unwrap()
            .replace(HEADER.current, "");
        fs::write(file.path(), format!("{}{body}", HEADER.earlier[0])).unwrap();

        let read = file.read().unwrap();
        file.write_over(&read, &[one, sqlite("two")]).unwrap();

        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.starts_with(HEADER.current), "{text}");
        assert!(!text.contains("Comments are not preserved"), "{text}");
    }
}
