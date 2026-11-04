//! DuckDB's errors as DbRover errors: where in the statement they point, and what kind of
//! rejection they are.
//!
//! DuckDB places an error by quoting the statement's line and putting a caret under it:
//!
//! ```text
//! Parser Error: syntax error at or near "WHERE"
//!
//! LINE 1: SELECT id FROM t WHERE WHERE id = 1
//!                                ^
//! ```
//!
//! A long line is quoted in part, with `...` where it was cut.

use dbrover_core::{BoxError, DriverError, Rejection, RejectionKind};

/// What DuckDB shows for a quoted line cut short, at either end.
const ELLIPSIS: &str = "...";

/// DuckDB's message, without the quoted line and caret, which DbRover shows by marking the
/// statement itself.
#[derive(Debug)]
pub(crate) struct DuckDbError(String);

impl std::fmt::Display for DuckDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DuckDbError {}

/// A failed user statement, with the byte offset of the error when DuckDB says where.
pub(crate) fn statement_error(error: duckdb::Error, sql: &str) -> DriverError {
    let message = error.to_string();
    let summary = summary(&message);
    DriverError::Statement {
        position: position(&message, sql),
        source: Box::new(DuckDbError(
            explain_missing_extension(summary).unwrap_or_else(|| summary.to_owned()),
        )),
        sqlstate: None,
    }
}

/// DuckDB refusing the secret DbRover made for the data source's bucket, without the quoted
/// line DuckDB adds to some errors: that line is the statement, which holds the credentials.
pub(crate) fn secret_error(error: duckdb::Error) -> DriverError {
    let message = error.to_string();
    let summary = summary(&message);
    let source: BoxError = match explain_missing_extension(summary) {
        Some(explained) => Box::new(MissingExtension(explained)),
        None => Box::new(DuckDbError(format!(
            "DuckDB refused the bucket's credentials: {summary}"
        ))),
    };
    DriverError::Connection(source)
}

/// An extension DuckDB couldn't download, explained.
#[derive(Debug)]
pub(crate) struct MissingExtension(String);

impl std::fmt::Display for MissingExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MissingExtension {}

/// Whether `error` is DuckDB failing to install an extension, rather than the statement's or
/// the credentials' fault.
pub(crate) fn is_missing_extension(error: &DriverError) -> bool {
    matches!(error, DriverError::Connection(source) if source.is::<MissingExtension>())
}

/// What DuckDB starts the message with when it couldn't install an extension a statement
/// needs, which it does on its own the first time one is needed.
const AUTOLOADING_ERROR: &str = "Extension Autoloading Error";

/// DuckDB's message for an extension it couldn't download, with what it means and what to do
/// offline; `None` for any other message. DuckDB's own message follows, for its cause.
fn explain_missing_extension(summary: &str) -> Option<String> {
    let rest = summary.strip_prefix(AUTOLOADING_ERROR)?;
    let name = rest
        .split_once("extension '")
        .and_then(|(_, after)| after.split_once('\''))
        .map_or("an", |(name, _)| name);
    Some(format!(
        "DuckDB couldn't download its {name} extension, which this needs. DuckDB downloads \
         each extension from extensions.duckdb.org the first time it is needed, once per \
         DuckDB version, and keeps it in ~/.duckdb/extensions. Try again once this computer \
         is online, or install the extension from a file: INSTALL \
         '/path/to/{name}.duckdb_extension'.\n\n{summary}"
    ))
}

/// A statement stopped by a cancel between two fetches, where DuckDB's own interrupt had
/// nothing to stop.
pub(crate) fn interrupted() -> DriverError {
    DriverError::Statement {
        source: Box::new(DuckDbError("Interrupted".to_owned())),
        position: None,
        sqlstate: None,
    }
}

/// What preparing a statement found wrong with it, or a connection error if the failure was
/// not about the statement.
pub(crate) fn rejection(error: duckdb::Error, sql: &str) -> Result<Rejection, DriverError> {
    let duckdb::Error::DuckDBFailure(_, Some(_)) = &error else {
        return Err(DriverError::Connection(Box::new(error)));
    };
    let message = error.to_string();
    let summary = summary(&message);
    let kind = if summary.starts_with("Parser Error") {
        RejectionKind::Syntax
    } else if (summary.starts_with("Catalog Error") || summary.starts_with("Binder Error"))
        && (summary.contains("does not exist") || summary.contains("not found"))
    {
        RejectionKind::UnknownName
    } else {
        RejectionKind::Other
    };
    Ok(Rejection {
        message: summary.to_owned(),
        position: position(&message, sql),
        kind,
    })
}

