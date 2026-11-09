//! Where a console's text is kept: a file in the config folder that saves itself, or a `.sql`
//! file the user chose, which is written only when asked.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use dbrover_engine::WatchedFile;
use dbrover_store::SqlFileContents;

#[derive(Debug)]
pub(super) enum Storage {
    Config(ConfigFile),
    File(SqlFile),
}

/// How the console's text relates to its file in the config folder.
#[derive(Debug, Default)]
pub(super) struct ConfigFile {
    /// When the text was first changed since it was last written; `None` if it is saved.
    pub unsaved_since: Option<Instant>,
    /// Whether closing the tab keeps the file, so the console can be reopened. False for
    /// consoles nobody typed in, such as a fresh one or one opened by Open data.
    pub keep: bool,
    /// Why the last save failed, until a later one succeeds.
    pub save_error: Option<String>,
    /// What another program did to the file, when the console had text it hadn't saved:
    /// [`Disk::Changed`] holds saves back until **Reload** or **Keep mine**.
    pub disk: Disk,
    /// **Reload** or **Keep mine** was chosen, and the engine is yet to be told, before the
    /// next save.
    pub answered: bool,
}

/// A `.sql` file anywhere on disk, and what the console knows of it.
#[derive(Debug)]
pub(super) struct SqlFile {
    path: PathBuf,
    /// What the file holds, as last read or written. The console has unsaved changes while
    /// its text differs, so undoing back to it counts as saved.
    saved_text: String,
    /// The file's modification time when last read or written, to notice other programs'
    /// changes; `None` once it is gone.
    modified: Option<SystemTime>,
    pub disk: Disk,
    /// The text of the save in flight, which becomes `saved_text` once it is written.
    saving: Option<String>,
    pub save_error: Option<String>,
    /// The console's file in the config folder, left from before it was saved here. Removed
    /// once this file is written, so the text is never only in memory.
    config_copy: bool,
}

/// What another program did to the file since the console last read or wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Disk {
    #[default]
    Unchanged,
    /// Changed while the console had unsaved changes: what the file holds now.
    Changed(String),
    Deleted,
}

/// What a change on disk did to the console's text.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reload {
    /// Nothing the user sees changed.
    Kept,
    /// The text was replaced by the file's, there being nothing unsaved to lose.
    Replaced(String),
}

impl SqlFile {
    /// A file just read, or written from the console's text.
    pub fn new(path: PathBuf, contents: SqlFileContents) -> Self {
        Self {
            path,
            saved_text: contents.text,
            modified: contents.modified,
            disk: Disk::Unchanged,
            saving: None,
            save_error: None,
            config_copy: false,
        }
    }

