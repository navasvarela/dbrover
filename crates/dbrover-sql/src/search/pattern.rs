//! `LIKE` patterns that match the searched text as itself.
//!
//! The text reaches a statement in two steps, each the dialect's own: here its wildcards and
//! the escape character are escaped, then [`dbrover_core::Quoting::string_literal`] writes the
//! pattern as a literal. Keeping them apart means a backslash added here is doubled there on
//! exactly the dialects whose literals read backslashes.

use dbrover_core::MatchKind;

use crate::Dialect;

/// How a dialect's `LIKE` is told which character escapes a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LikeEscape {
    pub character: char,
    /// Whether the statement names it with `ESCAPE`, rather than the dialect always reading it.
    pub named: bool,
}

impl LikeEscape {
    /// What follows the pattern: ` ESCAPE '!'`, or nothing.
    pub fn clause(self) -> String {
        if self.named {
            format!(" ESCAPE '{}'", self.character)
        } else {
            String::new()
        }
    }
}

impl Dialect {
    /// `!` named with `ESCAPE` wherever the dialect takes one. A backslash is also an escape
    /// inside MySQL's, MariaDB's, Snowflake's and Databricks's string literals, unless MySQL's
    /// `NO_BACKSLASH_ESCAPES` is set, which DbRover can't know before it asks; `!` means
    /// nothing in any literal, so the pattern means the same whatever the server's settings.
    /// SQLite's `LIKE` has no escape at all until one is named. ClickHouse's and GoogleSQL's
    /// `LIKE` take no `ESCAPE` and always read `\`.
    pub(crate) fn like_escape(self) -> LikeEscape {
        match self {
            Self::ClickHouse | Self::BigQuery => LikeEscape {
                character: '\\',
                named: false,
            },
            Self::Postgres
            | Self::Sqlite
            | Self::Mysql
            | Self::SqlServer
            | Self::DuckDb
            | Self::Cassandra
            | Self::Trino
            | Self::Snowflake
            | Self::Databricks => LikeEscape {
                character: '!',
                named: true,
            },
        }
    }

    /// Whether the character opens something in the dialect's `LIKE` patterns: `%` and `_`
    /// everywhere, and on SQL Server `[`, which opens a set of characters (`[a-c]`).
    fn is_like_wildcard(self, character: char) -> bool {
        matches!(character, '%' | '_') || (self == Self::SqlServer && character == '[')
    }
}

/// The `LIKE` pattern, before it is written as a literal, that matches `text` as
/// `matching` says: anywhere in a value, or as the whole of it.
pub(crate) fn like_pattern(dialect: Dialect, text: &str, matching: MatchKind) -> String {
    let escape = dialect.like_escape().character;
    let mut pattern = String::with_capacity(text.len() + 2);
    if matching == MatchKind::Contains {
        pattern.push('%');
    }
    for character in text.chars() {
        if character == escape || dialect.is_like_wildcard(character) {
            pattern.push(escape);
        }
        pattern.push(character);
    }
    if matching == MatchKind::Contains {
        pattern.push('%');
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_each_dialects_wildcards_and_its_escape() {
        let hard = r"50%_off! [x] C:\y";
        let cases = [
            (Dialect::Postgres, r"%50!%!_off!! [x] C:\y%"),
            (Dialect::Mysql, r"%50!%!_off!! [x] C:\y%"),
            (Dialect::SqlServer, r"%50!%!_off!! ![x] C:\y%"),
            (Dialect::ClickHouse, r"%50\%\_off! [x] C:\\y%"),
            (Dialect::BigQuery, r"%50\%\_off! [x] C:\\y%"),
        ];
        for (dialect, pattern) in cases {
            assert_eq!(
                like_pattern(dialect, hard, MatchKind::Contains),
                pattern,
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn a_whole_value_has_no_wildcards_around_it() {
        assert_eq!(
            like_pattern(Dialect::Postgres, "a_b", MatchKind::WholeValue),
            "a!_b"
        );
    }

    #[test]
    fn a_value_of_nothing_but_wildcards_matches_only_itself() {
        assert_eq!(
            like_pattern(Dialect::SqlServer, "%_![", MatchKind::Contains),
            "%!%!_!!![%"
        );
    }

    #[test]
    fn the_escape_is_named_where_the_dialect_takes_one() {
        assert_eq!(Dialect::Postgres.like_escape().clause(), " ESCAPE '!'");
        assert_eq!(Dialect::ClickHouse.like_escape().clause(), "");
    }
}
