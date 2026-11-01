//! Reads one PartiQL statement as far as DbRover needs: its verb, the table (and index) it
//! names, and its `WHERE` as the conditions joined by `AND` at the top. DynamoDB reads the rest
//! itself, and says what it can't.
//!
//! The statements, as DynamoDB documents them:
//!
//! ```text
//! SELECT projection FROM table ("." index)? (WHERE condition)? (ORDER BY …)?
//! INSERT INTO table VALUE item
//! UPDATE table (SET … | REMOVE …)+ WHERE condition (RETURNING …)?
//! DELETE FROM table WHERE condition (RETURNING …)?
//! ```

use std::fmt;
use std::ops::Range;

use dbrover_sql::{Token, TokenKind};

use super::lexer::tokenize;

/// Why a statement can't be read, with where in it the trouble is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// The bytes of the statement the message is about.
    pub range: Range<usize>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Select,
    Insert,
    Update,
    Delete,
}

impl Verb {
    fn from_word(word: &str) -> Option<Self> {
        [
            ("select", Self::Select),
            ("insert", Self::Insert),
            ("update", Self::Update),
            ("delete", Self::Delete),
        ]
        .into_iter()
        .find_map(|(name, verb)| word.eq_ignore_ascii_case(name).then_some(verb))
    }
}

/// One statement, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub verb: Verb,
    /// The table it reads or changes, unquoted.
    pub table: String,
    /// The secondary index a `SELECT` reads, named after the table: `"Orders"."ByStatus"`.
    pub index: Option<String>,
    pub condition: Condition,
}

/// A statement's `WHERE`, read only as far as telling a read by key from a Scan needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// No `WHERE`: every item.
    Everything,
    /// The conditions joined by `AND` at the top of the `WHERE`, parentheses around them
    /// taken away.
    All(Vec<Term>),
    /// A `WHERE` with `OR` at its top, which may match items of any partition.
    Any,
}

/// One condition of a `WHERE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// `name = value` or `value = name`, with a value written in the statement or a `?`.
    Equals(String),
    /// `name IN [value, …]`.
    In(String),
    /// Anything else: a range, a function, a path into a map, a comparison of two attributes.
    Other,
}

/// How a `SELECT` reads its table, for the scan guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Only the partitions its `WHERE` names, as DynamoDB's `Query` reads them.
    Partitions,
    /// Every item of the table or index, as DynamoDB's `Scan` reads them, billed for each
    /// whatever the `WHERE` keeps.
    Scan,
}

impl Statement {
    /// How the statement reads, when `partition_key` is the partition key of the table or
    /// index it names. DynamoDB reads by key when the `WHERE` holds, at its top, an `=` or an
    /// `IN` on the partition key; anything else is a Scan. Only a `SELECT` can scan: the other
    /// statements change one item, named by its whole key.
    pub fn access(&self, partition_key: &str) -> Access {
        if self.verb != Verb::Select {
            return Access::Partitions;
        }
        let Condition::All(terms) = &self.condition else {
            return Access::Scan;
        };
        let names_partition = terms.iter().any(|term| match term {
            Term::Equals(name) | Term::In(name) => name == partition_key,
            Term::Other => false,
        });
        match names_partition {
            true => Access::Partitions,
            false => Access::Scan,
        }
    }
}

/// What the error for any other statement says DynamoDB runs.
const STATEMENTS: &str = "DynamoDB's PartiQL runs SELECT, INSERT, UPDATE and DELETE";

/// Reads `statement`, one statement as [`super::split`] gives them.
///
/// # Errors
///
/// [`ParseError`] for text that isn't one of DynamoDB's statements, or whose table can't be
/// found, or whose brackets or quotes don't close.
pub fn parse(statement: &str) -> Result<Statement, ParseError> {
    let tokens: Vec<Token> = tokenize(statement)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let parser = Parser {
        text: statement,
        tokens,
    };
    parser.check_quotes_and_brackets()?;
    parser.statement()
}

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
}

