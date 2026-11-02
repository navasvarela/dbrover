//! Report files: `reports/<name>.toml` in the settings folder, one folder for every project
//! (`docs/reporting.md`, RP1's decisions). `dbrover-report` reads what they say; this finds
//! and writes them.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::config_text::create_new;

const EXTENSION: &str = "toml";

/// How many numbered names [`ReportFiles::create`] tries: far more new reports than anyone
/// leaves unrenamed.
const MAX_NUMBERED: u32 = 1_000;

/// A report file's name, without `.toml`: what the explorer and the worker know a report by
/// until its file is read. Always a plain file name, so joining it to the folder can never
/// point outside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReportName(String);

impl ReportName {
    /// `None` for anything that isn't a plain file name: empty, a dot or two, or with a folder
    /// separator in it.
    pub fn new(name: &str) -> Option<Self> {
        let plain = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\', '\0'])
            && !name.starts_with('.');
        plain.then(|| Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReportName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `reports/` folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFiles {
    dir: PathBuf,
}

impl ReportFiles {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, name: &ReportName) -> PathBuf {
        self.dir.join(format!("{name}.{EXTENSION}"))
    }

    /// The `.toml` files in the folder, by name; none if there is no folder yet. Folders and
    /// other files in it are left alone.
    ///
    /// # Errors
    ///
    /// If the folder exists but can't be read.
    pub fn list(&self) -> io::Result<Vec<ReportName>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut names = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if !path.is_file()
                || path
                    .extension()
                    .is_none_or(|extension| extension != EXTENSION)
            {
                continue;
            }
            if let Some(name) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(ReportName::new)
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// The report's text.
    ///
    /// # Errors
    ///
    /// If the file can't be read.
    pub fn read(&self, name: &ReportName) -> io::Result<String> {
        std::fs::read_to_string(self.path(name))
    }

    /// Writes `text` as a new report, under the first of `name`, `name-2`, `name-3`, … that
    /// no file has, and returns the name it took. Never replaces a report.
    ///
    /// # Errors
    ///
    /// If the file can't be written.
    pub fn create(&self, name: &ReportName, text: &str) -> io::Result<ReportName> {
        for number in 1..=MAX_NUMBERED {
            let candidate = match number {
                1 => name.clone(),
                n => ReportName(format!("{name}-{n}")),
            };
            match create_new(&self.path(&candidate), text) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{name}.{EXTENSION} and {MAX_NUMBERED} numbered names after it are taken"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_toml_files_by_name() {
        let folder = tempfile::tempdir().unwrap();
        let files = ReportFiles::new(folder.path().join("reports"));
        assert_eq!(files.list().unwrap(), []);
        std::fs::create_dir_all(files.dir().join("archive")).unwrap();
        for file in [
            "trial-balance.toml",
            "ledger.toml",
            "notes.txt",
            ".hidden.toml",
        ] {
            std::fs::write(files.dir().join(file), "").unwrap();
        }
        let names: Vec<String> = files
            .list()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["ledger", "trial-balance"]);
    }

    #[test]
    fn creates_a_report_under_a_free_name() {
        let folder = tempfile::tempdir().unwrap();
        let files = ReportFiles::new(folder.path().join("reports"));
        let name = ReportName::new("report").unwrap();
        assert_eq!(files.create(&name, "a").unwrap().as_str(), "report");
        assert_eq!(files.create(&name, "b").unwrap().as_str(), "report-2");
        assert_eq!(files.read(&name).unwrap(), "a");
    }

    #[test]
    fn takes_only_plain_names() {
        for name in ["", ".", "..", "a/b", "a\\b", ".hidden"] {
            assert_eq!(ReportName::new(name), None, "{name:?}");
        }
        assert!(ReportName::new("trial balance 2026").is_some());
    }
}
