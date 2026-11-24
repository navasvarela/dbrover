//! The catalog and schema a connection sends with every statement, the `USE` statements that
//! change them, and the statements that would need a session.
//!
//! Databricks's Statement Execution API keeps no session: each request names its own catalog
//! and schema, and nothing a statement sets lasts to the next. So DbRover keeps the context
//! itself, one per connection, and answers a console's `USE` by changing it without asking
//! Databricks (`docs/cloud-data.md`, C6, "A console's context is DbRover's to keep"). A name
//! that doesn't exist fails on the next statement, with Databricks's own error.

use std::fmt;

use serde_json::{Map, Value};

/// What a statement is sent with; an empty part is left out, so the workspace's default
/// applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Context {
    pub catalog: String,
    pub schema: String,
}

impl Context {
    /// The parts of a request's body that name the context: `catalog` and `schema`, each where
    /// it is set.
    pub(crate) fn fields(&self) -> Map<String, Value> {
        [("catalog", &self.catalog), ("schema", &self.schema)]
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.to_owned(), Value::from(value.as_str())))
            .collect()
    }

    /// The context after `change`.
    pub(crate) fn apply(&mut self, change: &Use) {
        match change {
            Use::Catalog(name) => {
                name.clone_into(&mut self.catalog);
                // As in Databricks, a new catalog starts in its default schema.
                self.schema = String::new();
            }
            Use::Schema {
                catalog: Some(catalog),
                schema,
            } => {
                catalog.clone_into(&mut self.catalog);
                schema.clone_into(&mut self.schema);
            }
            Use::Schema {
                catalog: None,
                schema,
            } => schema.clone_into(&mut self.schema),
        }
    }
}

/// A `USE` statement the connection answers itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Use {
    Catalog(String),
    Schema {
        catalog: Option<String>,
        schema: String,
    },
}

impl Use {
    /// The `USE` that `statement` is: `USE CATALOG c`, `SET CATALOG c`, `USE SCHEMA s`, `USE
    /// DATABASE s`, `USE s` or `USE c.s`. `None` for any other statement, and for a `USE` with
    /// a comment or a string in it, which go to Databricks as they are.
    pub(crate) fn recognise(statement: &str) -> Option<Self> {
        let words = Words::of(statement)?;
        let (first, rest) = words.0.split_first()?;
        match rest {
            [kind, name]
                if (first.is_keyword("USE") || first.is_keyword("SET"))
                    && kind.is_keyword("CATALOG") =>
            {
                Some(Self::Catalog(name.name()?))
            }
            [kind, name]
                if first.is_keyword("USE")
                    && (kind.is_keyword("SCHEMA") || kind.is_keyword("DATABASE")) =>
            {
                Self::schema(name)
            }
            [name] if first.is_keyword("USE") => Self::schema(name),
            _ => None,
        }
    }

    fn schema(name: &Word) -> Option<Self> {
        match name.parts()?.as_slice() {
            [schema] => Some(Self::Schema {
                catalog: None,
                schema: schema.clone(),
            }),
            [catalog, schema] => Some(Self::Schema {
                catalog: Some(catalog.clone()),
                schema: schema.clone(),
            }),
            _ => None,
        }
    }
}

/// What the console shows for a `USE`: what the connection's later statements will use.
impl fmt::Display for Use {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(name) => write!(f, "Later statements here run in catalog {name}."),
            Self::Schema {
                catalog: Some(catalog),
                schema,
            } => write!(f, "Later statements here run in schema {catalog}.{schema}."),
            Self::Schema {
                catalog: None,
                schema,
            } => write!(f, "Later statements here run in schema {schema}."),
        }
    }
}

/// What a statement that would need a session is, for the refusal that names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeedsSession {
    /// `SET key = value`, `SET VAR`, `SET TIME ZONE`, `RESET`: a setting or a variable.
    Setting,
    /// `DECLARE VARIABLE`.
    Variable,
    /// `CREATE TEMPORARY VIEW` or `FUNCTION`.
    TemporaryObject,
    /// `BEGIN`, `START TRANSACTION`, `COMMIT`, `ROLLBACK`.
    Transaction,
}

