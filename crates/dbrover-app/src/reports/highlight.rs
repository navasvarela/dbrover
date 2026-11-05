//! Highlighting a report's file (`docs/report-editor.md`, RP8d): its TOML, and each step's
//! code in the step's own language, a source's query in its data source's.

use std::ops::Range;

use dbrover_core::DataSource;
use dbrover_language::QueryLanguage;
use dbrover_report::{PrqlToken, StepLanguage, embedded_code, prql_tokens};
use dbrover_sql::Dialect;
use toml_parser::Source;
use toml_parser::lexer::{Token, TokenKind};

use crate::code_editor::highlight::{Highlighter, Span, SyntaxRole};

/// Highlights a report file against the data sources its sources name.
pub struct ReportHighlighter<'a> {
    pub data_sources: &'a [DataSource],
}

impl Highlighter for ReportHighlighter<'_> {
    fn spans(&self, text: &str) -> Vec<Span> {
        let mut spans = toml_spans(text);
        for embedded in embedded_code(text) {
            let code = &text[embedded.range.clone()];
            let inner = match &embedded.language {
                StepLanguage::Source { data_source } => {
                    let Some(language) = language_of(data_source, self.data_sources) else {
                        continue;
                    };
                    language.spans(code)
                }
                StepLanguage::DuckDb => QueryLanguage::Sql(Dialect::DuckDb).spans(code),
                StepLanguage::Prql => prql_spans(code),
            };
            splice(&mut spans, &embedded.range, inner);
        }
        spans
    }
}

/// The language of the data source a source names, by name or by id, as the plan finds it.
fn language_of(wanted: &str, data_sources: &[DataSource]) -> Option<QueryLanguage> {
    data_sources
        .iter()
        .find(|data_source| data_source.name == wanted)
        .or_else(|| {
            data_sources
                .iter()
                .find(|data_source| data_source.id.to_string() == wanted)
        })
        .map(|data_source| QueryLanguage::from(data_source.config.driver()))
}

/// `spans` with `range` given over to `inner`, whose ranges count from `range`'s start. The
/// string around the code keeps its colour up to the code and after it.
fn splice(spans: &mut Vec<Span>, range: &Range<usize>, inner: Vec<Span>) {
    let mut kept = Vec::with_capacity(spans.len() + inner.len());
    for span in spans.drain(..) {
        if span.range.end <= range.start || span.range.start >= range.end {
            kept.push(span);
            continue;
        }
        if span.range.start < range.start {
            kept.push(Span {
                range: span.range.start..range.start,
                role: span.role,
            });
        }
        if span.range.end > range.end {
            kept.push(Span {
                range: range.end..span.range.end,
                role: span.role,
            });
        }
    }
    kept.extend(inner.into_iter().map(|span| Span {
        range: span.range.start + range.start..span.range.end + range.start,
        role: span.role,
    }));
    kept.sort_by_key(|span| span.range.start);
    *spans = kept;
}

fn prql_spans(prql: &str) -> Vec<Span> {
    prql_tokens(prql)
        .into_iter()
        .map(|(range, token)| Span {
            range,
            role: match token {
                PrqlToken::Keyword => SyntaxRole::Keyword,
                PrqlToken::String => SyntaxRole::String,
                PrqlToken::Literal => SyntaxRole::Number,
                PrqlToken::Comment => SyntaxRole::Comment,
                PrqlToken::Other => SyntaxRole::Plain,
            },
        })
        .collect()
}

/// The TOML's tokens as spans: comments, table headers as keywords, keys as names, strings,
/// and numbers, dates and booleans as numbers.
fn toml_spans(text: &str) -> Vec<Span> {
    let tokens = Source::new(text).lex().into_vec();
    let mut reader = TomlReader::default();
    tokens
        .iter()
        .enumerate()
        .filter_map(|(index, token)| {
            let role = reader.role(token, &tokens[index + 1..], text)?;
            Some(Span {
                range: token.span().start()..token.span().end(),
                role,
            })
        })
        .collect()
}

/// Where the lexer is in the TOML's structure, which is what tells a key from a value.
#[derive(Default)]
struct TomlReader {
    /// Nothing but whitespace has come since the line began.
    line_start: bool,
    /// Inside a `[table]` or `[[table]]` header.
    in_header: bool,
    /// The brackets of arrays and inline tables open around the token.
    open: Vec<TokenKind>,
    /// The next name is a key.
    expects_key: bool,
    started: bool,
}

