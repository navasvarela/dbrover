//! The files that customise DbRover without a rebuild: `actions.toml`, `snippets.toml` and
//! `theme.toml`, next to `keymap.toml`. See `docs/hackability.md`.
//!
//! Each is read leniently, as `keymap.toml` is: an entry that doesn't parse is skipped and
//! listed as a problem, so one typo doesn't cost the rest of the file. A missing file adds
//! nothing; one that isn't TOML is a single problem.

mod actions;
mod commands;
mod snippets;
mod theme;
mod trust;

use std::path::{Path, PathBuf};

use dbrover_core::DriverKind;

pub use actions::{ActionBody, ActionId, ActionTarget, CustomAction, parse_actions};
pub use commands::{CommandAction, CommandOutput, DEFAULT_COMMAND_TIMEOUT, StdinFormat};
pub use snippets::{Snippet, SnippetBody, parse_snippets};
pub use theme::{Rgb, ThemeOverrides, ThemeToken, parse_theme};
pub use trust::{TrustError, TrustedCommand, TrustedCommands, TrustedCommandsFile};

use crate::config_text::{create_new, read_if_exists};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CustomFileKind {
    Actions,
    Snippets,
    Theme,
}

impl CustomFileKind {
    pub const ALL: [Self; 3] = [Self::Actions, Self::Snippets, Self::Theme];

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Actions => "actions.toml",
            Self::Snippets => "snippets.toml",
            Self::Theme => "theme.toml",
        }
    }
}

/// What a file holds that could be used, and what couldn't, each problem a sentence for the
/// user naming the entry it is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked<T> {
    pub value: T,
    pub problems: Vec<String>,
}

/// One file, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomFile {
    Actions(Checked<Vec<CustomAction>>),
    Snippets(Checked<Vec<Snippet>>),
    Theme(Checked<ThemeOverrides>),
}

impl CustomFile {
    /// Checks `text` as the file `kind` names.
    pub fn parse(kind: CustomFileKind, text: &str) -> Self {
        match kind {
            CustomFileKind::Actions => Self::Actions(parse_actions(text)),
            CustomFileKind::Snippets => Self::Snippets(parse_snippets(text)),
            CustomFileKind::Theme => Self::Theme(parse_theme(text)),
        }
    }

    /// A file that couldn't be read: nothing from it, and why.
    fn unreadable(kind: CustomFileKind, problem: String) -> Self {
        Self::parse(kind, "").with_problem(problem)
    }

    /// The file with one more problem, about the file as a whole.
    pub fn with_problem(mut self, problem: String) -> Self {
        match &mut self {
            Self::Actions(checked) => checked.problems.push(problem),
            Self::Snippets(checked) => checked.problems.push(problem),
            Self::Theme(checked) => checked.problems.push(problem),
        }
        self
    }

    pub fn kind(&self) -> CustomFileKind {
        match self {
            Self::Actions(_) => CustomFileKind::Actions,
            Self::Snippets(_) => CustomFileKind::Snippets,
            Self::Theme(_) => CustomFileKind::Theme,
        }
    }

    pub fn problems(&self) -> &[String] {
        match self {
            Self::Actions(checked) => &checked.problems,
            Self::Snippets(checked) => &checked.problems,
            Self::Theme(checked) => &checked.problems,
        }
    }
}

/// Where the files are: the config folder, beside `keymap.toml`.
#[derive(Debug, Clone)]
pub struct CustomFiles {
    dir: PathBuf,
}