impl NeedsSession {
    /// What `statement` would need a session for, or `None` for one that needs none. `SET`
    /// alone lists the settings, and `SET CATALOG` is a `USE`, so neither is refused.
    pub(crate) fn of(statement: &str) -> Option<Self> {
        let words = leading_words(statement, 5);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["set"] | ["set", "catalog", ..] => None,
            ["set", ..] | ["reset", ..] => Some(Self::Setting),
            ["declare", ..] => Some(Self::Variable),
            ["begin", ..] | ["start", "transaction", ..] | ["commit", ..] | ["rollback", ..] => {
                Some(Self::Transaction)
            }
            ["create", rest @ ..] => {
                let rest = match rest {
                    ["or", "replace", rest @ ..] => rest,
                    rest => rest,
                };
                matches!(
                    rest,
                    ["temp" | "temporary", "view" | "function", ..]
                        | ["temp" | "temporary", "table", ..]
                )
                .then_some(Self::TemporaryObject)
            }
            _ => None,
        }
    }

    /// Why DbRover doesn't send the statement.
    pub(crate) fn refusal(self) -> String {
        let what = match self {
            Self::Setting => "A setting or a variable set here wouldn't last",
            Self::Variable => "A variable declared here wouldn't last",
            Self::TemporaryObject => "A temporary view or function made here wouldn't last",
            Self::Transaction => "A transaction opened here wouldn't last",
        };
        format!(
            "{what} to the next statement: Databricks's Statement Execution API keeps no session \
             between statements, so DbRover doesn't send it. Use the data source's catalog and \
             schema, or USE CATALOG and USE SCHEMA, which DbRover keeps for this console; for a \
             setting, set it on the SQL warehouse."
        )
    }
}

/// The first `count` words of `statement` after any leading comments, lowercased, with
/// punctuation that ends a word (`(`, `;`, `=`) left out.
fn leading_words(statement: &str, count: usize) -> Vec<String> {
    let mut text = statement.trim_start();
    loop {
        if let Some(rest) = text.strip_prefix("--") {
            text = rest
                .split_once('\n')
                .map_or("", |(_, after)| after)
                .trim_start();
        } else if let Some(rest) = text.strip_prefix("/*") {
            text = rest
                .split_once("*/")
                .map_or("", |(_, after)| after)
                .trim_start();
        } else {
            break;
        }
    }
    text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ';' | '='))
        .filter(|word| !word.is_empty())
        .take(count)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A statement's words, split on whitespace outside backquotes, with a final `;` dropped.
struct Words(Vec<Word>);

impl Words {
    /// `None` for a statement with a comment or a string in it, which no `USE` DbRover keeps
    /// has; Databricks reads those itself.
    fn of(statement: &str) -> Option<Self> {
        let text = statement.trim().trim_end_matches(';').trim_end();
        if text.contains("--") || text.contains("/*") || text.contains(['\'', '"']) {
            return None;
        }
        let mut words = Vec::new();
        let mut current = String::new();
        let mut quoted = false;
        for character in text.chars() {
            match character {
                '`' => {
                    quoted = !quoted;
                    current.push(character);
                }
                character if character.is_whitespace() && !quoted => {
                    if !current.is_empty() {
                        words.push(Word(std::mem::take(&mut current)));
                    }
                }
                character => current.push(character),
            }
        }
        if quoted {
            return None;
        }
        if !current.is_empty() {
            words.push(Word(current));
        }
        Some(Self(words))
    }
}

/// One word of a statement, as written.
struct Word(String);

impl Word {
    fn is_keyword(&self, keyword: &str) -> bool {
        self.0.eq_ignore_ascii_case(keyword)
    }

    /// The word as one name.
    fn name(&self) -> Option<String> {
        match self.parts()?.as_slice() {
            [name] => Some(name.clone()),
            _ => None,
        }
    }

    /// The word's dot-separated names, each as written: Databricks matches names whatever
    /// their case, so none is folded; a backquoted one without its quotes, with `` `` `` read as
    /// one backquote.
    fn parts(&self) -> Option<Vec<String>> {
        let mut parts = Vec::new();
        let mut rest = self.0.as_str();
        loop {
            let (part, after) = if let Some(quoted) = rest.strip_prefix('`') {
                let end = closing_backquote(quoted)?;
                (quoted[..end].replace("``", "`"), &quoted[end + 1..])
            } else {
                let end = rest.find('.').unwrap_or(rest.len());
                let part = &rest[..end];
                if part.is_empty() || part.contains('`') {
                    return None;
                }
                (part.to_owned(), &rest[end..])
            };
            parts.push(part);
            match after.strip_prefix('.') {
                Some(next) => rest = next,
                None if after.is_empty() => return Some(parts),
                None => return None,
            }
        }
    }
}

