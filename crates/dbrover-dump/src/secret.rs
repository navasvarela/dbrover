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
    #[error(
        "the password's quotes and # can't be written to a MySQL option file so that the \
         client reads them back"
    )]
    OptionFile,
}

/// The password file `engine`'s programs read: for PostgreSQL a `PGPASSFILE`, for MySQL and
/// MariaDB an option file given as `--defaults-file`.
///
/// # Errors
///
/// [`SecretError::LineBreak`] for a PostgreSQL password with a line break in it, and
/// [`SecretError::OptionFile`] for a MySQL one whose quotes and `#` the client would misread.
pub fn password_file(engine: DumpEngine, password: &str) -> Result<String, SecretError> {
    match engine {
        DumpEngine::Postgres => pgpass(password),
        DumpEngine::Mysql | DumpEngine::MariaDb => option_file(password),
        // They dump themselves: there is no program to give a password.
        DumpEngine::Sqlite | DumpEngine::DuckDb => Ok(String::new()),
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

/// A `[client]` group with the password in quotes, so `#`, `;` and spaces at either end are
/// kept: MySQL's and MariaDB's option file reader strips the first and last character of a
/// value when they are the same quote, and reads `\\`, `\n`, `\r`, `\t` and `\b` escapes
/// inside it. A quote inside can't be escaped (`\"` stays two characters), and the reader
/// takes a `#` it finds outside a quoted stretch as a comment's start, so the quote is the
/// kind the password lacks, and a line the reader would cut short is refused.
fn option_file(password: &str) -> Result<String, SecretError> {
    let escaped = option_escaped(password);
    ['"', '\'']
        .into_iter()
        .map(|quote| format!("password={quote}{escaped}{quote}"))
        .find(|line| !cut_by_a_comment(line))
        .map(|line| format!("[client]\n{line}\n"))
        .ok_or(SecretError::OptionFile)
}

/// Whether MySQL's option file reader would take part of `line` for a comment: its
/// `remove_end_comment`, which ends the line at a `#` outside a quoted stretch, a quote
/// opening or closing a stretch unless a backslash inside one escapes it.
fn cut_by_a_comment(line: &str) -> bool {
    let mut quote = None;
    let mut escape = false;
    for c in line.chars() {
        if (c == '\'' || c == '"') && !escape {
            quote = match quote {
                None => Some(c),
                Some(open) if open == c => None,
                open => open,
            };
        } else if quote.is_none() && c == '#' {
            return true;
        }
        escape = quote.is_some() && c == '\\' && !escape;
    }
    false
}

fn option_escaped(password: &str) -> String {
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
    escaped
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
            password_file(DumpEngine::Mysql, "pa#ss;\\ \n\t").unwrap(),
            "[client]\npassword=\"pa#ss;\\\\ \\n\\t\"\n"
        );
        assert_eq!(
            password_file(DumpEngine::MariaDb, " spaced ").unwrap(),
            "[client]\npassword=\" spaced \"\n"
        );
    }

    #[test]
    fn quotes_with_the_quote_the_password_lacks() {
        assert_eq!(
            password_file(DumpEngine::Mysql, " p\"a#ss ").unwrap(),
            "[client]\npassword=' p\"a#ss '\n"
        );
        // Both quotes but no `#`: nothing is cut, whichever quote opens.
        assert_eq!(
            password_file(DumpEngine::Mysql, "it's \"x\"").unwrap(),
            "[client]\npassword=\"it's \"x\"\"\n"
        );
        // Quoted either way, a `#` is left outside a quoted stretch.
        assert_eq!(
            password_file(DumpEngine::Mysql, "\"#'#"),
            Err(SecretError::OptionFile)
        );
    }
}
