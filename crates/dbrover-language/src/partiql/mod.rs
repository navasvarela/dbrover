//! PartiQL as Amazon DynamoDB runs it, through `ExecuteStatement`: the language of DynamoDB's
//! consoles (`docs/nosql.md`, N5).
//!
//! ```text
//! SELECT * FROM "Orders" WHERE "customer" = 'c-42' AND "placed" > '2026-10'
//! ```
//!
//! DynamoDB has four statements, and each `UPDATE` and `DELETE` changes one item, named by its
//! whole primary key. What can cost the most is a `SELECT` that doesn't name a partition, which
//! reads the whole table: [`Statement::access`] tells, given the table's partition key, which
//! only the database knows.

mod edit;
mod lexer;
mod parser;
#[cfg(test)]
mod tests;
mod words;

use std::ops::Range;

use dbrover_core::{CatalogPath, NodeKind};
use dbrover_sql::{StatementKind, TokenKind, Unclassified};

pub use edit::{TableView, literal, typed_value};
pub use parser::{Access, Condition, ParseError, Statement, Term, Verb, parse};

pub(crate) use lexer::tokenize;
pub(crate) use words::{completion_words, detail as syntax, is_keyword};

/// One range per statement, from its first to its last meaningful token: statements end at
/// `;`, outside strings and comments.
pub(crate) fn split(script: &str) -> Vec<Range<usize>> {
    let mut statements = Vec::new();
    let mut current: Option<Range<usize>> = None;
    for token in tokenize(script) {
        match token.kind {
            kind if kind.is_trivia() => {}
            TokenKind::Semicolon => statements.extend(current.take()),
            _ => {
                current = Some(match current {
                    Some(statement) => statement.start..token.range.end,
                    None => token.range,
                });
            }
        }
    }
    statements.extend(current);
    statements
}

/// What **Open data** runs on a table, or on one of its indexes: every item, read a page at a
/// time.
pub fn select_all(table: &str, index: Option<&str>) -> String {
    match index {
        Some(index) => format!("SELECT * FROM {}.{}", quote(table), quote(index)),
        None => format!("SELECT * FROM {}", quote(table)),
    }
}

/// The `SELECT *` **Open data** runs on the DynamoDB table or index at `path`: a table at the
/// tree's top, or an index in its table's indexes folder. `None` for any other node.
pub(crate) fn read_all(path: &CatalogPath) -> Option<String> {
    match path.segments() {
        [table] if table.kind == NodeKind::Table => Some(select_all(&table.name, None)),
        [table, _, index] if table.kind == NodeKind::Table && index.kind == NodeKind::Index => {
            Some(select_all(&table.name, Some(&index.name)))
        }
        _ => None,
    }
}

/// `name` as a PartiQL `"quoted name"`, which DynamoDB's names with dots or dashes need.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The one statement `text` holds, read.
fn single(text: &str) -> Result<Statement, Unclassified> {
    match split(text).as_slice() {
        [] => Err(Unclassified::Empty),
        [range] => {
            parse(&text[range.clone()]).map_err(|error| Unclassified::Unparseable(error.message))
        }
        several => Err(Unclassified::Several(several.len())),
    }
}

/// The one statement `statement` holds, read, for the scan guard.
///
/// # Errors
///
/// [`Unclassified`] when it holds no statement, several, or one DbRover can't read.
pub fn statement(statement: &str) -> Result<Statement, Unclassified> {
    single(statement)
}

/// What `statement` may change: a `SELECT` reads, and the rest write.
pub(crate) fn classify(statement: &str) -> Result<StatementKind, Unclassified> {
    single(statement).map(|statement| match statement.verb {
        Verb::Select => StatementKind::Read,
        Verb::Insert | Verb::Update | Verb::Delete => StatementKind::Write,
    })
}

/// Whether `statement` answers with items, which Run to file writes as documents: a `SELECT`.
pub(crate) fn returns_documents(statement: &str) -> bool {
    single(statement).is_ok_and(|statement| statement.verb == Verb::Select)
}