impl TomlReader {
    /// The role of `token`, followed by `rest`; `None` for what draws plain.
    fn role(&mut self, token: &Token, rest: &[Token], text: &str) -> Option<SyntaxRole> {
        if !self.started {
            self.started = true;
            self.line_start = true;
            self.expects_key = true;
        }
        let kind = token.kind();
        let starts_line = self.line_start;
        if !matches!(kind, TokenKind::Whitespace) {
            self.line_start = false;
        }
        match kind {
            TokenKind::Newline => {
                self.line_start = true;
                self.in_header = false;
                self.expects_key = self.open.is_empty();
                None
            }
            TokenKind::Whitespace | TokenKind::Eof => None,
            TokenKind::Comment => Some(SyntaxRole::Comment),
            TokenKind::LeftSquareBracket if starts_line && self.open.is_empty() => {
                self.in_header = true;
                Some(SyntaxRole::Keyword)
            }
            _ if self.in_header => Some(SyntaxRole::Keyword),
            TokenKind::LeftSquareBracket | TokenKind::LeftCurlyBracket => {
                self.open.push(kind);
                self.expects_key = kind == TokenKind::LeftCurlyBracket;
                None
            }
            TokenKind::RightSquareBracket | TokenKind::RightCurlyBracket => {
                self.open.pop();
                self.expects_key = false;
                None
            }
            TokenKind::Comma => {
                self.expects_key = self.open.last() == Some(&TokenKind::LeftCurlyBracket);
                None
            }
            TokenKind::Equals => {
                self.expects_key = false;
                None
            }
            TokenKind::Dot => None,
            TokenKind::Atom | TokenKind::BasicString | TokenKind::LiteralString
                if self.expects_key && is_key(rest) =>
            {
                Some(SyntaxRole::Identifier)
            }
            TokenKind::BasicString
            | TokenKind::LiteralString
            | TokenKind::MlBasicString
            | TokenKind::MlLiteralString => Some(SyntaxRole::String),
            TokenKind::Atom => {
                let atom = &text[token.span().start()..token.span().end()];
                Some(atom_role(atom))
            }
        }
    }
}

/// Whether the name before `rest` is a key: what follows it on the line is a `.` or a `=`.
fn is_key(rest: &[Token]) -> bool {
    rest.iter()
        .map(Token::kind)
        .find(|kind| *kind != TokenKind::Whitespace)
        .is_some_and(|kind| matches!(kind, TokenKind::Equals | TokenKind::Dot))
}

/// A bare value: a number, a date or time, `true`, `false`, `inf` or `nan` is drawn as a
/// number; anything else (a mistake the reader reports) plain.
fn atom_role(atom: &str) -> SyntaxRole {
    let literal = matches!(atom, "true" | "false" | "inf" | "nan" | "+inf" | "-inf")
        || atom.starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '-');
    if literal {
        SyntaxRole::Number
    } else {
        SyntaxRole::Plain
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dbrover_core::{ConnectionConfig, DataSourceId, SafetyLevel, SqliteConfig};

    use super::*;

    fn sqlite(name: &str) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: PathBuf::from("/tmp/shop.db"),
                read_only: true,
            }),
        }
    }

    /// Each span's text and role, leaving out what draws plain.
    fn roles(text: &str, data_sources: &[DataSource]) -> Vec<(String, SyntaxRole)> {
        ReportHighlighter { data_sources }
            .spans(text)
            .into_iter()
            .filter(|span| span.role != SyntaxRole::Plain)
            .map(|span| (text[span.range].to_owned(), span.role))
            .collect()
    }

    #[test]
    fn colours_the_toml() {
        let text = "# Sales\n[report]\nname = \"S\"\nparams = { since = \"date\" }\n\
                    [[output]]\nmax_rows = 10\nbom = true\ncolumns = [\n  { name = 'a' },\n]\n";
        let key = SyntaxRole::Identifier;
        let string = SyntaxRole::String;
        let expected = [
            ("# Sales", SyntaxRole::Comment),
            ("[", SyntaxRole::Keyword),
            ("report", SyntaxRole::Keyword),
            ("]", SyntaxRole::Keyword),
            ("name", key),
            ("\"S\"", string),
            ("params", key),
            ("since", key),
            ("\"date\"", string),
            ("[", SyntaxRole::Keyword),
            ("[", SyntaxRole::Keyword),
            ("output", SyntaxRole::Keyword),
            ("]", SyntaxRole::Keyword),
            ("]", SyntaxRole::Keyword),
            ("max_rows", key),
            ("10", SyntaxRole::Number),
            ("bom", key),
            ("true", SyntaxRole::Number),
            ("columns", key),
            ("name", key),
            ("'a'", string),
        ];
        let found = roles(text, &[]);
        let found: Vec<(&str, SyntaxRole)> = found
            .iter()
            .map(|(fragment, role)| (fragment.as_str(), *role))
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn colours_each_step_in_its_own_language() {
        let text = "[[source]]\nid = \"a\"\ndata_source = \"shop\"\nquery = \"\"\"\nselect 1\n\"\"\"\n\
                    [[source]]\nid = \"b\"\ndata_source = \"unknown\"\nquery = 'select 2'\n\
                    [combine]\nprql = 'from a'\n";
        let found = roles(text, &[sqlite("shop")]);
        let has = |fragment: &str, role: SyntaxRole| {
            found
                .iter()
                .any(|(text, found_role)| text == fragment && *found_role == role)
        };
        assert!(has("select", SyntaxRole::Keyword), "{found:?}");
        assert!(has("1", SyntaxRole::Number), "{found:?}");
        assert!(
            has("\"\"\"\n", SyntaxRole::String),
            "the opening quotes: {found:?}"
        );
        // An unknown data source's query stays a string.
        assert!(has("'select 2'", SyntaxRole::String), "{found:?}");
        assert!(has("from", SyntaxRole::Keyword), "{found:?}");
        assert!(has("'", SyntaxRole::String), "PRQL's quotes: {found:?}");
    }
}
