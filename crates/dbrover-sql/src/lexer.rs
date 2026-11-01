use std::ops::Range;

use crate::dialect::{Dialect, LexicalRules};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    /// A string literal, including dollar-quoted bodies.
    String,
    QuotedIdentifier,
    Number,
    /// A keyword or an unquoted identifier; see [`is_keyword`](crate::is_keyword).
    Word,
    Semicolon,
    /// Any other single character: operators, parentheses, commas, `$1` markers…
    Punctuation,
}

impl TokenKind {
    /// Whitespace and comments: tokens with no meaning to the database.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// Splits `sql` into tokens that together cover every byte of it, in order.
pub fn tokenize(sql: &str, dialect: Dialect) -> Vec<Token> {
    let mut lexer = Lexer {
        sql,
        position: 0,
        rules: dialect.lexical_rules(),
    };
    let mut tokens = Vec::new();
    while lexer.position < sql.len() {
        let start = lexer.position;
        let kind = lexer.next_kind();
        tokens.push(Token {
            kind,
            range: start..lexer.position,
        });
    }
    tokens
}

/// The statement's first word, lowercased, skipping comments: usually its verb (`select`,
/// `update`, `create`, …).
pub fn first_word(sql: &str, dialect: Dialect) -> Option<String> {
    tokenize(sql, dialect)
        .into_iter()
        .find(|token| !token.kind.is_trivia())
        .filter(|token| token.kind == TokenKind::Word)
        .map(|token| sql[token.range].to_ascii_lowercase())
}

struct Lexer<'a> {
    sql: &'a str,
    position: usize,
    rules: LexicalRules,
}

