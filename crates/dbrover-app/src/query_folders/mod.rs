//! The explorer's **Query folders** (VC2, `docs/version-control.md`): folders of `.sql` files,
//! such as a repository's `queries/`, listed from `settings.toml` and read by the engine.

mod section;

use std::path::{Path, PathBuf};

use dbrover_engine::{Command, Event, QueryFolderProblem};
use dbrover_store::{QueryTree, expand_home, home_relative};

pub use section::{SectionState, section};

/// Something asked of the **Query folders** section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryFolderAction {
    /// Asks for a folder, and adds it to the list.
    Add,
    /// Takes the folder, as `settings.toml` writes it, off the list. No file is touched.
    Remove(PathBuf),
    /// Reads every folder again.
    Reload,
}

/// A listed folder, and what was last read of it.
#[derive(Debug)]
pub struct QueryFolder {
    /// As `settings.toml` writes it, which is how the list is kept and changed.
    written: PathBuf,
    /// Where it is on this machine.
    path: PathBuf,
    listing: Listing,
    reading: bool,
    /// Asked to read again while a read was on its way, which may have missed the change.
    read_again: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Reading,
    Read(QueryTree),
    Failed(QueryFolderProblem),
}

impl QueryFolder {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn written(&self) -> &Path {
        &self.written
    }

    /// The folder's own name, as the explorer lists it.
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    pub fn listing(&self) -> &Listing {
        &self.listing
    }

    /// Reads the folder, unless a read is on its way: then it is read again once that one is
    /// answered. Two reads in flight could otherwise be answered in either order, leaving the
    /// older tree in sight.
    fn read(&mut self) -> Option<Command> {
        if self.reading {
            self.read_again = true;
            return None;
        }
        self.reading = true;
        Some(Command::ReadQueryFolder(self.path.clone()))
    }
}

#[derive(Debug, Default)]
pub struct QueryFolders {
    folders: Vec<QueryFolder>,
    /// This machine's home folder, which `~` in `settings.toml` stands for.
    home: Option<PathBuf>,
}

impl QueryFolders {
    pub fn new(home: Option<PathBuf>) -> Self {
        Self {
            folders: Vec::new(),
            home,
        }
    }

    pub fn listed(&self) -> &[QueryFolder] {
        &self.folders
    }

    /// Follows the list in `settings.toml`: folders new to it are read, those gone from it
    /// are dropped, and the rest keep what was read of them.
    pub fn follow(&mut self, written: &[PathBuf]) -> Vec<Command> {
        let mut kept = std::mem::take(&mut self.folders);
        let mut reads = Vec::new();
        for path in written {
            let folder = match kept.iter().position(|folder| &folder.written == path) {
                Some(index) => kept.swap_remove(index),
                None => {
                    let mut folder = QueryFolder {
                        written: path.clone(),
                        path: expand_home(path, self.home.as_deref()),
                        listing: Listing::Reading,
                        reading: false,
                        read_again: false,
                    };
                    reads.extend(folder.read());
                    folder
                }
            };
            self.folders.push(folder);
        }
        reads
    }

    /// Reads every folder again, as when the window comes back to the front after a
    /// `git checkout` elsewhere.
    pub fn reload(&mut self) -> Vec<Command> {
        self.folders
            .iter_mut()
            .filter_map(QueryFolder::read)
            .collect()
    }

    pub fn apply(&mut self, event: &Event) -> Vec<Command> {
        let Event::QueryFolderRead { path, result } = event else {
            return Vec::new();
        };
        let mut reads = Vec::new();
        for folder in self
            .folders
            .iter_mut()
            .filter(|folder| &folder.path == path)
        {
            folder.reading = false;
            folder.listing = match result {
                Ok(tree) => Listing::Read(tree.clone()),
                Err(problem) => Listing::Failed(problem.clone()),
            };
            if std::mem::take(&mut folder.read_again) {
                reads.extend(folder.read());
            }
        }
        reads
    }

    /// The list in `settings.toml` with `path` added at its end, or `None` if it is listed
    /// already.
    pub fn with_folder(&self, written: &[PathBuf], path: &Path) -> Option<Vec<PathBuf>> {
        let added = home_relative(path, self.home.as_deref());
        let listed = written
            .iter()
            .any(|written| expand_home(written, self.home.as_deref()) == path);
        (!listed).then(|| written.iter().cloned().chain([added]).collect())
    }
}

/// The list in `settings.toml` without `removed`.
pub fn without_folder(written: &[PathBuf], removed: &Path) -> Vec<PathBuf> {
    written
        .iter()
        .filter(|written| written.as_path() != removed)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use dbrover_store::QueryFile;

    use super::*;

    fn tree(file: &str) -> QueryTree {
        QueryTree {
            folders: Vec::new(),
            files: vec![QueryFile {
                name: file.to_owned(),
                path: PathBuf::from(file),
            }],
        }
    }

    fn read(path: &Path, result: Result<QueryTree, QueryFolderProblem>) -> Event {
        Event::QueryFolderRead {
            path: path.to_owned(),
            result,
        }
    }

    #[test]
    fn follows_the_settings_reading_only_new_folders() {
        let mut folders = QueryFolders::new(Some("/home/ana".into()));
        let shop = PathBuf::from("~/shop/queries");
        let srv = PathBuf::from("/srv/queries");

        let reads = folders.follow(&[shop.clone(), srv.clone()]);
        assert_eq!(reads.len(), 2);
        let local = Path::new("/home/ana/shop/queries");
        assert_eq!(folders.listed()[0].path(), local);
        assert_eq!(folders.listed()[0].name(), "queries");

        folders.apply(&read(local, Ok(tree("sales.sql"))));
        assert!(folders.follow(&[srv.clone(), shop.clone()]).is_empty());
        assert_eq!(
            folders.listed()[1].listing(),
            &Listing::Read(tree("sales.sql"))
        );

        assert!(folders.follow(std::slice::from_ref(&srv)).is_empty());
        assert_eq!(folders.listed().len(), 1);
    }

    #[test]
    fn a_reload_while_reading_reads_once_more_when_answered() {
        let mut folders = QueryFolders::new(None);
        let path = PathBuf::from("/srv/queries");
        assert_eq!(folders.follow(std::slice::from_ref(&path)).len(), 1);

        assert!(folders.reload().is_empty(), "a read is on its way");
        let again = folders.apply(&read(&path, Ok(tree("old.sql"))));
        assert!(matches!(again.as_slice(), [Command::ReadQueryFolder(read)] if *read == path));

        assert!(
            folders
                .apply(&read(&path, Err(QueryFolderProblem::Missing)))
                .is_empty()
        );
        assert_eq!(
            folders.listed()[0].listing(),
            &Listing::Failed(QueryFolderProblem::Missing)
        );
        assert_eq!(folders.reload().len(), 1);
    }

    #[test]
    fn adds_a_folder_once_written_from_the_home_folder_and_removes_it_by_that_name() {
        let folders = QueryFolders::new(Some("/home/ana".into()));
        let listed = vec![PathBuf::from("~/shop/queries")];

        assert_eq!(
            folders.with_folder(&listed, Path::new("/home/ana/shop/queries")),
            None
        );
        let added = folders
            .with_folder(&listed, Path::new("/home/ana/crm/sql"))
            .unwrap();
        assert_eq!(added, [PathBuf::from("~/shop/queries"), "~/crm/sql".into()]);

        assert_eq!(
            without_folder(&added, Path::new("~/shop/queries")),
            [PathBuf::from("~/crm/sql")]
        );
    }
}
