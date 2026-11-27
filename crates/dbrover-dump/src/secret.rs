//! The files a program reads its password from, so it is never on a command line, where `ps`
//! shows it, nor in the environment, which every child of the program inherits and which
//! `/proc` shows on Linux. PostgreSQL's manual advises against `PGPASSWORD`, and MySQL
//! deprecated `MYSQL_PWD` in 8.0.
//!
//! The engine writes these files where only the user can read them and removes them once the
//! program has connected; this module only says what goes in them.

use dbrover_core::DumpEngine;

/// Why a password can't be written for a program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error(
        "the password has a line break, which a PostgreSQL password file can't hold; pg_dump \
         can't be given it"
    )]
    LineBreak,
}

/// The password file `engine`'s programs read: for PostgreSQL a `PGPASSFILE`, for MySQL and
/// MariaDB an option file given as `--defaults-file`.
///
/// # Errors
///
/// [`SecretError::LineBreak`] for a PostgreSQL password with a line break in it.
pub fn password_file(engine: DumpEngine, password: &str) -> Result<String, SecretError> {
    match engine {
        DumpEngine::Postgres => pgpass(password),
        DumpEngine::Mysql | DumpEngine::MariaDb => Ok(option_file(password)),
    }
}

/// One `.pgpass` line matching any host, port, database and user: the file is written for one
/// run of one program, so there is nothing to tell apart, and a tunnel's port or a `hostaddr`
/// can't make it miss. `:` and `\` are escaped with a backslash, as libpq reads them.
fn pgpass(password: &str) -> Result<String, SecretError> {
    if password.contains(['\n', '\r']) {
        return Err(SecretError::LineBreak);
    }
    let escaped = password.replace('\\', "\\\\").replace(':', "\\:");
    Ok(format!("*:*:*:*:{escaped}\n"))
}

/// A `[client]` group with the password in double quotes, so `#`, `;`, quotes and spaces at
/// either end are kept: MySQL's and MariaDB's option file reader strips one pair of quotes
/// around a value and reads `\\`, `\n`, `\r`, `\t` and `\b` escapes inside it.
fn option_file(password: &str) -> String {
    let mut escaped = String::with_capacity(password.len());
    for c in password.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{8}' => escaped.push_str("\\b"),
            other => escaped.push(other),
        }
    }
    format!("[client]\npassword=\"{escaped}\"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_a_pgpass_line() {
        assert_eq!(
            password_file(DumpEngine::Postgres, r"a:b\c#d e").unwrap(),
            "*:*:*:*:a\\:b\\\\c#d e\n"
        );
        assert_eq!(
            password_file(DumpEngine::Postgres, "two\nlines"),
            Err(SecretError::LineBreak)
        );
    }

    #[test]
    fn quotes_and_escapes_an_option_file() {
        assert_eq!(
            password_file(DumpEngine::Mysql, "p\"a#ss;\\ \n\t").unwrap(),
            "[client]\npassword=\"p\"a#ss;\\\\ \\n\\t\"\n"
        );
        assert_eq!(
            password_file(DumpEngine::MariaDb, " spaced ").unwrap(),
            "[client]\npassword=\" spaced \"\n"
        );
    }
}
