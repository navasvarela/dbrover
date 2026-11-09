//! The `.gitignore` DbRover puts in the settings folder, so the query history isn't committed
//! with the settings (VC1d in `docs/version-control.md`).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file's name, in the settings folder.
pub const GITIGNORE: &str = ".gitignore";

/// What DbRover writes. Each line names a file the folder holds that has no place in a
/// commit: the history's database, and SQLite's `-wal` and `-shm` files beside it while
/// it is open.
const TEXT: &str = "\
# Written by DbRover. The query history is this machine's, holds the text of every
# statement, and changes with each one; don't commit it.
history.sqlite
history.sqlite-*
# Consoles are saved as you type. Uncomment to keep them out of commits too.
# consoles/
";

/// How [`add_gitignore`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitignoreAdded {
    Written,
    /// The folder has one, which is never changed: it may be the user's own.
    AlreadyThere,
}

/// Writes DbRover's `.gitignore` in `folder`, creating the folder, unless there is one.
///
/// # Errors
///
/// The file system's error, if the folder or file can't be created.
pub fn add_gitignore(folder: &Path) -> io::Result<GitignoreAdded> {
    std::fs::create_dir_all(folder)?;
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(folder.join(GITIGNORE));
    match created {
        Ok(mut file) => {
            file.write_all(TEXT.as_bytes())?;
            Ok(GitignoreAdded::Written)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(GitignoreAdded::AlreadyThere)
        }
        Err(error) => Err(error),
    }
}

/// How the settings folder stands with Git, for **Settings › Files**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// The top of the Git repository the folder is in: the folder itself or one above it
    /// with a `.git` (a folder, or the file a worktree or submodule has).
    pub repository: Option<PathBuf>,
    /// Whether the folder has a `.gitignore` of its own, DbRover's or not.
    pub has_gitignore: bool,
}

impl GitStatus {
    /// Looks at `folder` and the folders above it. A few `stat`s, no Git.
    pub fn of(folder: &Path) -> Self {
        Self {
            repository: folder
                .ancestors()
                .find(|ancestor| ancestor.join(".git").exists())
                .map(Path::to_path_buf),
            has_gitignore: folder.join(GITIGNORE).is_file(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn writes_the_gitignore_once_and_never_over_another() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("dbrover");

        assert_eq!(add_gitignore(&folder).unwrap(), GitignoreAdded::Written);
        let written = fs::read_to_string(folder.join(GITIGNORE)).unwrap();
        assert!(
            written.contains("\nhistory.sqlite\nhistory.sqlite-*\n"),
            "{written}"
        );

        fs::write(folder.join(GITIGNORE), "*.log\n").unwrap();
        assert_eq!(
            add_gitignore(&folder).unwrap(),
            GitignoreAdded::AlreadyThere
        );
        assert_eq!(
            fs::read_to_string(folder.join(GITIGNORE)).unwrap(),
            "*.log\n"
        );
    }

    #[test]
    fn finds_the_repository_the_folder_is_in() {
        let dir = tempfile::tempdir().unwrap();
        let repository = dir.path().join("dotfiles");
        let folder = repository.join("dbrover");
        fs::create_dir_all(&folder).unwrap();

        assert_eq!(
            GitStatus::of(&folder),
            GitStatus {
                repository: None,
                has_gitignore: false
            }
        );

        fs::create_dir(repository.join(".git")).unwrap();
        add_gitignore(&folder).unwrap();
        assert_eq!(
            GitStatus::of(&folder),
            GitStatus {
                repository: Some(repository),
                has_gitignore: true
            }
        );
    }
}
