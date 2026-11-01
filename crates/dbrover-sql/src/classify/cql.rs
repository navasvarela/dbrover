//! CQL's statements, classified by their first words. `sqlparser` has no CQL dialect, and CQL's
//! grammar is small enough that a statement's verb, and for `CREATE`, `ALTER` and `DROP` the
//! kind of object, says what it may change: CQL has no subqueries, `WITH` queries or functions
//! with side effects to hide a write in.
//!
//! The keyspaces a data statement's table is qualified with are found too, for the MCP server's
//! allow-list. Any other statement names objects this reader doesn't look for, so its keyspaces
//! are unknown and an allow-list refuses it.

use std::collections::BTreeSet;

use super::{Analysis, NamePart, NamedSchemas, StatementKind, Unclassified};
use crate::dialect::{CQL_SYSTEM_KEYSPACES, Dialect};
use crate::lexer::{TokenKind, tokenize};
use crate::split::split_statements;

/// The objects whose `CREATE`, `ALTER` and `DROP` change the schema. Roles, users and service
/// levels are administration.
const SCHEMA_OBJECTS: &[&str] = &[
    "aggregate",
    "columnfamily",
    "function",
    "index",
    "keyspace",
    "schema",
    "table",
    "trigger",
    "type",
    "view",
];

/// Words that may come between `CREATE` and the kind of object it makes.
const CREATE_MODIFIERS: &[&str] = &["or", "replace", "custom", "materialized"];

/// Statements that administer the server: permissions and roles, ScyllaDB's service levels and
/// its pruning of a materialized view's stale rows.
const ADMIN_VERBS: &[&str] = &["grant", "revoke", "list", "attach", "detach", "prune"];

/// The verbs of the statements a batch holds.
const DATA_VERBS: &[&str] = &["insert", "update", "delete"];

/// Classifies `sql`, which must hold exactly one CQL statement (a batch is one).
pub(super) fn analyze(sql: &str) -> Result<Analysis, Unclassified> {
    let ranges = split_statements(sql, Dialect::Cassandra);
    let statement = match ranges.as_slice() {
        [] => return Err(Unclassified::Empty),
        [range] => Statement::read(&sql[range.clone()]),
        several => return Err(Unclassified::Several(several.len())),
    };
    statement.analysis()
}

/// One meaningful token of a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// An unquoted word, keyword or name, as written.
    Word(String),
    /// A `"quoted"` name, without its quotes.
    Quoted(String),
    /// A punctuation character.
    Symbol(char),
    /// A string, number or anything else that names nothing.
    Literal,
}

impl Part {
    fn is_word(&self, word: &str) -> bool {
        matches!(self, Self::Word(text) if text.eq_ignore_ascii_case(word))
    }

    fn name(&self) -> Option<NamePart> {
        match self {
            Self::Word(value) => Some(NamePart {
                value: value.clone(),
                quoted: false,
            }),
            Self::Quoted(value) => Some(NamePart {
                value: value.clone(),
                quoted: true,
            }),
            Self::Symbol(_) | Self::Literal => None,
        }
    }
}

struct Statement {
    parts: Vec<Part>,
}

