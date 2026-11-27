//! Where to look for a program: `PATH`, then the folders installers put each engine's client
//! tools in. A Mac app opened from the Finder gets a short `PATH` (`/usr/bin:/bin:/usr/sbin:
//! /sbin`), not the one a shell profile sets, so the installers' folders are what find the
//! programs there.
//!
//! The search only names candidate files; reading their versions is the engine's, since it
//! runs them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::program::{Os, Program};

/// What the search needs of the file system, so it can be tested on a made-up one.
pub trait Folders {
    fn is_file(&self, path: &Path) -> bool;
    /// The names of the folders directly in `path`; none if it can't be read.
    fn subfolder_names(&self, path: &Path) -> Vec<String>;
}

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFolders;

impl Folders for RealFolders {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn subfolder_names(&self, path: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect()
    }
}

/// Where the search starts from: the system, `PATH`'s folders, and on Windows the Program
/// Files folder.
#[derive(Debug, Clone)]
pub struct SearchContext {
    pub os: Os,
    pub path: Vec<PathBuf>,
    /// `%ProgramFiles%`, on Windows.
    pub program_files: Option<PathBuf>,
}

impl SearchContext {
    /// The running DbRover's.
    pub fn current() -> Self {
        Self {
            os: Os::current(),
            path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            program_files: std::env::var_os("ProgramFiles").map(PathBuf::from),
        }
    }
}

/// A folder installers put programs in: either a fixed one, or each folder in `parent` whose
/// name starts with `prefix`, then `rest` inside it (`/opt/homebrew/opt/postgresql@*/bin`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    Fixed(PathBuf),
    Each {
        parent: PathBuf,
        prefix: &'static str,
        rest: &'static str,
    },
}

const HOMEBREW_PREFIXES: [&str; 2] = ["/opt/homebrew/opt", "/usr/local/opt"];

fn places(program: Program, context: &SearchContext) -> Vec<Place> {
    let family = program.family();
    match context.os {
        Os::MacOs => mac_places(family),
        Os::Linux => linux_places(family),
        Os::Windows => context
            .program_files
            .as_deref()
            .map(|program_files| windows_places(family, program_files))
            .unwrap_or_default(),
    }
}

fn each(parent: impl Into<PathBuf>, prefix: &'static str, rest: &'static str) -> Place {
    Place::Each {
        parent: parent.into(),
        prefix,
        rest,
    }
}

fn mac_places(family: dbrover_core::DumpEngine) -> Vec<Place> {
    use dbrover_core::DumpEngine;
    let mut places = Vec::new();
    for brew in HOMEBREW_PREFIXES {
        let brew = Path::new(brew);
        match family {
            DumpEngine::Postgres => {
                places.push(Place::Fixed(brew.join("libpq/bin")));
                places.push(each(brew, "postgresql@", "bin"));
            }
            DumpEngine::Mysql => {
                places.push(Place::Fixed(brew.join("mysql-client/bin")));
                places.push(Place::Fixed(brew.join("mysql/bin")));
                places.push(each(brew, "mysql@", "bin"));
            }
            DumpEngine::MariaDb => {
                places.push(Place::Fixed(brew.join("mariadb/bin")));
                places.push(each(brew, "mariadb@", "bin"));
            }
        }
    }
    match family {
        DumpEngine::Postgres => {
            places.push(each(
                "/Applications/Postgres.app/Contents/Versions",
                "",
                "bin",
            ));
            places.push(each("/Library/PostgreSQL", "", "bin"));
        }
        DumpEngine::Mysql => places.push(Place::Fixed("/usr/local/mysql/bin".into())),
        DumpEngine::MariaDb => {}
    }
    places
}

fn linux_places(family: dbrover_core::DumpEngine) -> Vec<Place> {
    use dbrover_core::DumpEngine;
    match family {
        // Debian and Ubuntu keep one folder per version (`/usr/bin/pg_dump` is a wrapper that
        // picks one by cluster); PGDG's Red Hat and Fedora packages do too.
        DumpEngine::Postgres => vec![
            each("/usr/lib/postgresql", "", "bin"),
            each("/usr", "pgsql-", "bin"),
        ],
        DumpEngine::Mysql | DumpEngine::MariaDb => vec![Place::Fixed("/usr/bin".into())],
    }
}

fn windows_places(family: dbrover_core::DumpEngine, program_files: &Path) -> Vec<Place> {
    use dbrover_core::DumpEngine;
    match family {
        DumpEngine::Postgres => vec![each(program_files.join("PostgreSQL"), "", "bin")],
        DumpEngine::Mysql => vec![each(program_files.join("MySQL"), "MySQL Server ", "bin")],
        DumpEngine::MariaDb => vec![each(program_files, "MariaDB ", "bin")],
    }
}

