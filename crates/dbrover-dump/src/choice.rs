//! Choosing, among the copies of a program found, the one that fits a server, and saying what
//! to install when none does.

use std::path::PathBuf;

use dbrover_core::DumpEngine;

use crate::program::{Os, Program, Role};
use crate::version::{ProgramVersion, Version};

/// A copy of a program, and what its `--version` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundProgram {
    pub program: Program,
    pub path: PathBuf,
    pub version: ProgramVersion,
}

/// What a run needs of its program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub engine: DumpEngine,
    pub role: Role,
    /// The server dumped from or restored into.
    pub server: Version,
    /// For a restore of a PostgreSQL archive: the `pg_dump` that wrote it, which `pg_restore`
    /// must be at least as new as.
    pub archive_made_by: Option<Version>,
}

/// The copy chosen, and a warning to show beside it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<'a> {
    pub program: &'a FoundProgram,
    pub warning: Option<String>,
}

/// Why no copy fits, as a sentence, with what to install.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason} {install}")]
pub struct NoFit {
    pub reason: String,
    pub install: String,
}

/// Chooses the copy of a program that fits `need` from `found`.
///
/// - **PostgreSQL:** `pg_dump` refuses a server newer than itself, so a dump needs a copy whose
///   major version is at least the server's; `pg_restore` can't read an archive from a newer
///   `pg_dump`, so it needs one at least as new as the archive's maker. Of those, the copy of
///   the server's own major version is preferred, then the newest.
/// - **MySQL and MariaDB:** each family's own client, chosen by what its version says rather
///   than by its name; the server's major version is preferred, then the newest, and an older
///   client is used with a warning.
///
/// # Errors
///
/// [`NoFit`] when no copy can do it.
pub fn choose<'a>(found: &'a [FoundProgram], need: &Need, os: Os) -> Result<Choice<'a>, NoFit> {
    let programs = Program::for_role(need.engine, need.role);
    let candidates: Vec<&FoundProgram> = found
        .iter()
        .filter(|copy| programs.contains(&copy.program))
        .filter(|copy| copy.version.family == need.engine)
        .collect();
    let at_least = minimum_major(need);
    let fitting: Vec<&FoundProgram> = candidates
        .iter()
        .copied()
        .filter(|copy| at_least.is_none_or(|major| copy.version.version.major >= major))
        .collect();
    let preferred = fitting
        .iter()
        .copied()
        .filter(|copy| copy.version.version.major == need.server.major)
        .max_by_key(|copy| copy.version.version)
        .or_else(|| {
            fitting
                .iter()
                .copied()
                .max_by_key(|copy| copy.version.version)
        });
    match preferred {
        Some(program) => Ok(Choice {
            program,
            warning: warning(program, need),
        }),
        None => Err(no_fit(&candidates, need, at_least, os)),
    }
}

/// The lowest major version that can do the run, where there is one.
fn minimum_major(need: &Need) -> Option<u32> {
    match (need.engine, need.role) {
        (DumpEngine::Postgres, Role::Dump) => Some(need.server.major),
        (DumpEngine::Postgres, Role::RestoreArchive) => {
            need.archive_made_by.map(|maker| maker.major)
        }
        _ => None,
    }
}

fn warning(program: &FoundProgram, need: &Need) -> Option<String> {
    let version = program.version.version;
    let older = version.major < need.server.major;
    match need.engine {
        DumpEngine::Mysql | DumpEngine::MariaDb if older => Some(format!(
            "{} {version} is older than the server ({} {}); it usually works, but a client of \
             the server's version is safer.",
            program.program, need.engine, need.server
        )),
        DumpEngine::Postgres if need.role == Role::RunSql && older => Some(format!(
            "psql {version} is older than the server ({}); it runs the dump's SQL as it is.",
            need.server
        )),
        _ => None,
    }
}

fn no_fit(candidates: &[&FoundProgram], need: &Need, at_least: Option<u32>, os: Os) -> NoFit {
    let name = Program::for_role(need.engine, need.role)
        .first()
        .map_or("the client", |program| program.name());
    let newest = candidates.iter().max_by_key(|copy| copy.version.version);
    let reason = match (newest, at_least, need.archive_made_by) {
        (None, _, _) => format!("{name} was not found."),
        (Some(newest), Some(_), Some(maker)) if need.role == Role::RestoreArchive => format!(
            "{name} {} can't read a dump made by pg_dump {maker}.",
            newest.version.version
        ),
        (Some(newest), _, _) => format!(
            "{name} {} can't dump {} {}.",
            newest.version.version, need.engine, need.server
        ),
    };
    let major = at_least.unwrap_or(need.server.major);
    NoFit {
        reason,
        install: install_hint(need.engine, major, os),
    }
}

