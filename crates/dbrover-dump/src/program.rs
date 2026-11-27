//! The programs DbRover runs, and the systems it looks for them on.

use std::fmt;

use dbrover_core::DumpEngine;

/// A program of an engine's client tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Program {
    PgDump,
    PgRestore,
    Psql,
    Mysqldump,
    Mysql,
    MariadbDump,
    Mariadb,
}

/// What a program is for, whichever engine's it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Writes a dump.
    Dump,
    /// Restores a PostgreSQL archive (custom or directory).
    RestoreArchive,
    /// Runs a plain SQL dump.
    RunSql,
}

impl Program {
    pub const ALL: [Self; 7] = [
        Self::PgDump,
        Self::PgRestore,
        Self::Psql,
        Self::Mysqldump,
        Self::Mysql,
        Self::MariadbDump,
        Self::Mariadb,
    ];

    /// The program's name, without `.exe`.
    pub fn name(self) -> &'static str {
        match self {
            Self::PgDump => "pg_dump",
            Self::PgRestore => "pg_restore",
            Self::Psql => "psql",
            Self::Mysqldump => "mysqldump",
            Self::Mysql => "mysql",
            Self::MariadbDump => "mariadb-dump",
            Self::Mariadb => "mariadb",
        }
    }

    /// The program's file name on `os`.
    pub fn file_name(self, os: Os) -> String {
        match os {
            Os::Windows => format!("{}.exe", self.name()),
            Os::MacOs | Os::Linux => self.name().to_owned(),
        }
    }

    /// The programs that do `role` for `engine`, the engine's own name first. MariaDB installs
    /// its programs under MySQL's names too, and older MariaDBs only under those; which family
    /// a copy is comes from its version, not its name.
    pub fn for_role(engine: DumpEngine, role: Role) -> &'static [Self] {
        match (engine, role) {
            (DumpEngine::Postgres, Role::Dump) => &[Self::PgDump],
            (DumpEngine::Postgres, Role::RestoreArchive) => &[Self::PgRestore],
            (DumpEngine::Postgres, Role::RunSql) => &[Self::Psql],
            (DumpEngine::Mysql, Role::Dump) => &[Self::Mysqldump],
            (DumpEngine::Mysql, Role::RunSql) => &[Self::Mysql],
            (DumpEngine::MariaDb, Role::Dump) => &[Self::MariadbDump, Self::Mysqldump],
            (DumpEngine::MariaDb, Role::RunSql) => &[Self::Mariadb, Self::Mysql],
            (DumpEngine::Mysql | DumpEngine::MariaDb, Role::RestoreArchive) => &[],
        }
    }

    /// The client tools' family the program is first looked for under: PostgreSQL's, MySQL's
    /// or MariaDB's. A `mysqldump` may still turn out to be MariaDB's.
    pub fn family(self) -> DumpEngine {
        match self {
            Self::PgDump | Self::PgRestore | Self::Psql => DumpEngine::Postgres,
            Self::Mysqldump | Self::Mysql => DumpEngine::Mysql,
            Self::MariadbDump | Self::Mariadb => DumpEngine::MariaDb,
        }
    }

    /// The key a path set by hand is kept under in `settings.toml`'s `[dumps.programs]`.
    pub fn setting_key(self) -> &'static str {
        match self {
            Self::PgDump => "pg_dump",
            Self::PgRestore => "pg_restore",
            Self::Psql => "psql",
            Self::Mysqldump => "mysqldump",
            Self::Mysql => "mysql",
            Self::MariadbDump => "mariadb_dump",
            Self::Mariadb => "mariadb",
        }
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The systems DbRover is built for, which decide where programs are installed and how they
/// are named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    /// The system DbRover was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_programs_as_each_system_does() {
        assert_eq!(Program::PgDump.file_name(Os::MacOs), "pg_dump");
        assert_eq!(
            Program::MariadbDump.file_name(Os::Windows),
            "mariadb-dump.exe"
        );
    }

    #[test]
    fn mariadb_is_also_looked_for_under_mysqls_names() {
        assert_eq!(
            Program::for_role(DumpEngine::MariaDb, Role::Dump),
            [Program::MariadbDump, Program::Mysqldump]
        );
        assert_eq!(
            Program::for_role(DumpEngine::Mysql, Role::Dump),
            [Program::Mysqldump]
        );
        assert!(Program::for_role(DumpEngine::Mysql, Role::RestoreArchive).is_empty());
    }
}
