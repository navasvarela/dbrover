//! Where a program connects, written as each engine's client takes it: a libpq connection
//! string for PostgreSQL, options for MySQL's and MariaDB's clients. Never the password, which
//! goes in a file of its own ([`crate::secret`]).

use std::path::PathBuf;

use dbrover_core::{DumpEngine, TlsMode};

use crate::version::Version;

/// The server a program connects to, from the data source's resolved settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The server's name, as the data source has it: what TLS checks the certificate against.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub tls: TlsMode,
    /// The local port of the data source's SSH tunnel, when it has one: the program connects
    /// to `127.0.0.1` on it.
    pub tunnel_port: Option<u16>,
}

/// Where a tunnel listens.
const TUNNEL_ADDRESS: &str = "127.0.0.1";

/// What PostgreSQL's client is called by in `pg_stat_activity`, so Server activity names it.
pub const APPLICATION_NAME: &str = "DbRover dump";

/// Why a program can't connect as the data source does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    #[error(
        "checking the server's certificate against the system's authorities needs libpq 16 or \
         later, and this program is {0}; choose a newer one, or set a CA file in the data source"
    )]
    SystemRootsNeedLibpq16(Version),
}

/// libpq 16 added `sslrootcert=system`.
const SYSTEM_ROOTS_SINCE: u32 = 16;

