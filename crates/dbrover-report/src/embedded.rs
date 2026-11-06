//! The code inside a report file (`docs/report-editor.md`, RP8d): where each step's query sits
//! in the TOML and the language it is in, so an editor can highlight it in that language, and
//! PRQL's tokens, which no other part of DbRover lexes.

use std::ops::Range;

use toml::de::{DeTable, DeValue};

/// The language a step's code is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepLanguage {
    /// A source's query, in its data source's language, named as the file names it.
    Source { data_source: String },
    /// `[combine]`'s query: DuckDB's SQL.
    DuckDb,
    /// `[combine]`'s `prql`.
    Prql,
}

/// A step's code: the bytes of the file between its string's quotes, and its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embedded {
    pub range: Range<usize>,
    /// The whole string, its quotes too.
    pub string: Range<usize>,
    pub language: StepLanguage,
}

/// Each step's code in `text`, in the file's order. A string with escapes is left out, since
/// its bytes in the file aren't its code's; so is what doesn't parse.
pub fn embedded_code(text: &str) -> Vec<Embedded> {
    let (file, _) = DeTable::parse_recoverable(text);
    let file = file.get_ref();
    let mut found = Vec::new();
    for source in tables(file, "source") {
        let Some(DeValue::String(data_source)) = value(source, "data_source") else {
            continue;
        };
        let language = StepLanguage::Source {
            data_source: data_source.to_string(),
        };
        found.extend(code(text, source, "query", language));
    }
    if let Some(combine) = tables(file, "combine").next() {
        found.extend(code(text, combine, "query", StepLanguage::DuckDb));
        found.extend(code(text, combine, "prql", StepLanguage::Prql));
    }
    for chart in tables(file, "chart") {
        found.extend(code(text, chart, "query", StepLanguage::DuckDb));
    }
    found.sort_by_key(|embedded| embedded.range.start);
    found
}