impl Statement {
    fn read(text: &str) -> Self {
        let parts = tokenize(text, Dialect::Cassandra)
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| {
                let text = &text[token.range];
                match token.kind {
                    TokenKind::Word => Part::Word(text.to_owned()),
                    TokenKind::QuotedIdentifier => Part::Quoted(unquote(text)),
                    TokenKind::Semicolon => Part::Symbol(';'),
                    TokenKind::Punctuation => {
                        text.chars().next().map_or(Part::Literal, Part::Symbol)
                    }
                    _ => Part::Literal,
                }
            })
            .collect();
        Self { parts }
    }

    fn word(&self, index: usize) -> Option<String> {
        match self.parts.get(index)? {
            Part::Word(word) => Some(word.to_ascii_lowercase()),
            _ => None,
        }
    }

    fn analysis(&self) -> Result<Analysis, Unclassified> {
        let Some(verb) = self.word(0) else {
            return Err(Unclassified::Unparseable(
                "a CQL statement starts with a keyword".to_owned(),
            ));
        };
        let (kind, tables) = match verb.as_str() {
            "select" => (StatementKind::Read, self.table_after("from", 1)),
            "insert" => (StatementKind::Write, self.table_after("into", 1)),
            "update" => (StatementKind::Write, self.name_at(1)),
            "delete" => (StatementKind::Write, self.table_after("from", 1)),
            "truncate" => (StatementKind::Write, self.truncated_table()),
            "begin" => (StatementKind::Write, self.batch_tables()?),
            "use" => return Ok(self.use_keyspace()),
            "create" | "alter" | "drop" => (self.definition_kind(), Tables::Unknown),
            "describe" | "desc" => (StatementKind::Read, Tables::Unknown),
            admin if ADMIN_VERBS.contains(&admin) => (StatementKind::Admin, Tables::Unknown),
            other => {
                return Err(Unclassified::Unparseable(format!(
                    "\u{201c}{other}\u{201d} doesn't start a CQL statement"
                )));
            }
        };
        Ok(Analysis {
            kind,
            schemas: tables.schemas(),
        })
    }

    /// The table named after the first `keyword` from `start` on: `FROM` in a `SELECT` or a
    /// `DELETE`, `INTO` in an `INSERT`.
    fn table_after(&self, keyword: &str, start: usize) -> Tables {
        self.parts
            .iter()
            .skip(start)
            .position(|part| part.is_word(keyword))
            .map_or(Tables::Unknown, |found| self.name_at(start + found + 1))
    }

    /// `TRUNCATE [TABLE | COLUMNFAMILY] name`.
    fn truncated_table(&self) -> Tables {
        let has_object = self
            .word(1)
            .is_some_and(|word| word == "table" || word == "columnfamily");
        self.name_at(if has_object { 2 } else { 1 })
    }

    /// A name at `index`, `table` or `keyspace.table`.
    fn name_at(&self, index: usize) -> Tables {
        let Some(first) = self.parts.get(index).and_then(Part::name) else {
            return Tables::Unknown;
        };
        if self.parts.get(index + 1) != Some(&Part::Symbol('.')) {
            return Tables::Named(vec![TableName::Unqualified]);
        }
        match self.parts.get(index + 2).and_then(Part::name) {
            Some(_) => Tables::Named(vec![TableName::In(first)]),
            None => Tables::Unknown,
        }
    }

    /// `BEGIN [UNLOGGED | COUNTER] BATCH … APPLY BATCH`: the tables of the `INSERT`, `UPDATE`
    /// and `DELETE` statements it holds. CQL reserves those three words, so wherever one appears
    /// unquoted in a batch, a statement starts.
    fn batch_tables(&self) -> Result<Tables, Unclassified> {
        let header = match (self.word(1).as_deref(), self.word(2).as_deref()) {
            (Some("batch"), _) => 2,
            (Some("unlogged" | "counter"), Some("batch")) => 3,
            _ => {
                return Err(Unclassified::Unparseable(
                    "BEGIN starts a batch in CQL: BEGIN BATCH … APPLY BATCH".to_owned(),
                ));
            }
        };
        let mut tables = Tables::Named(Vec::new());
        for (index, part) in self.parts.iter().enumerate().skip(header) {
            let Part::Word(word) = part else {
                continue;
            };
            let word = word.to_ascii_lowercase();
            if !DATA_VERBS.contains(&word.as_str()) {
                continue;
            }
            let statement = match word.as_str() {
                "insert" => self.table_after("into", index + 1),
                "update" => self.name_at(index + 1),
                _ => self.table_after("from", index + 1),
            };
            tables = tables.and(statement);
        }
        Ok(tables)
    }

    /// `USE keyspace` reads, unless the keyspace is one of the server's own, whose tables an
    /// unqualified name would then reach.
    fn use_keyspace(&self) -> Analysis {
        let Some(keyspace) = self.parts.get(1).and_then(Part::name) else {
            return Analysis {
                kind: StatementKind::Read,
                schemas: NamedSchemas::Unknown,
            };
        };
        let system = CQL_SYSTEM_KEYSPACES.iter().any(|system| {
            if keyspace.quoted {
                keyspace.value == *system
            } else {
                keyspace.value.eq_ignore_ascii_case(system)
            }
        });
        Analysis {
            kind: if system {
                StatementKind::Admin
            } else {
                StatementKind::Read
            },
            schemas: Tables::Named(vec![TableName::In(keyspace)]).schemas(),
        }
    }

    /// What a `CREATE`, `ALTER` or `DROP` changes, by the kind of object after its verb.
    fn definition_kind(&self) -> StatementKind {
        let object = (1..self.parts.len())
            .map_while(|index| self.word(index))
            .find(|word| !CREATE_MODIFIERS.contains(&word.as_str()));
        match object {
            Some(object) if SCHEMA_OBJECTS.contains(&object.as_str()) => StatementKind::Schema,
            _ => StatementKind::Admin,
        }
    }
}

