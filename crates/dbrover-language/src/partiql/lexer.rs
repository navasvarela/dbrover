//! PartiQL's tokens, as DynamoDB reads them: names, `"quoted names"`, `'strings'`, numbers,
//! operators and comments. Every byte of the text is in exactly one token, so the editor can
//! highlight from them and the splitter and parser read the same ones.

use dbrover_sql::{Token, TokenKind};

/// The operators of two characters, each one token so that `<=` is never read as `<` then
/// `=`. `<<` and `>>` open and close a set.
const TWO_CHARACTER_OPERATORS: [&str; 7] = ["<=", ">=", "<>", "!=", "<<", ">>", "||"];

/// Splits `text` into tokens covering every byte. A string or quoted name that isn't closed
/// ends at the end of the text, and so does a block comment.
pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = 0;
    while let Some(first) = text[start..].chars().next() {
        let rest = &text[start..];
        let (kind, length) = match first {
            c if c.is_whitespace() => (
                TokenKind::Whitespace,
                rest.find(|c: char| !c.is_whitespace())
                    .unwrap_or(rest.len()),
            ),
            '-' if rest.starts_with("--") => (
                TokenKind::LineComment,
                rest.find('\n').unwrap_or(rest.len()),
            ),
            '/' if rest.starts_with("/*") => (
                TokenKind::BlockComment,
                rest[2..].find("*/").map_or(rest.len(), |end| end + 4),
            ),
            '\'' => (TokenKind::String, quoted_length(rest, '\'')),
            '"' => (TokenKind::QuotedIdentifier, quoted_length(rest, '"')),
            ';' => (TokenKind::Semicolon, 1),
            c if c.is_ascii_digit() => (TokenKind::Number, number_length(rest)),
            '.' if rest[1..].starts_with(|c: char| c.is_ascii_digit()) => {
                (TokenKind::Number, number_length(rest))
            }
            c if is_name_start(c) => (
                TokenKind::Word,
                rest.find(|c: char| !is_name_part(c)).unwrap_or(rest.len()),
            ),
            c => (TokenKind::Punctuation, operator_length(rest, c)),
        };
        tokens.push(Token {
            kind,
            range: start..start + length,
        });
        start += length;
    }
    tokens
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_name_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn operator_length(text: &str, first: char) -> usize {
    match TWO_CHARACTER_OPERATORS
        .iter()
        .find(|operator| text.starts_with(**operator))
    {
        Some(operator) => operator.len(),
        None => first.len_utf8(),
    }
}

/// The length of the string or name `text` starts with: up to its closing `quote`, which is
/// written twice to stand for itself (`'it''s'`), or the end of the text.
fn quoted_length(text: &str, quote: char) -> usize {
    let mut characters = text.char_indices().skip(1).peekable();
    while let Some((index, c)) = characters.next() {
        if c != quote {
            continue;
        }
        if characters.next_if(|(_, next)| *next == quote).is_none() {
            return index + 1;
        }
    }
    text.len()
}

/// The length of the number `text` starts with, with an optional fraction and exponent (`1`,
/// `.5`, `2.5e-3`). Letters and digits run on after it are taken too, so `12abc` is one token
/// the database can refuse whole.
fn number_length(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
        index += 1;
    }
    if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
        index += 1;
        if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
            index += 1;
        }
    }
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') {
        index += 1;
    }
    index
}