/// Every copy of `program` found: `PATH`'s first, then the installers' folders, each file once.
pub fn find(program: Program, context: &SearchContext, folders: &dyn Folders) -> Vec<PathBuf> {
    let file_name = program.file_name(context.os);
    let installed = places(program, context)
        .into_iter()
        .flat_map(|place| expand(place, folders));
    let mut seen = HashSet::new();
    context
        .path
        .iter()
        .cloned()
        .chain(installed)
        .map(|folder| folder.join(&file_name))
        .filter(|candidate| folders.is_file(candidate))
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

fn expand(place: Place, folders: &dyn Folders) -> Vec<PathBuf> {
    match place {
        Place::Fixed(folder) => vec![folder],
        Place::Each {
            parent,
            prefix,
            rest,
        } => {
            let mut names: Vec<String> = folders
                .subfolder_names(&parent)
                .into_iter()
                .filter(|name| name.starts_with(prefix))
                .collect();
            names.sort();
            names
                .into_iter()
                .map(|name| parent.join(name).join(rest))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// A file system of the files listed, and the folders they are in.
    struct Fake(BTreeSet<PathBuf>);

    impl Fake {
        fn new(files: &[&str]) -> Self {
            Self(files.iter().map(PathBuf::from).collect())
        }
    }

    impl Folders for Fake {
        fn is_file(&self, path: &Path) -> bool {
            self.0.contains(path)
        }

        fn subfolder_names(&self, path: &Path) -> Vec<String> {
            let names: BTreeSet<String> = self
                .0
                .iter()
                .filter_map(|file| file.strip_prefix(path).ok())
                .filter(|inside| inside.components().count() > 1)
                .filter_map(|inside| inside.components().next())
                .map(|first| first.as_os_str().to_string_lossy().into_owned())
                .collect();
            names.into_iter().collect()
        }
    }

    fn context(os: Os, path: &[&str]) -> SearchContext {
        SearchContext {
            os,
            path: path.iter().map(PathBuf::from).collect(),
            program_files: (os == Os::Windows).then(|| PathBuf::from("C:/Program Files")),
        }
    }

    fn found(program: Program, context: &SearchContext, files: &[&str]) -> Vec<String> {
        find(program, context, &Fake::new(files))
            .into_iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn finds_postgres_on_a_mac_beyond_the_finders_path() {
        let files = [
            "/usr/bin/true",
            "/opt/homebrew/opt/libpq/bin/pg_dump",
            "/opt/homebrew/opt/postgresql@16/bin/pg_dump",
            "/opt/homebrew/opt/postgresql@17/bin/pg_dump",
            "/Applications/Postgres.app/Contents/Versions/17/bin/pg_dump",
            "/Library/PostgreSQL/15/bin/pg_dump",
        ];
        assert_eq!(
            found(
                Program::PgDump,
                &context(Os::MacOs, &["/usr/bin", "/bin"]),
                &files
            ),
            [
                "/opt/homebrew/opt/libpq/bin/pg_dump",
                "/opt/homebrew/opt/postgresql@16/bin/pg_dump",
                "/opt/homebrew/opt/postgresql@17/bin/pg_dump",
                "/Applications/Postgres.app/Contents/Versions/17/bin/pg_dump",
                "/Library/PostgreSQL/15/bin/pg_dump",
            ]
        );
    }

    #[test]
    fn path_comes_first_and_each_file_once() {
        let files = [
            "/opt/homebrew/opt/libpq/bin/pg_dump",
            "/home/ada/bin/pg_dump",
        ];
        assert_eq!(
            found(
                Program::PgDump,
                &context(Os::MacOs, &["/home/ada/bin", "/opt/homebrew/opt/libpq/bin"]),
                &files
            ),
            [
                "/home/ada/bin/pg_dump",
                "/opt/homebrew/opt/libpq/bin/pg_dump"
            ]
        );
    }

    #[test]
    fn finds_each_debian_and_pgdg_version_on_linux() {
        let files = [
            "/usr/bin/pg_dump",
            "/usr/lib/postgresql/16/bin/pg_dump",
            "/usr/lib/postgresql/17/bin/pg_dump",
            "/usr/pgsql-17/bin/pg_dump",
            "/usr/bin/mariadb-dump",
        ];
        let linux = context(Os::Linux, &["/usr/bin"]);
        assert_eq!(
            found(Program::PgDump, &linux, &files),
            [
                "/usr/bin/pg_dump",
                "/usr/lib/postgresql/16/bin/pg_dump",
                "/usr/lib/postgresql/17/bin/pg_dump",
                "/usr/pgsql-17/bin/pg_dump",
            ]
        );
        assert_eq!(
            found(Program::MariadbDump, &linux, &files),
            ["/usr/bin/mariadb-dump"]
        );
    }

    #[test]
    fn finds_the_windows_installers_folders() {
        let files = [
            "C:/Program Files/PostgreSQL/17/bin/pg_dump.exe",
            "C:/Program Files/MySQL/MySQL Server 8.4/bin/mysqldump.exe",
            "C:/Program Files/MySQL/MySQL Shell 8.4/bin/mysqlsh.exe",
            "C:/Program Files/MariaDB 11.4/bin/mariadb-dump.exe",
        ];
        let windows = context(Os::Windows, &[]);
        assert_eq!(
            found(Program::PgDump, &windows, &files),
            ["C:/Program Files/PostgreSQL/17/bin/pg_dump.exe"]
        );
        assert_eq!(
            found(Program::Mysqldump, &windows, &files),
            ["C:/Program Files/MySQL/MySQL Server 8.4/bin/mysqldump.exe"]
        );
        assert_eq!(
            found(Program::MariadbDump, &windows, &files),
            ["C:/Program Files/MariaDB 11.4/bin/mariadb-dump.exe"]
        );
    }

    #[test]
    fn finds_nothing_where_nothing_is() {
        assert!(found(Program::Psql, &context(Os::MacOs, &["/usr/bin"]), &[]).is_empty());
    }
}