impl Target {
    /// The libpq connection string for `--dbname`: `host=… port=… user=… dbname=… sslmode=…`.
    /// Through a tunnel, `hostaddr` is where it connects and `host` stays the server's name, so
    /// `verify-full` still checks the certificate against it.
    ///
    /// # Errors
    ///
    /// [`ConnectionError::SystemRootsNeedLibpq16`] for `verify-full` without a CA file on a
    /// program older than libpq 16.
    pub fn conninfo(&self, program: Version) -> Result<String, ConnectionError> {
        let port = self.tunnel_port.unwrap_or(self.port).to_string();
        let mut pairs: Vec<(&str, String)> = vec![("host", self.host.clone())];
        if self.tunnel_port.is_some() {
            pairs.push(("hostaddr", TUNNEL_ADDRESS.to_owned()));
        }
        pairs.push(("port", port));
        pairs.push(("user", self.user.clone()));
        pairs.push(("dbname", self.database.clone()));
        let (mode, root) = postgres_tls(&self.tls);
        pairs.push(("sslmode", mode.to_owned()));
        match root {
            Root::None => {}
            Root::File(file) => pairs.push(("sslrootcert", file.to_string_lossy().into_owned())),
            Root::System if program.major < SYSTEM_ROOTS_SINCE => {
                return Err(ConnectionError::SystemRootsNeedLibpq16(program));
            }
            Root::System => pairs.push(("sslrootcert", "system".to_owned())),
        }
        pairs.push(("application_name", APPLICATION_NAME.to_owned()));
        Ok(pairs
            .iter()
            .map(|(key, value)| format!("{key}={}", conninfo_value(value)))
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// The options MySQL's or MariaDB's client connects with: host, port, user, TCP, and TLS
    /// in the family's own spelling. Through a tunnel, MySQL's client can't check the
    /// certificate's name against `127.0.0.1`, so `verify-full` checks its authority only.
    pub fn mysql_options(&self, family: DumpEngine) -> Vec<String> {
        let (host, port) = match self.tunnel_port {
            Some(port) => (TUNNEL_ADDRESS.to_owned(), port),
            None => (self.host.clone(), self.port),
        };
        let mut options = vec![
            format!("--host={host}"),
            format!("--port={port}"),
            format!("--user={}", self.user),
            // Without it, `localhost` is the Unix socket, which DbRover's own connection
            // never uses.
            "--protocol=TCP".to_owned(),
        ];
        let tunnelled = self.tunnel_port.is_some();
        if family == DumpEngine::MariaDb {
            options.extend(mariadb_tls(&self.tls));
        } else {
            options.extend(mysql_tls(&self.tls, tunnelled));
        }
        options
    }
}

enum Root<'a> {
    None,
    File(&'a PathBuf),
    System,
}

fn postgres_tls(tls: &TlsMode) -> (&'static str, Root<'_>) {
    match tls {
        TlsMode::Disable => ("disable", Root::None),
        TlsMode::Prefer => ("prefer", Root::None),
        TlsMode::Require => ("require", Root::None),
        TlsMode::VerifyFull {
            ca_file: Some(file),
        } => ("verify-full", Root::File(file)),
        TlsMode::VerifyFull { ca_file: None } => ("verify-full", Root::System),
    }
}

fn mysql_tls(tls: &TlsMode, tunnelled: bool) -> Vec<String> {
    let mode = match tls {
        TlsMode::Disable => "DISABLED",
        TlsMode::Prefer => "PREFERRED",
        TlsMode::Require => "REQUIRED",
        TlsMode::VerifyFull { .. } if tunnelled => "VERIFY_CA",
        TlsMode::VerifyFull { .. } => "VERIFY_IDENTITY",
    };
    let mut options = vec![format!("--ssl-mode={mode}")];
    options.extend(ca_option(tls));
    options
}

/// MariaDB's client spells TLS as switches, and since 11.4 verifies the server's certificate
/// unless told not to.
fn mariadb_tls(tls: &TlsMode) -> Vec<String> {
    let mut options: Vec<String> = match tls {
        TlsMode::Disable => vec!["--skip-ssl".to_owned()],
        TlsMode::Prefer => vec!["--skip-ssl-verify-server-cert".to_owned()],
        TlsMode::Require => vec![
            "--ssl".to_owned(),
            "--skip-ssl-verify-server-cert".to_owned(),
        ],
        TlsMode::VerifyFull { .. } => {
            vec!["--ssl".to_owned(), "--ssl-verify-server-cert".to_owned()]
        }
    };
    options.extend(ca_option(tls));
    options
}

fn ca_option(tls: &TlsMode) -> Option<String> {
    match tls {
        TlsMode::VerifyFull {
            ca_file: Some(file),
        } => Some(format!("--ssl-ca={}", file.to_string_lossy())),
        _ => None,
    }
}

/// A value in a libpq connection string: as it is when it is plain, otherwise in single
/// quotes with `'` and `\` escaped by a backslash, as libpq's `conninfo_parse` reads it.
fn conninfo_value(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '\\' || c == '=');
    if plain {
        return value.to_owned();
    }
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(tls: TlsMode, tunnel_port: Option<u16>) -> Target {
        Target {
            host: "db.internal".to_owned(),
            port: 5432,
            user: "ada".to_owned(),
            database: "shop".to_owned(),
            tls,
            tunnel_port,
        }
    }

    const PG17: Version = Version::new(17, 2, 0);

    #[test]
    fn writes_a_conninfo_for_each_tls_mode() {
        let cases = [
            (TlsMode::Disable, "sslmode=disable"),
            (TlsMode::Prefer, "sslmode=prefer"),
            (TlsMode::Require, "sslmode=require"),
            (
                TlsMode::VerifyFull {
                    ca_file: Some("/certs/my ca.pem".into()),
                },
                "sslmode=verify-full sslrootcert='/certs/my ca.pem'",
            ),
            (
                TlsMode::VerifyFull { ca_file: None },
                "sslmode=verify-full sslrootcert=system",
            ),
        ];
        for (tls, expected) in cases {
            assert_eq!(
                target(tls, None).conninfo(PG17).unwrap(),
                format!(
                    "host=db.internal port=5432 user=ada dbname=shop {expected} \
                     application_name='DbRover dump'"
                )
            );
        }
    }

    #[test]
    fn keeps_the_servers_name_through_a_tunnel() {
        assert_eq!(
            target(TlsMode::VerifyFull { ca_file: None }, Some(61234))
                .conninfo(PG17)
                .unwrap(),
            "host=db.internal hostaddr=127.0.0.1 port=61234 user=ada dbname=shop \
             sslmode=verify-full sslrootcert=system application_name='DbRover dump'"
        );
    }

    #[test]
    fn system_roots_need_libpq_16() {
        let old = Version::new(15, 8, 0);
        assert_eq!(
            target(TlsMode::VerifyFull { ca_file: None }, None).conninfo(old),
            Err(ConnectionError::SystemRootsNeedLibpq16(old))
        );
    }

    #[test]
    fn quotes_awkward_values() {
        let mut awkward = target(TlsMode::Disable, None);
        awkward.user = "o'brien".to_owned();
        awkward.database = r"we\ird db".to_owned();
        let conninfo = awkward.conninfo(PG17).unwrap();
        assert!(conninfo.contains(r"user='o\'brien'"), "{conninfo}");
        assert!(conninfo.contains(r"dbname='we\\ird db'"), "{conninfo}");
        assert_eq!(conninfo_value(""), "''");
        assert_eq!(conninfo_value("a=b"), "'a=b'");
    }

    #[test]
    fn writes_mysql_options_in_each_familys_spelling() {
        let verify = TlsMode::VerifyFull {
            ca_file: Some("/certs/ca.pem".into()),
        };
        assert_eq!(
            target(verify.clone(), None).mysql_options(DumpEngine::Mysql),
            [
                "--host=db.internal",
                "--port=5432",
                "--user=ada",
                "--protocol=TCP",
                "--ssl-mode=VERIFY_IDENTITY",
                "--ssl-ca=/certs/ca.pem"
            ]
        );
        assert_eq!(
            target(verify.clone(), Some(61234)).mysql_options(DumpEngine::Mysql)[..5],
            [
                "--host=127.0.0.1",
                "--port=61234",
                "--user=ada",
                "--protocol=TCP",
                "--ssl-mode=VERIFY_CA"
            ]
        );
        assert_eq!(
            target(verify, None).mysql_options(DumpEngine::MariaDb)[4..],
            [
                "--ssl",
                "--ssl-verify-server-cert",
                "--ssl-ca=/certs/ca.pem"
            ]
        );
        assert_eq!(
            target(TlsMode::Disable, None).mysql_options(DumpEngine::MariaDb)[4..],
            ["--skip-ssl"]
        );
        assert_eq!(
            target(TlsMode::Prefer, None).mysql_options(DumpEngine::Mysql)[4..],
            ["--ssl-mode=PREFERRED"]
        );
    }
}