/// Where the backquote closing a quoted name is in `text` (which starts after the opening
/// one), passing over the ``` `` ``` that stands for a backquote inside the name.
fn closing_backquote(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            if bytes.get(index + 1) == Some(&b'`') {
                index += 2;
                continue;
            }
            return Some(index);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(catalog: Option<&str>, schema: &str) -> Use {
        Use::Schema {
            catalog: catalog.map(str::to_owned),
            schema: schema.to_owned(),
        }
    }

    #[test]
    fn use_statements_are_read_as_databricks_reads_them() {
        for (statement, recognised) in [
            ("USE CATALOG main;", Some(Use::Catalog("main".to_owned()))),
            ("set catalog dev", Some(Use::Catalog("dev".to_owned()))),
            (
                "use catalog `my catalog`",
                Some(Use::Catalog("my catalog".to_owned())),
            ),
            ("USE SCHEMA sales", Some(schema(None, "sales"))),
            ("USE DATABASE Sales", Some(schema(None, "Sales"))),
            ("USE sales", Some(schema(None, "sales"))),
            (
                "USE main.`EU sales`",
                Some(schema(Some("main"), "EU sales")),
            ),
            ("USE SCHEMA main.sales", Some(schema(Some("main"), "sales"))),
            ("USE CATALOG `a``b`", Some(Use::Catalog("a`b".to_owned()))),
            ("USE CATALOG 'main'", None),
            ("USE SCHEMA s -- the sales one", None),
            ("USE a.b.c", None),
            ("SELECT 1", None),
            ("USE CATALOG `unclosed", None),
        ] {
            assert_eq!(Use::recognise(statement), recognised, "{statement}");
        }
    }

    #[test]
    fn a_use_changes_only_its_part_of_the_context() {
        let mut context = Context {
            catalog: "main".to_owned(),
            schema: "sales".to_owned(),
        };
        context.apply(&schema(None, "eu"));
        assert_eq!(context.catalog, "main");
        context.apply(&Use::Catalog("dev".to_owned()));
        assert_eq!(
            (context.catalog.as_str(), context.schema.as_str()),
            ("dev", "")
        );
        assert_eq!(
            Value::Object(context.fields()),
            serde_json::json!({"catalog": "dev"})
        );
        context.apply(&schema(Some("main"), "eu"));
        assert_eq!(
            Value::Object(context.fields()),
            serde_json::json!({"catalog": "main", "schema": "eu"})
        );
    }

    #[test]
    fn statements_that_need_a_session_are_recognised() {
        for (statement, needs) in [
            ("SET ansi_mode = true", Some(NeedsSession::Setting)),
            ("set statement_timeout=60", Some(NeedsSession::Setting)),
            ("SET VAR x = 1", Some(NeedsSession::Setting)),
            ("SET TIME ZONE 'UTC'", Some(NeedsSession::Setting)),
            ("RESET ansi_mode", Some(NeedsSession::Setting)),
            ("-- a note\nSET x = 1", Some(NeedsSession::Setting)),
            ("DECLARE VARIABLE n INT", Some(NeedsSession::Variable)),
            (
                "CREATE OR REPLACE TEMPORARY VIEW v AS SELECT 1",
                Some(NeedsSession::TemporaryObject),
            ),
            (
                "create temp function f() returns int return 1",
                Some(NeedsSession::TemporaryObject),
            ),
            ("BEGIN TRANSACTION", Some(NeedsSession::Transaction)),
            ("COMMIT;", Some(NeedsSession::Transaction)),
            ("ROLLBACK", Some(NeedsSession::Transaction)),
            ("SET", None),
            ("SET CATALOG main", None),
            ("CREATE VIEW v AS SELECT 1", None),
            ("CREATE TABLE temp (id INT)", None),
            ("SELECT 'set x = 1'", None),
            ("settings", None),
        ] {
            assert_eq!(NeedsSession::of(statement), needs, "{statement}");
        }
        assert!(
            NeedsSession::Setting
                .refusal()
                .contains("keeps no session between statements")
        );
    }
}