impl CustomFiles {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self, kind: CustomFileKind) -> PathBuf {
        self.dir.join(kind.file_name())
    }

    /// Reads and checks one file. Never fails: what can't be read is a problem in the result.
    pub fn read(&self, kind: CustomFileKind) -> CustomFile {
        let path = self.path(kind);
        match read_if_exists(&path) {
            Ok(text) => CustomFile::parse(kind, text.as_deref().unwrap_or_default()),
            Err(error) => {
                CustomFile::unreadable(kind, format!("could not read {}: {error}", path.display()))
            }
        }
    }

    /// Writes `text` as a new file of this kind. Never replaces an existing one.
    ///
    /// # Errors
    ///
    /// If the file exists or can't be written.
    pub fn create(&self, kind: CustomFileKind, text: &str) -> std::io::Result<()> {
        create_new(&self.path(kind), text)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// The file as a TOML table, or why it isn't one.
fn toml_table(text: &str) -> Result<toml::Table, String> {
    toml::from_str(text).map_err(|error| {
        let line = error
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
        match line {
            Some(line) => format!("line {line}: {}", error.message().trim()),
            None => error.message().trim().to_owned(),
        }
    })
}

/// The `[[key]]` tables of a file whose only content is a list of them, with problems for
/// whatever else it holds.
fn entries(text: &str, key: &str, problems: &mut Vec<String>) -> Vec<toml::Value> {
    let table = match toml_table(text) {
        Ok(table) => table,
        Err(problem) => {
            problems.push(problem);
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for (name, value) in table {
        match value {
            toml::Value::Array(list) if name == key => entries.extend(list),
            _ if name == key => problems.push(format!("{key} must be a list of [[{key}]] tables")),
            _ => problems.push(format!(
                "{name:?} is not part of this file; write each entry as a [[{key}]] table"
            )),
        }
    }
    entries
}

/// How an entry is named in problems: by the given field if it has one, else by its place.
fn entry_label(kind: &str, entry: &toml::Value, field: &str, index: usize) -> String {
    match entry.get(field).and_then(toml::Value::as_str) {
        Some(name) => format!("{kind} {name:?}"),
        None => format!("{kind} {}", index + 1),
    }
}

/// What serde says is wrong with an entry, without the TOML position it doesn't have here.
fn serde_problem(error: &toml::de::Error) -> String {
    error.message().trim().to_owned()
}

/// A `drivers` list as `data_sources.toml` names drivers. `None` for every driver.
fn drivers(names: Option<Vec<String>>) -> Result<Option<Vec<DriverKind>>, String> {
    let Some(names) = names else {
        return Ok(None);
    };
    names
        .iter()
        .map(|name| {
            DriverKind::ALL
                .into_iter()
                .find(|&driver| driver_id(driver) == name)
                .ok_or_else(|| {
                    let ids: Vec<&str> = DriverKind::ALL.into_iter().map(driver_id).collect();
                    format!(
                        "{name:?} is not a driver; the drivers are {}",
                        ids.join(", ")
                    )
                })
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// The name `data_sources.toml` gives the driver, as `driver = "postgres"`.
pub fn driver_id(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Postgres => "postgres",
        DriverKind::Sqlite => "sqlite",
        DriverKind::Mysql => "mysql",
        DriverKind::SqlServer => "sqlserver",
        DriverKind::DuckDb => "duckdb",
        DriverKind::ClickHouse => "clickhouse",
        DriverKind::Redis => "redis",
        DriverKind::Mongo => "mongodb",
        DriverKind::Elasticsearch => "elasticsearch",
        DriverKind::Cassandra => "cassandra",
        DriverKind::DynamoDb => "dynamodb",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_adds_nothing_and_a_broken_one_says_where() {
        let dir = tempfile::tempdir().unwrap();
        let files = CustomFiles::new(dir.path());
        for kind in CustomFileKind::ALL {
            assert_eq!(files.read(kind), CustomFile::parse(kind, ""), "{kind:?}");
            assert!(files.read(kind).problems().is_empty(), "{kind:?}");
        }

        std::fs::write(files.path(CustomFileKind::Actions), "[[action]]\nid = \n").unwrap();
        assert_eq!(
            files.read(CustomFileKind::Actions).problems(),
            ["line 2: string values must be quoted, expected literal string"]
        );
    }

    #[test]
    fn says_what_else_the_file_holds() {
        let mut problems = Vec::new();
        let found = entries("acton = 1\naction = 2\n", "action", &mut problems);
        assert!(found.is_empty());
        assert_eq!(
            problems,
            [
                "action must be a list of [[action]] tables",
                "\"acton\" is not part of this file; write each entry as a [[action]] table",
            ]
        );
    }

    #[test]
    fn reads_drivers_as_data_sources_toml_names_them() {
        let names = |list: &[&str]| Some(list.iter().map(|&name| name.to_owned()).collect());
        assert_eq!(drivers(None), Ok(None));
        assert_eq!(
            drivers(names(&["postgres", "sqlite"])),
            Ok(Some(vec![DriverKind::Postgres, DriverKind::Sqlite]))
        );
        assert_eq!(
            drivers(names(&["postgresql"])),
            Err(
                "\"postgresql\" is not a driver; the drivers are postgres, sqlite, mysql, \
                 sqlserver, duckdb, clickhouse, redis, mongodb, elasticsearch, cassandra, dynamodb"
                    .to_owned()
            )
        );
    }
}