/// How to install the client tools of `engine`, version `major` or later, on `os`.
pub fn install_hint(engine: DumpEngine, major: u32, os: Os) -> String {
    match (engine, os) {
        (DumpEngine::Postgres, Os::MacOs) => format!(
            "Install PostgreSQL {major}'s client tools: `brew install libpq` (the newest) or \
             `brew install postgresql@{major}`, or Postgres.app."
        ),
        (DumpEngine::Postgres, Os::Linux) => format!(
            "Install PostgreSQL {major}'s client tools: `sudo apt install \
             postgresql-client-{major}` from apt.postgresql.org, or `sudo dnf install \
             postgresql{major}` from the PGDG repository."
        ),
        (DumpEngine::Postgres, Os::Windows) => format!(
            "Install PostgreSQL {major} from EDB's installer, with only Command Line Tools ticked."
        ),
        (DumpEngine::Mysql, Os::MacOs) => {
            "Install MySQL's client: `brew install mysql-client`.".to_owned()
        }
        (DumpEngine::Mysql, Os::Linux) => {
            "Install MySQL's client: `sudo apt install mysql-client`, or MySQL's own packages."
                .to_owned()
        }
        (DumpEngine::Mysql, Os::Windows) => {
            "Install MySQL's client programs with the MySQL Installer.".to_owned()
        }
        (DumpEngine::MariaDb, Os::MacOs) => {
            "Install MariaDB's client: `brew install mariadb`.".to_owned()
        }
        (DumpEngine::MariaDb, Os::Linux) => {
            "Install MariaDB's client: `sudo apt install mariadb-client`.".to_owned()
        }
        (DumpEngine::MariaDb, Os::Windows) => {
            "Install MariaDB's client programs with MariaDB's installer.".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(program: Program, family: DumpEngine, major: u32, minor: u32) -> FoundProgram {
        FoundProgram {
            program,
            path: PathBuf::from(format!("/opt/{program}-{major}.{minor}")),
            version: ProgramVersion {
                family,
                version: Version::new(major, minor, 0),
            },
        }
    }

    fn need(engine: DumpEngine, role: Role, server: u32) -> Need {
        Need {
            engine,
            role,
            server: Version::new(server, 4, 0),
            archive_made_by: None,
        }
    }

    #[test]
    fn prefers_the_servers_major_version_then_the_newest() {
        let found = [
            copy(Program::PgDump, DumpEngine::Postgres, 16, 4),
            copy(Program::PgDump, DumpEngine::Postgres, 17, 1),
            copy(Program::PgDump, DumpEngine::Postgres, 17, 2),
            copy(Program::PgDump, DumpEngine::Postgres, 18, 0),
        ];
        let to_17 = choose(
            &found,
            &need(DumpEngine::Postgres, Role::Dump, 17),
            Os::MacOs,
        )
        .unwrap();
        assert_eq!(to_17.program.version.version, Version::new(17, 2, 0));
        assert_eq!(to_17.warning, None);
        let to_15 = choose(
            &found,
            &need(DumpEngine::Postgres, Role::Dump, 15),
            Os::MacOs,
        )
        .unwrap();
        assert_eq!(to_15.program.version.version, Version::new(18, 0, 0));
    }

    #[test]
    fn refuses_a_pg_dump_older_than_the_server_saying_what_to_install() {
        let found = [copy(Program::PgDump, DumpEngine::Postgres, 16, 4)];
        let refused = choose(
            &found,
            &need(DumpEngine::Postgres, Role::Dump, 17),
            Os::Linux,
        )
        .unwrap_err();
        assert_eq!(refused.reason, "pg_dump 16.4 can't dump PostgreSQL 17.4.");
        assert!(
            refused.install.contains("postgresql-client-17"),
            "{}",
            refused.install
        );
        let none = choose(&[], &need(DumpEngine::Postgres, Role::Dump, 17), Os::MacOs).unwrap_err();
        assert_eq!(none.reason, "pg_dump was not found.");
    }

    #[test]
    fn pg_restore_must_be_as_new_as_the_archives_maker() {
        let found = [copy(Program::PgRestore, DumpEngine::Postgres, 16, 4)];
        let mut restore = need(DumpEngine::Postgres, Role::RestoreArchive, 16);
        restore.archive_made_by = Some(Version::new(17, 2, 0));
        let refused = choose(&found, &restore, Os::MacOs).unwrap_err();
        assert_eq!(
            refused.reason,
            "pg_restore 16.4 can't read a dump made by pg_dump 17.2."
        );
        restore.archive_made_by = Some(Version::new(15, 0, 0));
        assert!(choose(&found, &restore, Os::MacOs).is_ok());
    }

    #[test]
    fn chooses_each_familys_own_client_by_its_version() {
        let found = [
            copy(Program::Mysqldump, DumpEngine::Mysql, 8, 4),
            copy(Program::Mysqldump, DumpEngine::MariaDb, 10, 11),
            copy(Program::MariadbDump, DumpEngine::MariaDb, 11, 4),
        ];
        let mysql = choose(&found, &need(DumpEngine::Mysql, Role::Dump, 8), Os::Linux).unwrap();
        assert_eq!(mysql.program.version.family, DumpEngine::Mysql);
        let mariadb = choose(
            &found,
            &need(DumpEngine::MariaDb, Role::Dump, 11),
            Os::Linux,
        )
        .unwrap();
        assert_eq!(mariadb.program.program, Program::MariadbDump);
        let only_mysql = [copy(Program::Mysqldump, DumpEngine::Mysql, 8, 4)];
        assert!(
            choose(
                &only_mysql,
                &need(DumpEngine::MariaDb, Role::Dump, 11),
                Os::Linux
            )
            .is_err()
        );
    }

    #[test]
    fn warns_of_a_mysql_client_older_than_the_server() {
        let found = [copy(Program::Mysql, DumpEngine::Mysql, 8, 0)];
        let chosen = choose(&found, &need(DumpEngine::Mysql, Role::RunSql, 9), Os::MacOs).unwrap();
        assert!(
            chosen
                .warning
                .unwrap()
                .starts_with("mysql 8.0 is older than the server")
        );
    }
}