impl Parser<'_> {
    fn text_of(&self, index: usize) -> &str {
        &self.text[self.tokens[index].range.clone()]
    }

    fn is_word(&self, index: usize, word: &str) -> bool {
        self.tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Word)
            && self.text_of(index).eq_ignore_ascii_case(word)
    }

    fn is_punctuation(&self, index: usize, punctuation: &str) -> bool {
        self.tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation)
            && self.text_of(index) == punctuation
    }

    fn error(&self, message: String, index: usize) -> ParseError {
        let range = match self.tokens.get(index) {
            Some(token) => token.range.clone(),
            None => self.text.len()..self.text.len(),
        };
        ParseError { message, range }
    }

    fn check_quotes_and_brackets(&self) -> Result<(), ParseError> {
        let mut open: Vec<(usize, &str)> = Vec::new();
        for (index, token) in self.tokens.iter().enumerate() {
            let text = self.text_of(index);
            match token.kind {
                TokenKind::String | TokenKind::QuotedIdentifier if !is_closed(text) => {
                    return Err(self.error(format!("{} isn't closed", quote_name(text)), index));
                }
                TokenKind::Punctuation => {
                    if let Some(closer) = closer_of(text) {
                        open.push((index, closer));
                    } else if is_closer(text) {
                        match open.pop() {
                            Some((_, expected)) if expected == text => {}
                            Some((_, expected)) => {
                                return Err(self.error(format!("Expected `{expected}`"), index));
                            }
                            None => {
                                return Err(self.error(format!("`{text}` closes nothing"), index));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        match open.pop() {
            Some((index, closer)) => Err(self.error(
                format!("`{}` isn't closed by `{closer}`", self.text_of(index)),
                index,
            )),
            None => Ok(()),
        }
    }

    fn statement(&self) -> Result<Statement, ParseError> {
        let Some(first) = self.tokens.first() else {
            return Err(self.error("No statement".to_owned(), 0));
        };
        let verb = (first.kind == TokenKind::Word)
            .then(|| Verb::from_word(self.text_of(0)))
            .flatten()
            .ok_or_else(|| {
                self.error(
                    format!("{STATEMENTS}; `{}` isn't one of them", self.text_of(0)),
                    0,
                )
            })?;
        let table_at = match verb {
            Verb::Select => self.after_top_level_word(1, "from", "FROM")?,
            Verb::Insert => self.after_word(1, "into", "INSERT INTO")?,
            Verb::Update => 1,
            Verb::Delete => self.after_word(1, "from", "DELETE FROM")?,
        };
        let table = self.name(table_at)?;
        let index = match verb == Verb::Select && self.is_punctuation(table_at + 1, ".") {
            true => Some(self.name(table_at + 2)?),
            false => None,
        };
        Ok(Statement {
            verb,
            table,
            index,
            condition: self.condition(table_at + 1),
        })
    }

    /// The index after `word`, which must be at `index`.
    fn after_word(&self, index: usize, word: &str, phrase: &str) -> Result<usize, ParseError> {
        match self.is_word(index, word) {
            true => Ok(index + 1),
            false => Err(self.error(format!("Expected {phrase} and a table"), index)),
        }
    }

    /// The index after the first `word` outside brackets, from `from` on.
    fn after_top_level_word(
        &self,
        from: usize,
        word: &str,
        phrase: &str,
    ) -> Result<usize, ParseError> {
        self.top_level(from..self.tokens.len())
            .find(|&index| self.is_word(index, word))
            .map(|index| index + 1)
            .ok_or_else(|| self.error(format!("Expected {phrase} and a table"), from))
    }

    /// A table's or index's name at `index`, unquoted.
    fn name(&self, index: usize) -> Result<String, ParseError> {
        let Some(token) = self.tokens.get(index) else {
            return Err(self.error("Expected a table's name".to_owned(), index));
        };
        match token.kind {
            TokenKind::Word => Ok(self.text_of(index).to_owned()),
            TokenKind::QuotedIdentifier => Ok(unquote(self.text_of(index))),
            _ => Err(self.error("Expected a table's name".to_owned(), index)),
        }
    }

    /// The `WHERE` after `from`, up to an `ORDER BY` or `RETURNING` outside brackets.
    fn condition(&self, from: usize) -> Condition {
        let Some(start) = self
            .top_level(from..self.tokens.len())
            .find(|&index| self.is_word(index, "where"))
        else {
            return Condition::Everything;
        };
        let end = self
            .top_level(start..self.tokens.len())
            .find(|&index| {
                (self.is_word(index, "order") && self.is_word(index + 1, "by"))
                    || self.is_word(index, "returning")
            })
            .unwrap_or(self.tokens.len());
        match self.terms(start + 1..end) {
            Some(terms) => Condition::All(terms),
            None => Condition::Any,
        }
    }

    /// The conditions joined by `AND` at the top of `range`; `None` when an `OR` is at its top.
    /// Parentheses around the whole range, or around a condition, are looked through.
    fn terms(&self, range: Range<usize>) -> Option<Vec<Term>> {
        let range = self.without_parentheses(range);
        if self
            .top_level(range.clone())
            .any(|index| self.is_word(index, "or"))
        {
            return None;
        }
        let parts = self.split_at_and(range.clone());
        if parts.len() == 1 {
            return Some(vec![self.term(range)]);
        }
        let mut terms = Vec::new();
        for part in parts {
            match self.terms(part) {
                Some(inner) => terms.extend(inner),
                None => terms.push(Term::Other),
            }
        }
        Some(terms)
    }

    /// `range` split at each `AND` outside brackets, except the one a `BETWEEN` takes.
    fn split_at_and(&self, range: Range<usize>) -> Vec<Range<usize>> {
        let mut parts = Vec::new();
        let mut start = range.start;
        let mut between = false;
        for index in self.top_level(range.clone()) {
            if self.is_word(index, "between") {
                between = true;
            } else if self.is_word(index, "and") {
                if between {
                    between = false;
                } else {
                    parts.push(start..index);
                    start = index + 1;
                }
            }
        }
        parts.push(start..range.end);
        parts
    }

    fn term(&self, range: Range<usize>) -> Term {
        let equals = (range.start + 1..range.end).find(|&index| self.is_punctuation(index, "="));
        if let Some(equals) = equals {
            let (left, right) = (range.start..equals, equals + 1..range.end);
            let name = match (self.lone_attribute(&left), self.lone_attribute(&right)) {
                (Some(name), None) if self.is_value(right) => Some(name),
                (None, Some(name)) if self.is_value(left) => Some(name),
                _ => None,
            };
            return name.map_or(Term::Other, Term::Equals);
        }
        let list = range.start + 2;
        let is_list = (self.is_punctuation(list, "[") || self.is_punctuation(list, "("))
            && self.closing(list) == Some(range.end - 1);
        if range.len() >= 4
            && self.is_word(range.start + 1, "in")
            && is_list
            && let Some(name) = self.lone_attribute(&(range.start..range.start + 1))
        {
            return Term::In(name);
        }
        Term::Other
    }

    fn lone_attribute(&self, range: &Range<usize>) -> Option<String> {
        match range.len() {
            1 => self.attribute(range.start),
            _ => None,
        }
    }

    /// The attribute named at `index` alone: a name, unquoted, or a `"quoted name"`; not a
    /// literal word such as `true`, nor a path into a map, which would take more tokens.
    fn attribute(&self, index: usize) -> Option<String> {
        let token = self.tokens.get(index)?;
        match token.kind {
            TokenKind::QuotedIdentifier => Some(unquote(self.text_of(index))),
            TokenKind::Word if !is_literal_word(self.text_of(index)) => {
                Some(self.text_of(index).to_owned())
            }
            _ => None,
        }
    }

    /// Whether `range` is one value written in the statement, or a `?` for one: a string, a
    /// number with or without its sign, or a literal word.
    fn is_value(&self, range: Range<usize>) -> bool {
        let start = match range.len() == 2 && self.is_punctuation(range.start, "-") {
            true => range.start + 1,
            false => range.start,
        };
        if start + 1 != range.end {
            return false;
        }
        let Some(token) = self.tokens.get(start) else {
            return false;
        };
        match token.kind {
            TokenKind::String | TokenKind::Number => true,
            TokenKind::Punctuation => self.text_of(start) == "?",
            TokenKind::Word => is_literal_word(self.text_of(start)),
            _ => false,
        }
    }

    /// `range` without the parentheses around the whole of it, as many pairs as there are.
    fn without_parentheses(&self, mut range: Range<usize>) -> Range<usize> {
        while range.len() >= 2
            && self.is_punctuation(range.start, "(")
            && self.closing(range.start) == Some(range.end - 1)
        {
            range = range.start + 1..range.end - 1;
        }
        range
    }

    /// The index of the bracket closing the one opened at `open`.
    fn closing(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for index in open..self.tokens.len() {
            let text = self.text_of(index);
            if self.tokens[index].kind != TokenKind::Punctuation {
                continue;
            }
            if closer_of(text).is_some() {
                depth += 1;
            } else if is_closer(text) {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
        }
        None
    }

    /// The indices in `range` outside any brackets opened within it.
    fn top_level(&self, range: Range<usize>) -> impl Iterator<Item = usize> + '_ {
        let mut depth = 0usize;
        range.filter(move |&index| {
            if self.tokens[index].kind == TokenKind::Punctuation {
                let text = self.text_of(index);
                if closer_of(text).is_some() {
                    depth += 1;
                    return false;
                }
                if is_closer(text) {
                    depth = depth.saturating_sub(1);
                    return false;
                }
            }
            depth == 0
        })
    }
}

/// The bracket that closes `open`, when `open` opens one.
fn closer_of(open: &str) -> Option<&'static str> {
    match open {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" => Some("}"),
        "<<" => Some(">>"),
        _ => None,
    }
}

fn is_closer(text: &str) -> bool {
    matches!(text, ")" | "]" | "}" | ">>")
}

/// The words that are values rather than names.
fn is_literal_word(word: &str) -> bool {
    ["true", "false", "null", "missing"]
        .iter()
        .any(|literal| word.eq_ignore_ascii_case(literal))
}

/// Whether a quoted token ends with its quote: a quote written twice stands for itself, so a
/// closed one holds an even number of them.
fn is_closed(text: &str) -> bool {
    let Some(quote) = text.chars().next() else {
        return false;
    };
    text.len() >= 2 && text.ends_with(quote) && text.matches(quote).count() % 2 == 0
}

fn quote_name(text: &str) -> &'static str {
    match text.starts_with('"') {
        true => "The quoted name",
        false => "The string",
    }
}

/// A `"quoted name"` without its quotes, a doubled quote standing for one.
fn unquote(text: &str) -> String {
    text.get(1..text.len().saturating_sub(1))
        .unwrap_or_default()
        .replace("\"\"", "\"")
}
