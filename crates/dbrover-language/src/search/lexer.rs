//! A console's tokens, a line at a time: a request line is a method, a path cut at `/`, `?`,
//! `&`, `=` and `,`, and anything after the path; every other line is JSON. Every byte of the
//! text is in exactly one token, so the editor highlights from them and the splitter and parser
//! read the same ones.

use dbrover_sql::{Token, TokenKind};

use super::Method;

/// Splits `text` into tokens covering every byte.
///
/// A string that isn't closed by the end of its line is its opening quote, as
/// [`TokenKind::Punctuation`], and the rest of the line as one [`TokenKind::Word`]. That is how
/// the text before the cursor ends while a key is being typed, so completion sees a word to
/// complete there, and a closed string is highlighted as a string.
pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        line_tokens(line, start, &mut tokens);
        start += line.len();
    }
    tokens
}

/// What starts a comment line, as Kibana's console reads them.
const COMMENT_STARTS: [&str; 2] = ["#", "//"];

fn line_tokens(line: &str, offset: usize, tokens: &mut Vec<Token>) {
    let mut push = |kind, range: std::ops::Range<usize>| {
        if !range.is_empty() {
            tokens.push(Token {
                kind,
                range: offset + range.start..offset + range.end,
            });
        }
    };
    let indent = line.len() - line.trim_start().len();
    push(TokenKind::Whitespace, 0..indent);
    let rest = &line[indent..];
    let content = rest.trim_end_matches(['\n', '\r']);
    let newline = indent + content.len()..line.len();
    if COMMENT_STARTS
        .iter()
        .any(|start| content.starts_with(start))
    {
        push(TokenKind::LineComment, indent..indent + content.len());
        push(TokenKind::Whitespace, newline);
        return;
    }
    let mut at = indent;
    if let Some(method) = method_word(content) {
        push(TokenKind::Word, at..at + method);
        at += method;
        let spaces = space_length(&line[at..]);
        push(TokenKind::Whitespace, at..at + spaces);
        at += spaces;
        at = path_tokens(line, at, &mut push);
    }
    json_tokens(line, at, &mut push);
}

/// The length of the method `line` starts with, when it is a request line: a method word
/// followed by whitespace or the end of the line.
fn method_word(line: &str) -> Option<usize> {
    let length = line.find(char::is_whitespace).unwrap_or(line.len());
    Method::from_word(&line[..length]).map(|_| length)
}

fn space_length(text: &str) -> usize {
    text.find(|c: char| !c.is_whitespace() || c == '\n')
        .unwrap_or(text.len())
}

/// Characters that separate a path's parts and its query string's: each is a token of its own.
const PATH_PUNCTUATION: &[char] = &['/', '?', '&', '=', ','];

/// The path's tokens, from `at` to the first whitespace; returns where they end.
fn path_tokens(
    line: &str,
    mut at: usize,
    push: &mut impl FnMut(TokenKind, std::ops::Range<usize>),
) -> usize {
    let end = at
        + line[at..]
            .find(char::is_whitespace)
            .unwrap_or(line.len() - at);
    while at < end {
        let rest = &line[at..end];
        let length = if rest.starts_with(PATH_PUNCTUATION) {
            1
        } else {
            rest.find(PATH_PUNCTUATION).unwrap_or(rest.len())
        };
        let kind = if length == 1 && rest.starts_with(PATH_PUNCTUATION) {
            TokenKind::Punctuation
        } else {
            TokenKind::Word
        };
        push(kind, at..at + length);
        at += length;
    }
    end
}

/// JSON's tokens, from `at` to the end of the line.
fn json_tokens(
    line: &str,
    mut at: usize,
    push: &mut impl FnMut(TokenKind, std::ops::Range<usize>),
) {
    while let Some(first) = line[at..].chars().next() {
        let rest = &line[at..];
        let length = match first {
            c if c.is_whitespace() => {
                let length = rest
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(rest.len());
                push(TokenKind::Whitespace, at..at + length);
                length
            }
            '"' => match closed_string_length(rest) {
                Some(length) => {
                    push(TokenKind::String, at..at + length);
                    length
                }
                None => {
                    let length = rest.trim_end_matches(['\n', '\r']).len();
                    push(TokenKind::Punctuation, at..at + 1);
                    push(TokenKind::Word, at + 1..at + length);
                    length
                }
            },
            '-' | '0'..='9' => {
                let length = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
                    .unwrap_or(rest.len());
                push(TokenKind::Number, at..at + length);
                length
            }
            c if c.is_alphanumeric() || c == '_' => {
                let length = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                push(TokenKind::Word, at..at + length);
                length
            }
            c => {
                push(TokenKind::Punctuation, at..at + c.len_utf8());
                c.len_utf8()
            }
        };
        at += length;
    }
}

/// The length of the string `text` starts with, its quotes included, when it closes on this
/// line.
fn closed_string_length(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, c) in text.char_indices().skip(1) {
        match c {
            '\n' => return None,
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(index + 1),
            _ => {}
        }
    }
    None
}
