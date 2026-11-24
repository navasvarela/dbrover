//! How SQL that DbRover writes itself quotes identifiers and strings, which differs between
//! dialects: MySQL reads `"x"` as a string and `\` inside a string as an escape, and SQL
//! Server reads `"x"` as an identifier only while the session's `QUOTED_IDENTIFIER` is on.

/// The quoting rules of one dialect, for generated SQL. `dbrover-sql`'s `Dialect::quoting`
/// says which applies to a data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quoting {
    /// Opens a quoted identifier.
    pub identifier_open: char,
    /// Closes a quoted identifier; doubled inside one to stand for itself. The same as
    /// `identifier_open` except for SQL Server's `[brackets]`.
    pub identifier_close: char,
    /// Whether a backslash inside a string literal escapes the next character, so a literal
    /// backslash has to be written twice.
    pub backslash_escapes: bool,
    /// Written before a string literal's opening quote: `N` on SQL Server, where a plain
    /// `'…'` is read in the database's code page and loses the characters it lacks.
    pub string_prefix: &'static str,
    /// Which names read the same without quotes, which [`Self::identifier_if_needed`] leaves
    /// bare.
    pub bare_names: BareNames,
    /// How a quote inside a string literal is written.
    pub quote_escape: QuoteEscape,
}

/// How a dialect writes a quote inside a string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteEscape {
    /// Doubled, as the SQL standard writes it: `'it''s'`.
    Doubled,
    /// After a backslash: `'it\'s'`. Databricks reads `'it''s'` as two strings side by side,
    /// which Spark joins into `its`.
    Backslash,
}

/// Which names a dialect reads the same quoted or not, so that text a person reads may leave
/// them bare: that depends on how the dialect folds the case of an unquoted name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareNames {
    /// Plain lowercase names, which read the same in every dialect that folds unquoted names
    /// to lowercase (PostgreSQL) or matches them whatever their case.
    Lowercase,
    /// Plain uppercase names, which may hold `$` after their first character, and aren't one
    /// of `reserved` (uppercase): Snowflake folds unquoted names to uppercase, as the SQL
    /// standard does, so `ORDERS` is the table created as `orders`, and `"orders"` another.
    Uppercase { reserved: &'static [&'static str] },
}

impl Quoting {
    /// Standard SQL, as PostgreSQL and SQLite read it: `"identifier"`, `'it''s'`.
    pub const STANDARD: Self = Self {
        identifier_open: '"',
        identifier_close: '"',
        backslash_escapes: false,
        string_prefix: "",
        bare_names: BareNames::Lowercase,
        quote_escape: QuoteEscape::Doubled,
    };

    /// Quotes `name` even where it needs no quoting, as generated statements must not depend
    /// on whether a name happens to be a keyword.
    pub fn identifier(self, name: &str) -> String {
        let (open, close) = (self.identifier_open, self.identifier_close);
        let doubled = format!("{close}{close}");
        format!("{open}{}{close}", name.replace(close, &doubled))
    }

    /// Quotes `name` unless it reads the same without quotes (see [`BareNames`]). For text a
    /// person reads and edits, such as completions.
    pub fn identifier_if_needed(self, name: &str) -> String {
        if self.bare_names.reads_bare(name) {
            name.to_owned()
        } else {
            self.identifier(name)
        }
    }

    /// `text` as a string literal.
    pub fn string_literal(self, text: &str) -> String {
        let escaped = if self.backslash_escapes {
            text.replace('\\', "\\\\")
        } else {
            text.to_owned()
        };
        let quote = match self.quote_escape {
            QuoteEscape::Doubled => "''",
            QuoteEscape::Backslash => "\\'",
        };
        format!("{}'{}'", self.string_prefix, escaped.replace('\'', quote))
    }
}

impl BareNames {
    fn reads_bare(self, name: &str) -> bool {
        let mut chars = name.chars();
        match self {
            Self::Lowercase => {
                chars
                    .next()
                    .is_some_and(|first| first.is_ascii_lowercase() || first == '_')
                    && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            }
            Self::Uppercase { reserved } => {
                chars
                    .next()
                    .is_some_and(|first| first.is_ascii_uppercase() || first == '_')
                    && chars.all(|c| {
                        c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$')
                    })
                    && !reserved.contains(&name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKTICKS: Quoting = Quoting {
        identifier_open: '`',
        identifier_close: '`',
        backslash_escapes: true,
        string_prefix: "",
        bare_names: BareNames::Lowercase,
        quote_escape: QuoteEscape::Doubled,
    };

    const BRACKETS: Quoting = Quoting {
        identifier_open: '[',
        identifier_close: ']',
        backslash_escapes: false,
        string_prefix: "N",
        bare_names: BareNames::Lowercase,
        quote_escape: QuoteEscape::Doubled,
    };

    const UPPERCASE: Quoting = Quoting {
        bare_names: BareNames::Uppercase {
            reserved: &["ORDER", "TABLE"],
        },
        ..Quoting::STANDARD
    };

    #[test]
    fn quotes_identifiers_doubling_the_quote_inside() {
        let cases = [
            (Quoting::STANDARD, "users", "\"users\"", "users"),
            (
                Quoting::STANDARD,
                "Order \"Items\"",
                "\"Order \"\"Items\"\"\"",
                "\"Order \"\"Items\"\"\"",
            ),
            (BACKTICKS, "order`s", "`order``s`", "`order``s`"),
            (BACKTICKS, "Users", "`Users`", "`Users`"),
            (BACKTICKS, "users_2", "`users_2`", "users_2"),
            (BRACKETS, "a]b[c", "[a]]b[c]", "[a]]b[c]"),
            (BRACKETS, "order", "[order]", "order"),
            (UPPERCASE, "ORDERS", "\"ORDERS\"", "ORDERS"),
            (UPPERCASE, "MY$TABLE_2", "\"MY$TABLE_2\"", "MY$TABLE_2"),
            (UPPERCASE, "_X", "\"_X\"", "_X"),
            (UPPERCASE, "orders", "\"orders\"", "\"orders\""),
            (UPPERCASE, "Orders", "\"Orders\"", "\"Orders\""),
            (UPPERCASE, "ORDER", "\"ORDER\"", "\"ORDER\""),
            (UPPERCASE, "$X", "\"$X\"", "\"$X\""),
            (UPPERCASE, "2X", "\"2X\"", "\"2X\""),
            (Quoting::STANDARD, "MY$T", "\"MY$T\"", "\"MY$T\""),
        ];
        for (quoting, name, always, if_needed) in cases {
            assert_eq!(quoting.identifier(name), always, "{name}");
            assert_eq!(quoting.identifier_if_needed(name), if_needed, "{name}");
        }
    }

    #[test]
    fn escapes_backslashes_only_where_they_are_escapes() {
        let text = r"it's C:\temp";

        assert_eq!(Quoting::STANDARD.string_literal(text), r"'it''s C:\temp'");
        assert_eq!(BACKTICKS.string_literal(text), r"'it''s C:\\temp'");
        assert_eq!(BRACKETS.string_literal("José"), "N'José'");
    }
}
