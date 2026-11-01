//! Syntax errors, from `sqlparser` with the dialect's own parser dialect.
//!
//! `sqlparser` reads most of each dialect's queries, but not all: the constructs it rejects
//! although the database accepts them are the dialect's [`ParserGap`]s, and a statement using
//! one is not checked.

use sqlparser::parser::Parser;

use super::{Problem, ProblemSource, Severity, is_checkable, token_at};
use crate::dialect::{Dialect, ParserGap};
use crate::lexer::{Token, TokenKind, tokenize};

/// What may follow `IS` (or `IS NOT`) for `sqlparser`; SQLite also accepts any expression
/// there (`a IS b`).
const AFTER_IS: &[&str] = &[
    "null", "true", "false", "distinct", "unknown", "json", "not",
];

/// The statement's syntax error, if it is a checked kind of statement and the parser rejects
/// it. None in CQL, which `sqlparser` can't read: its server checks it (`Connection::validate`).
pub(super) fn problem(statement: &str, dialect: Dialect) -> Option<Problem> {
    let parser = dialect.sqlparser_dialect()?;
    if !is_checkable(statement, dialect) || has_known_gap(statement, dialect) {
        return None;
    }
    let parsed = Parser::parse_sql(parser, statement);
    let error = parsed.err()?.to_string();
    let error = error.strip_prefix("sql parser error: ").unwrap_or(&error);
    let (message, location) = match error.rsplit_once(" at Line: ") {
        Some((message, location)) => (message, parse_location(location)),
        None => (error, None),
    };
    let offset = location
        .and_then(|(line, column)| byte_offset(statement, line, column))
        .unwrap_or(statement.len());
    Some(Problem {
        range: token_at(statement, dialect, offset),
        severity: Severity::Error,
        message: describe(message),
        source: ProblemSource::Syntax,
    })
}

/// Reads `3, Column: 14` into a 1-based line and column.
fn parse_location(location: &str) -> Option<(usize, usize)> {
    let (line, column) = location.split_once(", Column: ")?;
    Some((line.trim().parse().ok()?, column.trim().parse().ok()?))
}

/// The byte offset of a 1-based line and column (counted in characters, as `sqlparser` does).
fn byte_offset(text: &str, line: usize, column: usize) -> Option<usize> {
    let line_start = if line <= 1 {
        0
    } else {
        text.match_indices('\n').nth(line - 2)?.0 + 1
    };
    let rest = &text[line_start..];
    let within = rest
        .char_indices()
        .nth(column.checked_sub(1)?)
        .map_or(rest.len(), |(byte, _)| byte);
    Some(line_start + within)
}

/// `Expected: an expression, found: =` reads as `Syntax error: expected an expression, found
/// =`; the end of the text is called that rather than `EOF`.
fn describe(message: &str) -> String {
    let message = message
        .replace("Expected: ", "expected ")
        .replace(", found: EOF", " at the end of the statement")
        .replace(", found: ", ", found ");
    format!("Syntax error: {message}")
}

fn has_known_gap(statement: &str, dialect: Dialect) -> bool {
    let tokens = Tokens::new(statement, dialect);
    (0..tokens.len()).any(|index| {
        dialect
            .parser_gaps()
            .iter()
            .any(|gap| tokens.has_gap_at(index, *gap))
    })
}

/// A statement's meaningful tokens, read by position.
struct Tokens<'a> {
    statement: &'a str,
    tokens: Vec<Token>,
}

impl<'a> Tokens<'a> {
    fn new(statement: &'a str, dialect: Dialect) -> Self {
        let tokens = tokenize(statement, dialect)
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .collect();
        Self { statement, tokens }
    }

    fn len(&self) -> usize {
        self.tokens.len()
    }

    /// The token at `index`, lowercased, if it is a word.
    fn word(&self, index: usize) -> Option<String> {
        self.tokens
            .get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| self.statement[token.range.clone()].to_ascii_lowercase())
    }

    fn text(&self, index: usize) -> Option<&'a str> {
        self.tokens
            .get(index)
            .map(|token| &self.statement[token.range.clone()])
    }

    /// Whether the tokens from `index` on are written together, with nothing between them,
    /// and spell `text`.
    fn spells_at(&self, index: usize, text: &str) -> bool {
        let Some(first) = self.tokens.get(index) else {
            return false;
        };
        let start = first.range.start;
        let end = start + text.len();
        let mut reached = start;
        for token in &self.tokens[index..] {
            if token.range.start >= end {
                break;
            }
            if token.range.start != reached {
                return false;
            }
            reached = token.range.end;
        }
        reached == end && self.statement.get(start..end) == Some(text)
    }

    /// Whether the construct `gap` starts at token `index`.
    fn has_gap_at(&self, index: usize, gap: ParserGap) -> bool {
        let current = self.word(index);
        match gap {
            ParserGap::Words(words) => words
                .iter()
                .enumerate()
                .all(|(offset, expected)| self.word(index + offset).as_deref() == Some(expected)),
            ParserGap::XmlFunctionArguments => {
                current
                    .as_deref()
                    .is_some_and(|word| word.starts_with("xml"))
                    && self.text(index + 1) == Some("(")
            }
            ParserGap::JoinUsingAlias => {
                current.as_deref() == Some("using")
                    && self.text(index + 1) == Some("(")
                    && (index + 2..self.len())
                        .find(|&close| self.text(close) == Some(")"))
                        .is_some_and(|close| self.word(close + 1).as_deref() == Some("as"))
            }
            ParserGap::Operator(operator) => self.spells_at(index, operator),
            ParserGap::ElementByPosition => {
                self.text(index).is_some_and(|text| text.starts_with('.'))
                    && self.tokens[index].kind == TokenKind::Number
                    && index
                        .checked_sub(1)
                        .and_then(|before| self.tokens.get(before))
                        .is_some_and(|before| before.range.end == self.tokens[index].range.start)
            }
            ParserGap::WithExpressionAlias => {
                let names_a_query = self.word(index + 1).is_some()
                    && self.word(index + 2).as_deref() == Some("as")
                    && self.text(index + 3) == Some("(");
                current.as_deref() == Some("with")
                    && !names_a_query
                    && self.word(index + 1).as_deref() != Some("recursive")
            }
            ParserGap::InfixOperator(operator) => {
                current.as_deref() == Some(operator) && self.text(index + 1) != Some("(")
            }
            ParserGap::IsAnyExpression => {
                current.as_deref() == Some("is") && {
                    let next = if self.word(index + 1).as_deref() == Some("not") {
                        index + 2
                    } else {
                        index + 1
                    };
                    !self
                        .word(next)
                        .is_some_and(|word| AFTER_IS.contains(&word.as_str()))
                }
            }
        }
    }
}