/// The message up to the quoted line.
fn summary(message: &str) -> &str {
    message
        .split_once("\nLINE ")
        .map_or(message, |(summary, _)| summary)
        .trim_end()
}

/// The byte offset in `sql` that DuckDB's caret points at.
fn position(message: &str, sql: &str) -> Option<usize> {
    let mut lines = message.lines();
    let quoted = lines.find(|line| line.starts_with("LINE "))?;
    let caret_line = lines.next()?;
    let (number, text) = quoted.strip_prefix("LINE ")?.split_once(": ")?;
    let line: usize = number.parse().ok()?;
    let prefix = quoted.chars().count() - text.chars().count();
    let caret = caret_line
        .chars()
        .position(|c| c == '^')?
        .checked_sub(prefix)?;

    let line_start = if line <= 1 {
        0
    } else {
        sql.match_indices('\n').nth(line - 2)?.0 + 1
    };
    let line_text = &sql[line_start..];
    let line_text = &line_text[..line_text.find('\n').unwrap_or(line_text.len())];
    // A cut line starts and may end with `...`: find what is left of it in the statement.
    let (shown, caret) = match text.strip_prefix(ELLIPSIS) {
        Some(rest) => (rest, caret.checked_sub(ELLIPSIS.len())?),
        None => (text, caret),
    };
    let shown = shown.strip_suffix(ELLIPSIS).unwrap_or(shown);
    let shown_start = line_text.find(shown)?;
    let within = shown
        .char_indices()
        .nth(caret)
        .map_or(shown.len(), |(byte, _)| byte);
    Some(line_start + shown_start + within)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(error: &str, line: usize, text: &str, caret: usize) -> String {
        let prefix = format!("LINE {line}: ");
        format!(
            "{error}\n\n{prefix}{text}\n{}^",
            " ".repeat(prefix.len() + caret)
        )
    }

    #[test]
    fn finds_the_offset_the_caret_points_at() {
        let long = format!("SELECT a\nFROM t WHERE {} WHERE", "x AND ".repeat(30));
        let cut = format!("...{}", &long[long.len() - 40..]);
        let cases = [
            (
                "SELECT id FROM t WHERE WHERE id = 1",
                quoted(
                    "Parser Error: syntax error at or near \"WHERE\"",
                    1,
                    "SELECT id FROM t WHERE WHERE id = 1",
                    23,
                ),
                Some(23),
            ),
            (
                "SELECT a\nFROM missing",
                quoted(
                    "Catalog Error: Table with name missing does not exist!",
                    2,
                    "FROM missing",
                    5,
                ),
                Some("SELECT a\nFROM ".len()),
            ),
            (
                long.as_str(),
                quoted(
                    "Parser Error: syntax error at end of input",
                    2,
                    &cut,
                    39 + 3,
                ),
                Some(long.len() - 1),
            ),
            ("SELECT 1", "Conversion Error: no".to_owned(), None),
        ];
        for (sql, message, expected) in cases {
            assert_eq!(position(&message, sql), expected, "{message}");
        }
    }

    #[test]
    fn explains_an_extension_duckdb_couldnt_download() {
        let duckdb = "Extension Autoloading Error: An error occurred while trying to \
                      automatically install the required extension 'httpfs':\n\
                      Failed to download extension \"httpfs\" at URL \
                      \"http://127.0.0.1:9/v1.5.6/osx_arm64/httpfs.duckdb_extension.gz\"";

        let explained = explain_missing_extension(duckdb).unwrap();

        assert!(
            explained.starts_with("DuckDB couldn't download its httpfs extension"),
            "{explained}"
        );
        assert!(explained.contains("INSTALL '/path/to/httpfs.duckdb_extension'"));
        assert!(explained.ends_with(duckdb), "{explained}");
        assert_eq!(
            explain_missing_extension("Catalog Error: Table x does not exist"),
            None
        );
    }

    #[test]
    fn keeps_the_message_without_its_quoted_line() {
        let message = quoted("Parser Error: syntax error", 1, "SELECT", 0);
        assert_eq!(summary(&message), "Parser Error: syntax error");
    }
}