/// The tables under `key`: one for `[key]`, each of `[[key]]`'s.
fn tables<'a, 'i>(file: &'a DeTable<'i>, key: &str) -> impl Iterator<Item = &'a DeTable<'i>> {
    let value = file
        .iter()
        .find(|(name, _)| name.get_ref().as_ref() == key)
        .map(|(_, value)| value.get_ref());
    let items: Vec<&'a DeTable<'i>> = match value {
        Some(DeValue::Table(table)) => vec![table],
        Some(DeValue::Array(items)) => items
            .iter()
            .filter_map(|item| match item.get_ref() {
                DeValue::Table(table) => Some(table),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    items.into_iter()
}

fn value<'a, 'i>(table: &'a DeTable<'i>, key: &str) -> Option<&'a DeValue<'i>> {
    table
        .iter()
        .find(|(name, _)| name.get_ref().as_ref() == key)
        .map(|(_, value)| value.get_ref())
}

/// `key`'s string in `table`, as code in `language`, if its bytes in the file are its text.
fn code(text: &str, table: &DeTable<'_>, key: &str, language: StepLanguage) -> Option<Embedded> {
    let (_, value) = table
        .iter()
        .find(|(name, _)| name.get_ref().as_ref() == key)?;
    if !matches!(value.get_ref(), DeValue::String(_)) {
        return None;
    }
    let string = value.span();
    let range = string_content(text, string.clone())?;
    Some(Embedded {
        range,
        string,
        language,
    })
}

/// The bytes between a string's quotes, leaving out the line break TOML drops after a
/// multi-line string's opening quotes; `None` for a basic string with escapes, whose bytes
/// aren't its value.
fn string_content(text: &str, span: Range<usize>) -> Option<Range<usize>> {
    let raw = text.get(span.clone())?;
    let quotes = ["'''", "\"\"\"", "'", "\""]
        .into_iter()
        .find(|quotes| raw.len() >= 2 * quotes.len() && raw.starts_with(quotes))?;
    if quotes.starts_with('"') && raw.contains('\\') {
        return None;
    }
    let mut start = span.start + quotes.len();
    let end = span.end - quotes.len();
    if quotes.len() == 3 {
        let after = text.get(start..end)?;
        if after.starts_with("\r\n") {
            start += 2;
        } else if after.starts_with('\n') {
            start += 1;
        }
    }
    Some(start..end)
}

/// What a piece of PRQL is, for highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrqlToken {
    /// One of PRQL's keywords, or a transform of its standard library (`from`, `select`, …),
    /// which read as keywords though PRQL defines them as functions.
    Keyword,
    String,
    /// A number, a date or time, `true`, `false` or `null`.
    Literal,
    Comment,
    /// A name, an operator, a bracket.
    Other,
}

/// PRQL's standard transforms, from the PRQL book's "Transforms" chapter (0.13): the words a
/// pipeline is made of.
const TRANSFORMS: [&str; 15] = [
    "aggregate",
    "append",
    "derive",
    "filter",
    "from",
    "from_text",
    "group",
    "intersect",
    "join",
    "loop",
    "remove",
    "select",
    "sort",
    "take",
    "window",
];

/// `prql`'s tokens and what each is. Empty when it doesn't lex, which compiling it reports.
pub fn prql_tokens(prql: &str) -> Vec<(Range<usize>, PrqlToken)> {
    use prqlc::lr::{Literal, TokenKind};

    let Ok(tokens) = prqlc::prql_to_tokens(prql) else {
        return Vec::new();
    };
    tokens
        .0
        .into_iter()
        .filter_map(|token| {
            let kind = match &token.kind {
                TokenKind::Start | TokenKind::NewLine | TokenKind::LineWrap(_) => return None,
                TokenKind::Keyword(_) => PrqlToken::Keyword,
                TokenKind::Ident(name) if TRANSFORMS.contains(&name.as_str()) => PrqlToken::Keyword,
                TokenKind::Literal(Literal::String(_) | Literal::RawString(_))
                | TokenKind::Interpolation(..) => PrqlToken::String,
                TokenKind::Literal(_) => PrqlToken::Literal,
                TokenKind::Comment(_) | TokenKind::DocComment(_) => PrqlToken::Comment,
                _ => PrqlToken::Other,
            };
            Some((token.span, kind))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"[report]
name = "R"

[[source]]
id = "lines"
data_source = "shop"
query = """
SELECT 1
"""

[[source]]
id = "escaped"
data_source = "shop"
query = "SELECT '\t'"

[[source]]
id = "plain"
data_source = "ledger"
query = 'SELECT 2'

[combine]
prql = '''
from lines
select {a}
'''

[[chart]]
file = "a.svg"
query = "SELECT a FROM lines"
"#;

    #[test]
    fn finds_each_steps_code_and_its_language() {
        let found: Vec<(&str, StepLanguage)> = embedded_code(FILE)
            .into_iter()
            .map(|embedded| (&FILE[embedded.range], embedded.language))
            .collect();
        let source = |name: &str| StepLanguage::Source {
            data_source: name.to_owned(),
        };
        assert_eq!(
            found,
            [
                ("SELECT 1\n", source("shop")),
                ("SELECT 2", source("ledger")),
                ("from lines\nselect {a}\n", StepLanguage::Prql),
                ("SELECT a FROM lines", StepLanguage::DuckDb),
            ]
        );
    }

    #[test]
    fn finds_every_step_of_the_example_reports() {
        let examples = [
            include_str!("../../../examples/reports/battery-cycles.toml"),
            include_str!("../../../examples/reports/battery-flows.toml"),
            include_str!("../../../examples/reports/order-history.toml"),
            include_str!("../../../examples/reports/sales-by-category.toml"),
            include_str!("../../../examples/reports/sales-by-week.toml"),
        ];
        for text in examples {
            let steps = text
                .lines()
                .filter(|line| line.starts_with("query = ") || line.starts_with("prql = "))
                .count();
            let found = embedded_code(text);
            assert_eq!(found.len(), steps, "{text}");
            assert!(
                found
                    .iter()
                    .all(|code| !text[code.range.clone()].contains("\"\"\"")),
                "{text}"
            );
        }
    }

    #[test]
    fn tells_prqls_transforms_strings_and_comments() {
        let prql = "from lines # all\nselect {name = \"x\", n = 3}";
        let kinds: Vec<(&str, PrqlToken)> = prql_tokens(prql)
            .into_iter()
            .filter(|(_, kind)| *kind != PrqlToken::Other)
            .map(|(range, kind)| (&prql[range], kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("from", PrqlToken::Keyword),
                ("# all", PrqlToken::Comment),
                ("select", PrqlToken::Keyword),
                ("\"x\"", PrqlToken::String),
                ("3", PrqlToken::Literal),
            ]
        );
        assert!(prql_tokens("from 'unclosed").is_empty());
    }
}
