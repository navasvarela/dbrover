//! ER diagram files: `diagrams/<project-slug>/<name>.toml`, or `diagrams/_none/<name>.toml`
//! for a data source in no project.

use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use dbrover_core::DataSourceId;
use serde::{Deserialize, Serialize};

use crate::ProjectSlug;
use crate::atomic::write_atomically;

const EXTENSION: &str = "toml";
const HEADER: &str = "\
# A DbRover ER diagram: its data source, and where each table's box sits. Each table is
# described again when the diagram opens. DbRover rewrites this file as the diagram changes,
# dropping comments.

";

/// The folder of diagrams made for data sources in no project. Never a project's slug, which
/// has no underscores.
const NO_PROJECT_FOLDER: &str = "_none";

/// Which folder of `diagrams/` a diagram is in: its data source's project's, when it was made.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagramFolder {
    Project(ProjectSlug),
    NoProject,
}

impl DiagramFolder {
    pub fn of(project: Option<ProjectSlug>) -> Self {
        project.map_or(Self::NoProject, Self::Project)
    }

    /// The folder a name in `diagrams/` stands for, if it stands for one.
    fn named(name: &str) -> Option<Self> {
        if name == NO_PROJECT_FOLDER {
            return Some(Self::NoProject);
        }
        ProjectSlug::try_from(name.to_owned())
            .ok()
            .map(Self::Project)
    }

    /// The folder's name in `diagrams/`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Project(slug) => slug.as_str(),
            Self::NoProject => NO_PROJECT_FOLDER,
        }
    }
}

/// A diagram file's name, without `.toml`. Always a plain file name, so joining it to its
/// folder can never point outside it, whatever a saved layout says.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DiagramName(String);

#[derive(Debug, thiserror::Error)]
#[error("{0:?} can't be a diagram file name")]
pub struct InvalidDiagramName(String);

impl DiagramName {
    /// A name made from `text`, a schema's or a table's name: what a file name can't hold
    /// becomes `_`, and `diagram` stands in for nothing usable.
    pub fn from_text(text: &str) -> Self {
        let name: String = text
            .trim()
            .chars()
            .map(|c| match c {
                '/' | '\\' | '\0' | ':' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect();
        let name = name.trim_start_matches('.');
        Self::try_from(name.to_owned()).unwrap_or_else(|_| Self("diagram".to_owned()))
    }

    /// This name with `_2`, `_3`… added.
    pub fn numbered(&self, number: usize) -> Self {
        Self(format!("{}_{number}", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DiagramName {
    type Error = InvalidDiagramName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let plain = !name.is_empty()
            && !name.starts_with('.')
            && !name.contains(['/', '\\', '\0'])
            && Path::new(&name).file_name() == Some(name.as_ref());
        if plain {
            Ok(Self(name))
        } else {
            Err(InvalidDiagramName(name))
        }
    }
}

impl From<DiagramName> for String {
    fn from(name: DiagramName) -> Self {
        name.0
    }
}

impl fmt::Display for DiagramName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which diagram file: unique per folder and name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DiagramKey {
    pub folder: DiagramFolder,
    pub name: DiagramName,
}

/// What a diagram file holds: whose tables, and where each one's box sits. Never the tables'
/// columns or keys, which are described again when the diagram opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagramFile {
    pub data_source: DataSourceId,
    /// The schema the diagram was opened on, whose tables it names without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(rename = "table", default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<PlacedTable>,
}

/// A table of a diagram, and where its box's top-left corner sits, in whole points: a box a
/// fraction of a point off looks the same, and the file reads better. A table without a place
/// is placed by the layout when the diagram opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacedTable {
    pub schema: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDiagram {
    pub key: DiagramKey,
    pub file: DiagramFile,
}

#[derive(Debug, thiserror::Error)]
pub enum DiagramStoreError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a diagram DbRover can read", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write the diagram {name}")]
    Serialize {
        name: DiagramName,
        #[source]
        source: toml::ser::Error,
    },
}

/// The `diagrams/` folder.
#[derive(Debug, Clone)]
pub struct DiagramFiles {
    root: PathBuf,
}

impl DiagramFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self, key: &DiagramKey) -> PathBuf {
        self.root
            .join(key.folder.as_str())
            .join(format!("{}.{EXTENSION}", key.name))
    }

    /// Reads every diagram file. A missing folder means none were saved yet. Folders that are
    /// neither `_none` nor a project's slug, and files that aren't `.toml`, are skipped; each
    /// file that can't be read is an error of its own, so one bad file doesn't hide the
    /// others.
    ///
    /// # Errors
    ///
    /// [`DiagramStoreError::Read`] if the folder exists but can't be listed.
    pub fn load_all(
        &self,
    ) -> Result<Vec<Result<SavedDiagram, DiagramStoreError>>, DiagramStoreError> {
        let mut diagrams = Vec::new();
        for (folder, folder_path) in self.folders()? {
            for (name, path) in diagram_files(&folder_path)? {
                let key = DiagramKey {
                    folder: folder.clone(),
                    name,
                };
                diagrams.push(read(&path).map(|file| SavedDiagram { key, file }));
            }
        }
        Ok(diagrams)
    }

    fn folders(&self) -> Result<Vec<(DiagramFolder, PathBuf)>, DiagramStoreError> {
        Ok(list(&self.root)?
            .filter_map(|path| {
                let folder = DiagramFolder::named(path.file_name()?.to_str()?)?;
                path.is_dir().then_some((folder, path))
            })
            .collect())
    }

    /// Replaces the diagram's file, atomically.
    ///
    /// # Errors
    ///
    /// [`DiagramStoreError::Serialize`] if the diagram can't be written as TOML, and
    /// [`DiagramStoreError::Write`] if the folder or file can't be written.
    pub fn save(&self, key: &DiagramKey, file: &DiagramFile) -> Result<(), DiagramStoreError> {
        let body = toml::to_string(file).map_err(|source| DiagramStoreError::Serialize {
            name: key.name.clone(),
            source,
        })?;
        let path = self.path(key);
        write_atomically(&path, &format!("{HEADER}{body}"))
            .map_err(|source| DiagramStoreError::Write { path, source })
    }

    /// Removes the diagram's file. Succeeds if there was none.
    ///
    /// # Errors
    ///
    /// [`DiagramStoreError::Write`] if the file exists but can't be removed.
    pub fn delete(&self, key: &DiagramKey) -> Result<(), DiagramStoreError> {
        let path = self.path(key);
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(DiagramStoreError::Write {
                    path,
                    source: error,
                })
            }
            _ => Ok(()),
        }
    }
}

