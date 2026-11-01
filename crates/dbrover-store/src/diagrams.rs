//! ER diagram files: `diagrams/<project-slug>/<name>.toml`, or `diagrams/_none/<name>.toml`
//! for a data source in no project.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ProjectSlug;

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
}