impl Lexer<'_> {
    fn rest(&self) -> &str {
        &self.sql[self.position..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.position += c.len_utf8();
        Some(c)
    }

    fn bump_while(&mut self, predicate: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&predicate) {
            self.bump();
        }
    }

    /// Consumes one token, which always advances by at least one character.
    fn next_kind(&mut self) -> TokenKind {
        let Some(first) = self.peek() else {
            return TokenKind::Whitespace;
        };
        let second = self.peek_second();
        match (first, second) {
            (c, _) if c.is_whitespace() => {
                self.bump_while(char::is_whitespace);
                TokenKind::Whitespace
            }
            ('-', Some('-')) if self.dashes_start_comment() => self.line_comment(),
            ('#', _) if self.rules.hash_comments => self.line_comment(),
            ('/', Some('/')) if self.rules.slash_comments => self.line_comment(),
            ('/', Some('*')) => self.block_comment(),
            ('\'', _) => self.quoted('\'', TokenKind::String),
            ('e' | 'E', Some('\'')) if self.rules.escape_strings => {
                self.bump();
                self.escape_string()
            }
            ('n' | 'N', Some('\'')) if self.rules.national_strings => {
                self.bump();
                self.quoted('\'', TokenKind::String)
            }
            ('"', _) if self.rules.double_quoted_strings => self.quoted('"', TokenKind::String),
            ('"', _) => self.quoted('"', TokenKind::QuotedIdentifier),
            ('`', _) if self.rules.backtick_identifiers => {
                self.quoted('`', TokenKind::QuotedIdentifier)
            }
            ('[', _) if self.rules.bracket_identifiers => self.bracketed(),
            ('$', _) if self.rules.dollar_quoting => self.dollar(),
            (c, _) if c.is_ascii_digit() => self.number(),
            ('.', Some(c)) if c.is_ascii_digit() => self.number(),
            ('@' | '#', Some(next)) if self.rules.sigil_identifiers && is_sigil_word_part(next) => {
                self.word()
            }
            (c, _) if is_word_start(c) => self.word(),
            (';', _) => {
                self.bump();
                TokenKind::Semicolon
            }
            _ => {
                self.bump();
                TokenKind::Punctuation
            }
        }
    }

    /// `--` is a comment, except in a dialect where it needs whitespace (or the end of the
    /// text) after it: there `1--1` is one minus minus one.
    fn dashes_start_comment(&self) -> bool {
        !self.rules.dash_comments_need_space
            || self.rest()[2..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    }

    fn word(&mut self) -> TokenKind {
        let rules = self.rules;
        self.bump();
        self.bump_while(|c| is_word_part(c, rules));
        TokenKind::Word
    }

    /// `[name]`, where `]]` stands for `]`. Only SQL Server reads it so, but SQLite has no
    /// way to write `]` in a bracketed name at all, so no valid SQLite reads differently.
    fn bracketed(&mut self) -> TokenKind {
        self.bump();
        while let Some(c) = self.bump() {
            if c == ']' {
                if self.peek() == Some(']') {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        TokenKind::QuotedIdentifier
    }

    fn line_comment(&mut self) -> TokenKind {
        self.bump_while(|c| c != '\n');
        TokenKind::LineComment
    }

    /// `/* … */`, nested or ending at the first `*/` as the dialect has it.
    fn block_comment(&mut self) -> TokenKind {
        self.position += 2;
        let mut depth = 1;
        while depth > 0 {
            if self.rest().starts_with("*/") {
                self.position += 2;
                depth -= 1;
            } else if self.rules.nested_block_comments && self.rest().starts_with("/*") {
                self.position += 2;
                depth += 1;
            } else if self.bump().is_none() {
                break;
            }
        }
        TokenKind::BlockComment
    }

    /// Text between `quote`s, where a doubled quote stands for itself (`'it''s'`), and, in a
    /// string of a dialect with backslash escapes, `\` escapes the next character.
    fn quoted(&mut self, quote: char, kind: TokenKind) -> TokenKind {
        let backslash_escapes = kind == TokenKind::String && self.rules.backslash_escapes;
        self.bump();
        while let Some(c) = self.bump() {
            if c == '\\' && backslash_escapes {
                self.bump();
            } else if c == quote {
                if self.peek() == Some(quote) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        kind
    }

    /// PostgreSQL `E'…'`, where a backslash escapes the next character, quotes included.
    fn escape_string(&mut self) -> TokenKind {
        self.bump();
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    self.bump();
                }
                '\'' if self.peek() == Some('\'') => {
                    self.bump();
                }
                '\'' => break,
                _ => {}
            }
        }
        TokenKind::String
    }

    /// `$tag$ … $tag$` (dollar quoting) or `$1` (a parameter marker).
    fn dollar(&mut self) -> TokenKind {
        let tag_length = self.rest()[1..]
            .find(|c: char| !is_plain_word_part(c))
            .unwrap_or(self.rest().len() - 1);
        let tag = &self.rest()[..tag_length + 1];
        let opens_quote = self.rest()[tag_length + 1..].starts_with('$')
            && !tag[1..].starts_with(|c: char| c.is_ascii_digit());
        if !opens_quote {
            self.bump();
            return TokenKind::Punctuation;
        }
        let delimiter = format!("{tag}$");
        self.position += delimiter.len();
        match self.rest().find(&delimiter) {
            Some(end) => self.position += end + delimiter.len(),
            None => self.position = self.sql.len(),
        }
        TokenKind::String
    }

    fn number(&mut self) -> TokenKind {
        self.bump();
        self.bump_while(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
        TokenKind::Number
    }
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_word_part(c: char, rules: LexicalRules) -> bool {
    is_plain_word_part(c)
        || (c == '$' && rules.dollar_in_identifiers)
        || (matches!(c, '@' | '#') && rules.sigil_identifiers)
}

/// What may follow a Transact-SQL `@` or `#` for the two to be one name: `@@rowcount`,
/// `##global`, `@x`. Anything else leaves the sigil a character of its own.
fn is_sigil_word_part(c: char) -> bool {
    is_word_start(c) || matches!(c, '@' | '#')
}

/// A character of an identifier in every dialect; also what a `$tag$` is made of.
fn is_plain_word_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str, dialect: Dialect) -> Vec<(TokenKind, &str)> {
        tokenize(sql, dialect)
            .into_iter()
            .filter(|token| token.kind != TokenKind::Whitespace)
            .map(|token| (token.kind, &sql[token.range]))
            .collect()
    }

    #[test]
    fn covers_the_input_without_gaps() {
        let sql = "SELECT 'a;b', \"x\"\"y\" -- c\n/* d */ FROM t WHERE é = $1; $$ body; $$";
        let tokens = tokenize(sql, Dialect::Postgres);

        let mut expected_start = 0;
        for token in &tokens {
            assert_eq!(token.range.start, expected_start);
            expected_start = token.range.end;
        }
        assert_eq!(expected_start, sql.len());
    }

    /// Input, dialect, and the expected non-whitespace tokens.
    type Case = (&'static str, Dialect, Vec<(TokenKind, &'static str)>);

    #[test]
    fn recognises_dialect_specific_quoting() {
        use TokenKind::*;
        let cases: [Case; 13] = [
            (
                "$fn$ a; $x$ b $fn$ ;",
                Dialect::Postgres,
                vec![(String, "$fn$ a; $x$ b $fn$"), (Semicolon, ";")],
            ),
            (
                "$1 + $2",
                Dialect::Postgres,
                vec![
                    (Punctuation, "$"),
                    (Number, "1"),
                    (Punctuation, "+"),
                    (Punctuation, "$"),
                    (Number, "2"),
                ],
            ),
            (
                r"E'it\'s' x",
                Dialect::Postgres,
                vec![(String, r"E'it\'s'"), (Word, "x")],
            ),
            (
                "/* a /* b */ c */ d",
                Dialect::Postgres,
                vec![(BlockComment, "/* a /* b */ c */"), (Word, "d")],
            ),
            (
                "[my col] `x` ;",
                Dialect::Sqlite,
                vec![
                    (QuotedIdentifier, "[my col]"),
                    (QuotedIdentifier, "`x`"),
                    (Semicolon, ";"),
                ],
            ),
            (
                "'unterminated; string",
                Dialect::Sqlite,
                vec![(String, "'unterminated; string")],
            ),
            (
                r#"select 'it\'s', "a \"b\"", `my``col`, a$b"#,
                Dialect::Mysql,
                vec![
                    (Word, "select"),
                    (String, r"'it\'s'"),
                    (Punctuation, ","),
                    (String, r#""a \"b\"""#),
                    (Punctuation, ","),
                    (QuotedIdentifier, "`my``col`"),
                    (Punctuation, ","),
                    (Word, "a$b"),
                ],
            ),
            (
                "# note\nselect 1--1, 2 -- end\n",
                Dialect::Mysql,
                vec![
                    (LineComment, "# note"),
                    (Word, "select"),
                    (Number, "1"),
                    (Punctuation, "-"),
                    (Punctuation, "-"),
                    (Number, "1"),
                    (Punctuation, ","),
                    (Number, "2"),
                    (LineComment, "-- end"),
                ],
            ),
            (
                r"'C:\\' x",
                Dialect::Mysql,
                vec![(String, r"'C:\\'"), (Word, "x")],
            ),
            (
                "select [a]]b], N'it''s', @x, @@rowcount, #tmp, ##g, a$b#c from \"t\"",
                Dialect::SqlServer,
                vec![
                    (Word, "select"),
                    (QuotedIdentifier, "[a]]b]"),
                    (Punctuation, ","),
                    (String, "N'it''s'"),
                    (Punctuation, ","),
                    (Word, "@x"),
                    (Punctuation, ","),
                    (Word, "@@rowcount"),
                    (Punctuation, ","),
                    (Word, "#tmp"),
                    (Punctuation, ","),
                    (Word, "##g"),
                    (Punctuation, ","),
                    (Word, "a$b#c"),
                    (Word, "from"),
                    (QuotedIdentifier, "\"t\""),
                ],
            ),
            (
                "/* a /* b */ c */ x @ 1, n'x'",
                Dialect::SqlServer,
                vec![
                    (BlockComment, "/* a /* b */ c */"),
                    (Word, "x"),
                    (Punctuation, "@"),
                    (Number, "1"),
                    (Punctuation, ","),
                    (String, "n'x'"),
                ],
            ),
            (
                r#"select `a b`, "c", 'it\'s', $$x;y$$ # note"#,
                Dialect::ClickHouse,
                vec![
                    (Word, "select"),
                    (QuotedIdentifier, "`a b`"),
                    (Punctuation, ","),
                    (QuotedIdentifier, "\"c\""),
                    (Punctuation, ","),
                    (String, r"'it\'s'"),
                    (Punctuation, ","),
                    (String, "$$x;y$$"),
                    (LineComment, "# note"),
                ],
            ),
            // Only Transact-SQL has `@` names; elsewhere `@p` is a parameter marker's sign.
            (
                "@p N'x'",
                Dialect::Sqlite,
                vec![
                    (Punctuation, "@"),
                    (Word, "p"),
                    (Word, "N"),
                    (String, "'x'"),
                ],
            ),
        ];

        for (sql, dialect, expected) in cases {
            assert_eq!(kinds(sql, dialect), expected, "{sql}");
        }
    }

    /// CQL's `//` comments, `$$` strings and `''` quotes, with no backslash escapes; `//` is two
    /// divisions elsewhere.
    #[test]
    fn reads_cql_comments_and_strings() {
        use TokenKind::*;
        assert_eq!(
            kinds("a // b\n$$ c; $$ 'it''s \\' /* d */", Dialect::Cassandra),
            [
                (Word, "a"),
                (LineComment, "// b"),
                (String, "$$ c; $$"),
                (String, "'it''s \\'"),
                (BlockComment, "/* d */"),
            ]
        );
        assert_eq!(
            kinds("a // b", Dialect::Postgres),
            [
                (Word, "a"),
                (Punctuation, "/"),
                (Punctuation, "/"),
                (Word, "b")
            ]
        );
    }

    /// What DbRover generates must read back as one token in its own dialect: a quoted name
    /// as one identifier, a literal as one string, whatever quotes and backslashes they hold.
    #[test]
    fn generated_quoting_reads_back_as_one_token() {
        let awkward = r#"it's "a" `b` \ c"#;
        for dialect in Dialect::ALL {
            let quoting = dialect.quoting();
            let cases = [
                (quoting.identifier(awkward), TokenKind::QuotedIdentifier),
                (quoting.string_literal(awkward), TokenKind::String),
            ];
            for (sql, kind) in cases {
                assert_eq!(kinds(&sql, dialect), [(kind, sql.as_str())], "{dialect:?}");
            }
        }
    }
}