fn read(path: &Path) -> Result<DiagramFile, DiagramStoreError> {
    let text = fs::read_to_string(path).map_err(|source| DiagramStoreError::Read {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| DiagramStoreError::Parse {
        path: path.to_owned(),
        source,
    })
}

/// The `.toml` files directly in `folder`, by diagram name.
fn diagram_files(folder: &Path) -> Result<Vec<(DiagramName, PathBuf)>, DiagramStoreError> {
    Ok(list(folder)?
        .filter_map(|path| {
            if path.extension()? != EXTENSION || !path.is_file() {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_owned();
            let name = DiagramName::try_from(stem).ok()?;
            Some((name, path))
        })
        .collect())
}

/// The entries of `folder`, or none if it doesn't exist.
fn list(folder: &Path) -> Result<impl Iterator<Item = PathBuf>, DiagramStoreError> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(DiagramStoreError::Read {
                path: folder.to_owned(),
                source,
            });
        }
    };
    Ok(entries
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_from_any_text_as_plain_file_names() {
        let cases = [
            ("public", "public"),
            ("Sales Data", "Sales Data"),
            ("a/b\\c:d", "a_b_c_d"),
            ("..", "diagram"),
            (".hidden", "hidden"),
            ("  ", "diagram"),
            ("tab\there", "tab_here"),
        ];
        for (text, expected) in cases {
            assert_eq!(DiagramName::from_text(text).as_str(), expected, "{text:?}");
        }
    }

    #[test]
    fn a_name_that_isnt_a_plain_file_name_is_refused() {
        for name in ["", ".", "..", "a/b", "a\\b", ".x"] {
            assert!(DiagramName::try_from(name.to_owned()).is_err(), "{name:?}");
        }
    }

    #[test]
    fn data_sources_in_no_project_share_a_folder_no_slug_can_name() {
        let none = DiagramFolder::of(None);

        assert_eq!(none.as_str(), "_none");
        assert!(ProjectSlug::try_from(none.as_str().to_owned()).is_err());
    }

    fn key(folder: DiagramFolder, name: &str) -> DiagramKey {
        DiagramKey {
            folder,
            name: DiagramName::try_from(name.to_owned()).unwrap(),
        }
    }

    fn shop(data_source: DataSourceId) -> DiagramFile {
        DiagramFile {
            data_source,
            schema: Some("public".to_owned()),
            tables: vec![
                PlacedTable {
                    schema: "public".to_owned(),
                    name: "customers".to_owned(),
                    x: Some(0),
                    y: Some(-12),
                },
                PlacedTable {
                    schema: "audit".to_owned(),
                    name: "log".to_owned(),
                    x: None,
                    y: None,
                },
            ],
        }
    }

    #[test]
    fn saves_and_reads_diagrams_in_their_projects_folders() {
        let dir = tempfile::tempdir().unwrap();
        let files = DiagramFiles::new(dir.path().join("diagrams"));
        let slug = ProjectSlug::try_from("shop-eu".to_owned()).unwrap();
        let in_project = key(DiagramFolder::Project(slug), "public");
        let alone = key(DiagramFolder::NoProject, "public");
        let data_source = DataSourceId::generate();

        files.save(&in_project, &shop(data_source)).unwrap();
        files.save(&alone, &shop(data_source)).unwrap();

        assert!(dir.path().join("diagrams/shop-eu/public.toml").is_file());
        assert!(dir.path().join("diagrams/_none/public.toml").is_file());
        let mut loaded: Vec<SavedDiagram> = files
            .load_all()
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        loaded.sort_by(|a, b| a.key.cmp(&b.key));
        let keys: Vec<&DiagramKey> = loaded.iter().map(|saved| &saved.key).collect();
        assert_eq!(keys, [&in_project, &alone]);
        assert_eq!(loaded[0].file, shop(data_source));

        files.delete(&alone).unwrap();
        files.delete(&alone).unwrap();
        assert_eq!(files.load_all().unwrap().len(), 1);
    }

    #[test]
    fn writes_a_file_people_can_read_and_keep_in_git() {
        let dir = tempfile::tempdir().unwrap();
        let files = DiagramFiles::new(dir.path());
        let diagram = key(DiagramFolder::NoProject, "shop");
        let id: DataSourceId = "6f1c0d9e-3b8a-4c3e-9f57-2d1e8a7b5c40".parse().unwrap();

        files.save(&diagram, &shop(id)).unwrap();

        let text = fs::read_to_string(files.path(&diagram)).unwrap();
        let body = text.strip_prefix(HEADER).unwrap();
        assert_eq!(
            body,
            "data_source = \"6f1c0d9e-3b8a-4c3e-9f57-2d1e8a7b5c40\"\n\
             schema = \"public\"\n\
             \n\
             [[table]]\n\
             schema = \"public\"\n\
             name = \"customers\"\n\
             x = 0\n\
             y = -12\n\
             \n\
             [[table]]\n\
             schema = \"audit\"\n\
             name = \"log\"\n"
        );
    }

    #[test]
    fn a_file_that_cant_be_read_is_reported_without_hiding_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let files = DiagramFiles::new(dir.path());
        let good = key(DiagramFolder::NoProject, "good");
        files.save(&good, &shop(DataSourceId::generate())).unwrap();
        fs::write(dir.path().join("_none/broken.toml"), "data_source = 3").unwrap();
        fs::create_dir_all(dir.path().join("Not A Slug")).unwrap();
        fs::write(dir.path().join("Not A Slug/x.toml"), "").unwrap();
        fs::write(dir.path().join("_none/notes.txt"), "").unwrap();

        let loaded = files.load_all().unwrap();

        assert_eq!(loaded.len(), 2);
        let broken = loaded.iter().filter(|result| result.is_err()).count();
        assert_eq!(broken, 1);
    }

    #[test]
    fn no_folder_means_no_diagrams() {
        let dir = tempfile::tempdir().unwrap();

        let loaded = DiagramFiles::new(dir.path().join("diagrams"))
            .load_all()
            .unwrap();

        assert!(loaded.is_empty());
    }
}
