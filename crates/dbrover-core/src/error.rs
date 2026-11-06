use std::error::Error;

use crate::CatalogPath;

/// The engine-specific error a driver wraps. Boxed because core can't name driver crates'
/// error types; [`DriverError`]'s variants carry what callers need to match on.
pub type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// The connection could not be established: unreachable host, bad credentials, missing
    /// file, and so on.
    #[error("could not connect")]
    Connect(#[source] BoxError),
    /// An established connection failed to do what was asked.
    #[error("the connection failed")]
    Connection(#[source] BoxError),
    /// The connection is gone: the server closed it, ended the session, restarted, or stopped
    /// answering. Nothing more can be done with it; its transaction, temporary tables and
    /// session settings went with it. Each driver decides which of its errors mean this.
    #[error("the connection was lost")]
    Lost(#[source] BoxError),
    /// A statement the user ran failed: a syntax error, a constraint violation, a
    /// cancellation, and so on.
    #[error("the statement failed")]
    Statement {
        #[source]
        source: BoxError,
        /// Byte offset into the statement's text where the error was found, when the
        /// database reports one.
        position: Option<usize>,
        /// The SQLSTATE the database reported, such as `42P01`, on engines that have them.
        /// Lets a caller tell kinds of failure apart without parsing messages.
        sqlstate: Option<String>,
    },
    /// A catalog path this driver doesn't produce, or one whose object has no definition.
    /// Points at a bug in the caller, or at a node that disappeared from the database.
    #[error("nothing to show for {0}")]
    InvalidPath(CatalogPath),
    /// Something this driver can't do yet, which the app's menus should not have offered.
    #[error("{0}")]
    Unsupported(&'static str),
}

/// Formats an error and all its sources as one line, outermost first, e.g.
/// `could not connect: db error: FATAL: password authentication failed for user "juan"`.
pub fn describe_error(error: &(dyn Error + 'static)) -> String {
    let mut description = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        description.push_str(": ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
}

/// The byte of `text` at a line and column, both counted from 1 and the column in characters,
/// as Trino and Athena point into a statement (`line 1:8: …`). The column may be one past its
/// line's end, where a statement that stops short is pointed at; `None` further on.
pub fn position_at(text: &str, line: usize, column: usize) -> Option<usize> {
    let line_start: usize = text
        .split_inclusive('\n')
        .take(line.checked_sub(1)?)
        .map(str::len)
        .sum();
    let rest = text.get(line_start..)?;
    let line_text = rest.split('\n').next().unwrap_or(rest);
    let column = column.checked_sub(1)?;
    line_text
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(line_text.len()))
        .nth(column)
        .map(|at| line_start + at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_byte_at_a_line_and_column() {
        let text = "SELECT é\nFROM t WHERE";
        let cases = [
            (1, 1, Some(0)),
            (1, 8, Some(7)),
            // After the two-byte `é`.
            (1, 9, Some(9)),
            (2, 6, Some(15)),
            // Just past the end, where a statement that stops short is pointed at.
            (2, 13, Some(22)),
            (2, 14, None),
            // Within its line: past the end of the first line is no place at all.
            (1, 10, None),
            (0, 1, None),
            (1, 0, None),
        ];
        for (line, column, expected) in cases {
            assert_eq!(position_at(text, line, column), expected, "{line}:{column}");
        }
    }

    #[test]
    fn describes_the_whole_chain() {
        let error = DriverError::Connect(BoxError::from("connection refused"));

        assert_eq!(
            describe_error(&error),
            "could not connect: connection refused"
        );
    }
}