/// The tables a statement names.
enum Tables {
    Named(Vec<TableName>),
    /// A statement whose names this reader doesn't look for, or can't read.
    Unknown,
}

enum TableName {
    /// Resolves to the session's keyspace.
    Unqualified,
    In(NamePart),
}

impl Tables {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Named(mut names), Self::Named(more)) => {
                names.extend(more);
                Self::Named(names)
            }
            _ => Self::Unknown,
        }
    }

    fn schemas(self) -> NamedSchemas {
        let Self::Named(names) = self else {
            return NamedSchemas::Unknown;
        };
        let mut qualified = BTreeSet::new();
        let mut unqualified = false;
        for name in names {
            match name {
                TableName::Unqualified => unqualified = true,
                TableName::In(keyspace) => {
                    qualified.insert(keyspace);
                }
            }
        }
        NamedSchemas::Known {
            qualified,
            unqualified,
        }
    }
}

/// A `"quoted"` name's text, `""` standing for `"`.
fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .map(|rest| rest.strip_suffix('"').unwrap_or(rest))
        .unwrap_or(text)
        .replace("\"\"", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(qualified: &[(&str, bool)], unqualified: bool) -> NamedSchemas {
        NamedSchemas::Known {
            qualified: qualified
                .iter()
                .map(|(value, quoted)| NamePart {
                    value: (*value).to_owned(),
                    quoted: *quoted,
                })
                .collect(),
            unqualified,
        }
    }

    #[test]
    fn finds_the_keyspaces_a_statement_names() {
        let cases = [
            ("SELECT * FROM orders", known(&[], true)),
            (
                "SELECT count(*) FROM shop.orders WHERE id = 1",
                known(&[("shop", false)], false),
            ),
            (
                "INSERT INTO \"Shop\".\"Orders\" (id) VALUES (1)",
                known(&[("Shop", true)], false),
            ),
            ("UPDATE orders SET total = 2 WHERE id = 1", known(&[], true)),
            (
                "DELETE total FROM shop.orders WHERE id = 1",
                known(&[("shop", false)], false),
            ),
            (
                "TRUNCATE TABLE shop.orders",
                known(&[("shop", false)], false),
            ),
            (
                "BEGIN BATCH INSERT INTO shop.a (id) VALUES (1); UPDATE b SET x = 1 WHERE id = 1 \
                 APPLY BATCH",
                known(&[("shop", false)], true),
            ),
            ("USE \"Shop\"", known(&[("Shop", true)], false)),
            (
                "CREATE TABLE shop.t (id int PRIMARY KEY)",
                NamedSchemas::Unknown,
            ),
            ("SELECT * FROM", NamedSchemas::Unknown),
        ];
        for (sql, expected) in cases {
            assert_eq!(
                analyze(sql).map(|analysis| analysis.schemas),
                Ok(expected),
                "{sql}"
            );
        }
    }

    #[test]
    fn unquotes_names() {
        assert_eq!(unquote("\"a\"\"b\""), "a\"b");
        assert_eq!(unquote("\"open"), "open");
    }
}
