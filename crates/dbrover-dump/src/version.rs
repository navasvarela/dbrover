//! Reading a program's version from what `--version` prints, and a server's from what its
//! driver reported.

use std::fmt;

use crate::program::ClientFamily;

/// A version number. Missing parts read as 0, so `18beta1` is 18.0.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The version at the start of `text`: `17.4 (Debian 17.4-1.pgdg120+2)` is 17.4.0,
    /// `11.4.4-MariaDB` 11.4.4. `None` when it doesn't start with a number.
    pub fn parse_leading(text: &str) -> Option<Self> {
        let first_word = text.split_whitespace().next()?;
        let mut parts = first_word.split('.').map(leading_number);
        let major = parts.next().flatten()?;
        let minor = parts.next().flatten().unwrap_or(0);
        let patch = parts.next().flatten().unwrap_or(0);
        Some(Self::new(major, minor, patch))
    }
}

/// The digits a part starts with: `18beta1` is 18, `4-MariaDB` 4.
fn leading_number(part: &str) -> Option<u32> {
    let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)?;
        if self.patch > 0 {
            write!(f, ".{}", self.patch)?;
        }
        Ok(())
    }
}

/// Whose client tools a copy of a program is, and which version, as its `--version` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramVersion {
    pub family: ClientFamily,
    pub version: Version,
}

impl ProgramVersion {
    /// Reads the first line of a program's `--version` output:
    ///
    /// - `pg_dump (PostgreSQL) 17.2` (with `(Homebrew)` or `(Debian …)` after it, sometimes);
    /// - `mysqldump  Ver 8.4.3 for macos15 on arm64 (Homebrew)`, and before MySQL 8
    ///   `mysqldump  Ver 10.13 Distrib 5.7.44, for Linux (x86_64)`, whose real version is the
    ///   one after `Distrib`;
    /// - `mariadb-dump from 11.4.4-MariaDB, client 10.19 for debian-linux-gnu (aarch64)`, and
    ///   an older MariaDB's `mysqldump  Ver 10.19 Distrib 10.11.6-MariaDB, for …`.
    ///
    /// `None` for anything else, which is then not a program DbRover uses.
    pub fn parse(output: &str) -> Option<Self> {
        let line = output.lines().next()?.trim();
        if let Some(rest) = line.split_once("(PostgreSQL)").map(|(_, rest)| rest) {
            return Some(Self {
                family: ClientFamily::Postgres,
                version: Version::parse_leading(rest)?,
            });
        }
        let family = if line.contains("MariaDB") {
            ClientFamily::MariaDb
        } else {
            ClientFamily::Mysql
        };
        let number = after_word(line, "Distrib")
            .or_else(|| after_word(line, "from"))
            .or_else(|| after_word(line, "Ver"))?;
        Some(Self {
            family,
            version: Version::parse_leading(number)?,
        })
    }
}

/// The word that follows `word` in `line`.
fn after_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let mut words = line.split_whitespace();
    words.by_ref().find(|candidate| *candidate == word)?;
    words.next()
}

impl fmt::Display for ProgramVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.family, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_each_program_prints() {
        let cases = [
            (
                "pg_dump (PostgreSQL) 17.2",
                ClientFamily::Postgres,
                Version::new(17, 2, 0),
            ),
            (
                "pg_restore (PostgreSQL) 16.4 (Homebrew)\n",
                ClientFamily::Postgres,
                Version::new(16, 4, 0),
            ),
            (
                "psql (PostgreSQL) 17.2 (Debian 17.2-1.pgdg120+1)",
                ClientFamily::Postgres,
                Version::new(17, 2, 0),
            ),
            (
                "pg_dump (PostgreSQL) 18beta1",
                ClientFamily::Postgres,
                Version::new(18, 0, 0),
            ),
            (
                "mysqldump  Ver 8.4.3 for macos15 on arm64 (Homebrew)",
                ClientFamily::Mysql,
                Version::new(8, 4, 3),
            ),
            (
                "mysqldump  Ver 10.13 Distrib 5.7.44, for Linux (x86_64)",
                ClientFamily::Mysql,
                Version::new(5, 7, 44),
            ),
            (
                "mariadb-dump from 11.4.4-MariaDB, client 10.19 for debian-linux-gnu (aarch64)",
                ClientFamily::MariaDb,
                Version::new(11, 4, 4),
            ),
            (
                "mysqldump  Ver 10.19 Distrib 10.11.6-MariaDB, for debian-linux-gnu (x86_64)",
                ClientFamily::MariaDb,
                Version::new(10, 11, 6),
            ),
            (
                "mysql  Ver 8.0.40-0ubuntu0.24.04.1 for Linux on x86_64 ((Ubuntu))",
                ClientFamily::Mysql,
                Version::new(8, 0, 40),
            ),
        ];
        for (output, family, version) in cases {
            assert_eq!(
                ProgramVersion::parse(output),
                Some(ProgramVersion { family, version }),
                "{output}"
            );
        }
    }

    #[test]
    fn refuses_what_isnt_a_version() {
        for output in [
            "",
            "command not found",
            "pg_dump (PostgreSQL) unknown",
            "mysqldump Ver",
        ] {
            assert_eq!(ProgramVersion::parse(output), None, "{output:?}");
        }
    }

    #[test]
    fn reads_servers_versions_as_their_drivers_report_them() {
        assert_eq!(
            Version::parse_leading("17.4 (Debian 17.4-1.pgdg120+2)"),
            Some(Version::new(17, 4, 0))
        );
        assert_eq!(
            Version::parse_leading("11.4.4"),
            Some(Version::new(11, 4, 4))
        );
        assert_eq!(Version::parse_leading("devel"), None);
        assert_eq!(Version::new(17, 2, 0).to_string(), "17.2");
        assert_eq!(Version::new(8, 4, 3).to_string(), "8.4.3");
    }
}