    /// A console from the config folder being saved to `path` for the first time.
    pub fn saved_from_config(path: PathBuf) -> Self {
        Self {
            config_copy: true,
            ..Self::new(
                path,
                SqlFileContents {
                    text: String::new(),
                    modified: None,
                },
            )
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    pub fn is_unsaved(&self, text: &str) -> bool {
        text != self.saved_text || self.disk == Disk::Deleted
    }

    /// Marks `text` as being written.
    pub fn saving(&mut self, text: String) {
        self.saving = Some(text);
    }

    /// Takes in how a save went. `true` if the console's copy in the config folder can now be
    /// removed.
    pub fn saved(&mut self, result: &Result<Option<SystemTime>, String>) -> bool {
        let text = self.saving.take();
        match result {
            Ok(modified) => {
                if let Some(text) = text {
                    self.saved_text = text;
                }
                self.modified = *modified;
                self.disk = Disk::Unchanged;
                self.save_error = None;
                std::mem::take(&mut self.config_copy)
            }
            Err(error) => {
                self.save_error = Some(error.clone());
                false
            }
        }
    }

    /// Takes in what the file holds now that another program changed it (`None`: removed). A
    /// console without unsaved changes follows the file; one with them keeps its text and
    /// says the file changed.
    pub fn changed_on_disk(&mut self, contents: Option<&SqlFileContents>, text: &str) -> Reload {
        let Some(contents) = contents else {
            self.modified = None;
            self.disk = Disk::Deleted;
            return Reload::Kept;
        };
        self.modified = contents.modified;
        let ours = contents.text == self.saved_text
            || self.saving.as_deref() == Some(contents.text.as_str());
        if ours {
            self.disk = Disk::Unchanged;
            return Reload::Kept;
        }
        if text == self.saved_text {
            self.saved_text.clone_from(&contents.text);
            self.disk = Disk::Unchanged;
            return Reload::Replaced(contents.text.clone());
        }
        self.disk = Disk::Changed(contents.text.clone());
        Reload::Kept
    }

    /// Takes the file's text, dropping the console's unsaved changes. `None` unless the file
    /// changed on disk.
    pub fn reload(&mut self) -> Option<String> {
        let Disk::Changed(text) = std::mem::replace(&mut self.disk, Disk::Unchanged) else {
            return None;
        };
        self.saved_text.clone_from(&text);
        Some(text)
    }

    /// Keeps the console's text over the file's; saving it overwrites the file.
    pub fn keep_mine(&mut self) {
        if matches!(self.disk, Disk::Changed(_)) {
            self.disk = Disk::Unchanged;
        }
    }

    pub fn watched(&self) -> WatchedFile {
        WatchedFile {
            path: self.path.clone(),
            modified: self.modified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(text: &str, seconds: u64) -> SqlFileContents {
        SqlFileContents {
            text: text.to_owned(),
            modified: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
        }
    }

    fn file(text: &str) -> SqlFile {
        SqlFile::new("/work/report.sql".into(), contents(text, 1))
    }

    #[test]
    fn is_unsaved_while_the_text_differs_from_the_file() {
        let mut file = file("select 1;");
        assert!(!file.is_unsaved("select 1;"));
        assert!(file.is_unsaved("select 12;"));

        file.saving("select 12;".to_owned());
        assert!(file.is_unsaved("select 12;"), "until the save is written");
        assert!(!file.saved(&Ok(None)));
        assert!(!file.is_unsaved("select 12;"));
        assert_eq!(file.watched().modified, None);
    }

    #[test]
    fn follows_the_file_unless_there_is_something_unsaved() {
        let mut clean = file("select 1;");
        assert_eq!(
            clean.changed_on_disk(Some(&contents("select 2;", 2)), "select 1;"),
            Reload::Replaced("select 2;".to_owned())
        );
        assert!(!clean.is_unsaved("select 2;"));

        let mut edited = file("select 1;");
        assert_eq!(
            edited.changed_on_disk(Some(&contents("select 2;", 2)), "select 1; -- mine"),
            Reload::Kept
        );
        assert_eq!(edited.disk, Disk::Changed("select 2;".to_owned()));
        assert_eq!(edited.reload(), Some("select 2;".to_owned()));
        assert!(!edited.is_unsaved("select 2;"));
        assert_eq!(edited.disk, Disk::Unchanged);
    }

    #[test]
    fn keeping_mine_leaves_the_text_unsaved_until_it_is_written() {
        let mut file = file("select 1;");
        file.changed_on_disk(Some(&contents("select 2;", 2)), "select 3;");

        file.keep_mine();

        assert_eq!(file.disk, Disk::Unchanged);
        assert!(file.is_unsaved("select 3;"));
        assert_eq!(file.reload(), None);
    }

    #[test]
    fn its_own_save_is_not_a_change_on_disk() {
        let mut file = file("select 1;");
        file.saving("select 2;".to_owned());

        let reload = file.changed_on_disk(Some(&contents("select 2;", 2)), "select 2;");

        assert_eq!(reload, Reload::Kept);
        assert_eq!(file.disk, Disk::Unchanged);
    }

    #[test]
    fn a_deleted_file_is_unsaved_until_written_again() {
        let mut file = file("select 1;");

        file.changed_on_disk(None, "select 1;");

        assert_eq!(file.disk, Disk::Deleted);
        assert!(file.is_unsaved("select 1;"));
        file.saving("select 1;".to_owned());
        file.saved(&Ok(Some(SystemTime::UNIX_EPOCH)));
        assert!(!file.is_unsaved("select 1;"));
    }

    #[test]
    fn a_console_saved_from_the_config_folder_removes_its_copy_once_written() {
        let mut file = SqlFile::saved_from_config("/work/new.sql".into());
        file.saving("select 1;".to_owned());

        assert!(!file.saved(&Err("disk full".to_owned())));
        assert_eq!(file.save_error.as_deref(), Some("disk full"));
        file.saving("select 1;".to_owned());
        assert!(file.saved(&Ok(None)));
        assert!(!file.saved(&Ok(None)), "only once");
        assert_eq!(file.file_name(), "new.sql");
    }
}
