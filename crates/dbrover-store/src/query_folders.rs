//! Query folders (VC2, `docs/version-control.md`): folders of `.sql` files the explorer lists,
//! such as a repository's `queries/`. This only finds the files; opening and saving one is
//! W6's, as for any `.sql` file.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;

/// The extension of the files a query folder lists, matched ignoring case.
const SQL_EXTENSION: &str = "sql";

/// What a query folder holds: its `.sql` files, and the subfolders that hold any, however
/// deep. Subfolders come first, then files, each sorted by name ignoring case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryTree {
    pub folders: Vec<QuerySubfolder>,
    pub files: Vec<QueryFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySubfolder {
    pub name: String,
    pub path: PathBuf,
    pub tree: QueryTree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFile {
    pub name: String,
    pub path: PathBuf,
}

/// A folder's tree, and what couldn't be read on the way, such as a subfolder without
/// permission, which is left out rather than failing the whole folder.
#[derive(Debug, Default)]
pub struct QueryFolderListing {
    pub tree: QueryTree,
    pub skipped: Vec<ignore::Error>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryFolderError {
    /// Expected when a synced settings folder lists a folder this machine hasn't cloned.
    #[error("{} isn't on this machine", path.display())]
    Missing { path: PathBuf },
    #[error("{} is not a folder", path.display())]
    NotAFolder { path: PathBuf },
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Lists the `.sql` files under `root`, as Git would show them.
///
/// Hidden files and folders (`.git`, `.idea`) aren't entered. Inside a Git repository,
/// neither is anything its `.gitignore` files (in the folder, below it, or above it up to the
/// repository's top), `.git/info/exclude` or Git's global excludes file ignore, so
/// `node_modules` and `target` are never walked. `root` itself is listed even when hidden or
/// ignored: it was chosen.
///
/// Outside a repository `.gitignore` files mean nothing, as for Git. Why not honour them
/// anyway: with no repository to stop at, every `.gitignore` up to the root of the disk would
/// count, and a home folder kept in Git with `*` in its `.gitignore` would hide every file.
///
/// # Errors
///
/// [`QueryFolderError`] if `root` is missing, isn't a folder, or can't be read at all.
pub fn read_query_folder(root: &Path) -> Result<QueryFolderListing, QueryFolderError> {
    check_root(root)?;
    let mut listing = QueryFolderListing::default();
    let walk = WalkBuilder::new(root)
        .hidden(true)
        // `.ignore` files are ripgrep's, not Git's.
        .ignore(false)
        .follow_links(false)
        .build();
    for entry in walk {
        match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                if is_sql(entry.path())
                    && let Ok(relative) = entry.path().strip_prefix(root)
                {
                    listing.tree.insert(root, relative);
                }
            }
            Ok(_) => {}
            Err(error) => listing.skipped.push(error),
        }
    }
    listing.tree.sort();
    Ok(listing)
}

fn check_root(root: &Path) -> Result<(), QueryFolderError> {
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(QueryFolderError::NotAFolder {
            path: root.to_owned(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(QueryFolderError::Missing {
            path: root.to_owned(),
        }),
        Err(source) => Err(QueryFolderError::Read {
            path: root.to_owned(),
            source,
        }),
    }
}

fn is_sql(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(SQL_EXTENSION))
}

impl QueryTree {
    /// Adds the file at `relative` under `root`, making the subfolders on its way.
    fn insert(&mut self, root: &Path, relative: &Path) {
        let mut names: Vec<String> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let Some(file) = names.pop() else {
            return;
        };
        let mut tree = self;
        let mut path = root.to_owned();
        for name in names {
            path.push(&name);
            let index = match tree.folders.iter().position(|folder| folder.name == name) {
                Some(index) => index,
                None => {
                    tree.folders.push(QuerySubfolder {
                        name,
                        path: path.clone(),
                        tree: Self::default(),
                    });
                    tree.folders.len() - 1
                }
            };
            tree = &mut tree.folders[index].tree;
        }
        tree.files.push(QueryFile {
            path: path.join(&file),
            name: file,
        });
    }

    fn sort(&mut self) {
        self.folders.sort_by(|a, b| by_name(&a.name, &b.name));
        self.files.sort_by(|a, b| by_name(&a.name, &b.name));
        for folder in &mut self.folders {
            folder.tree.sort();
        }
    }

    /// How many `.sql` files the tree holds, in every subfolder.
    pub fn file_count(&self) -> usize {
        self.files.len()
            + self
                .folders
                .iter()
                .map(|folder| folder.tree.file_count())
                .sum::<usize>()
    }
}

/// Ignoring case, so `Admin` and `audit` sit together, then exactly, so the order is total.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// `path` as `settings.toml` keeps it: under `home` as `~/…`, spelt with `/`, so a synced
/// settings folder names the same clone on every machine that has it at the same place.
pub fn home_relative(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(in_home) = home.and_then(|home| path.strip_prefix(home).ok()) else {
        return path.to_owned();
    };
    let names = in_home
        .components()
        .map(|component| component.as_os_str().to_string_lossy());
    let joined: Vec<_> = std::iter::once("~".into()).chain(names).collect();
    PathBuf::from(joined.join("/"))
}

/// `path` as this machine finds it: `~/…` under `home`.
pub fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let mut components = path.components();
    match (components.next(), home) {
        (Some(Component::Normal(first)), Some(home)) if first == "~" => {
            home.join(components.as_path())
        }
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "select 1;").unwrap();
    }

    /// The tree as indented names, folders ending in `/`.
    fn outline(tree: &QueryTree) -> Vec<String> {
        fn walk(tree: &QueryTree, depth: usize, lines: &mut Vec<String>) {
            let indent = "  ".repeat(depth);
            for folder in &tree.folders {
                lines.push(format!("{indent}{}/", folder.name));
                walk(&folder.tree, depth + 1, lines);
            }
            for file in &tree.files {
                lines.push(format!("{indent}{}", file.name));
            }
        }
        let mut lines = Vec::new();
        walk(tree, 0, &mut lines);
        lines
    }

    #[test]
    fn lists_sql_files_and_the_folders_holding_them_leaving_out_hidden_and_ignored_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        for file in [
            "sales.sql",
            "Audit.SQL",
            "admin/users.sql",
            "admin/roles/grants.sql",
            "admin/notes.md",
            "docs/readme.md",
            ".git/hooks/pre-commit.sql",
            ".idea/scratch.sql",
            ".hidden.sql",
            "target/generated.sql",
            "drafts/old.sql",
            "drafts/keep.sql",
        ] {
            write(root, file);
        }
        fs::write(root.join(".gitignore"), "target/\ndrafts/old.sql\n").unwrap();

        let listing = read_query_folder(root).unwrap();

        assert_eq!(
            outline(&listing.tree),
            [
                "admin/",
                "  roles/",
                "    grants.sql",
                "  users.sql",
                "drafts/",
                "  keep.sql",
                "Audit.SQL",
                "sales.sql",
            ]
        );
        assert_eq!(listing.tree.file_count(), 5);
        assert!(listing.skipped.is_empty());
        let users = &listing.tree.folders[0].tree.files[0];
        assert_eq!(users.path, root.join("admin").join("users.sql"));
    }

    #[test]
    fn a_gitignore_above_the_folder_counts_up_to_the_repository_top() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "*.sql\n").unwrap();
        let repository = dir.path().join("shop");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::write(repository.join(".gitignore"), "scratch/\n").unwrap();
        let queries = repository.join("queries");
        write(&queries, "sales.sql");
        write(&queries, "scratch/try.sql");

        let listing = read_query_folder(&queries).unwrap();

        assert_eq!(outline(&listing.tree), ["sales.sql"]);
    }

    #[test]
    fn outside_a_repository_a_gitignore_means_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sales.sql");
        write(dir.path(), "scratch/try.sql");
        fs::write(dir.path().join(".gitignore"), "scratch/\n").unwrap();

        let listing = read_query_folder(dir.path()).unwrap();

        assert_eq!(
            outline(&listing.tree),
            ["scratch/", "  try.sql", "sales.sql"]
        );
    }

    #[test]
    fn a_hidden_folder_chosen_as_the_root_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".queries");
        write(&root, "sales.sql");

        let listing = read_query_folder(&root).unwrap();

        assert_eq!(outline(&listing.tree), ["sales.sql"]);
    }

    #[test]
    fn tells_a_missing_folder_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sales.sql");
        write(dir.path(), "sales.sql");

        assert!(matches!(
            read_query_folder(&dir.path().join("gone")),
            Err(QueryFolderError::Missing { .. })
        ));
        assert!(matches!(
            read_query_folder(&file),
            Err(QueryFolderError::NotAFolder { .. })
        ));
    }

    #[test]
    fn writes_folders_under_the_home_folder_as_tilde_paths_and_reads_them_back() {
        let home = Path::new("/home/ana");
        let cases = [
            ("/home/ana/src/shop/queries", "~/src/shop/queries"),
            ("/home/ana", "~"),
            ("/srv/queries", "/srv/queries"),
            ("/home/anabel/queries", "/home/anabel/queries"),
        ];
        for (path, written) in cases {
            let path = Path::new(path);
            assert_eq!(
                home_relative(path, Some(home)),
                PathBuf::from(written),
                "{path:?}"
            );
            assert_eq!(expand_home(Path::new(written), Some(home)), path);
        }
        assert_eq!(
            expand_home(Path::new("~/queries"), Some(Path::new("/Users/bo"))),
            Path::new("/Users/bo/queries")
        );
        assert_eq!(
            home_relative(Path::new("/home/ana/q"), None),
            Path::new("/home/ana/q")
        );
        assert_eq!(expand_home(Path::new("~/q"), None), Path::new("~/q"));
    }
}
