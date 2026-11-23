//! The warehouse, role, database and schema a connection sends with every statement, and the
//! `USE` statements that change them.
//!
//! Snowflake's SQL API keeps no session: each request names its own context, and a `USE` runs
//! only inside a request of several statements, where it lasts until that request ends. So
//! DbRover keeps the context itself, one per connection, and answers a console's `USE` by
//! changing it without asking Snowflake (`docs/cloud-data.md`, C5, "A console's context is
//! DbRover's to keep"). A name that doesn't exist fails on the next statement, with
//! Snowflake's own error.

use std::fmt;

use serde_json::{Map, Value};

/// What a statement is sent with; an empty part is left out, so the user's default applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Context {
    pub warehouse: String,
    pub role: String,
    pub database: String,
    pub schema: String,
}

impl Context {
    /// The parts of a request's body that name the context: `warehouse`, `role`, `database`
    /// and `schema`, each where it is set.
    pub(crate) fn fields(&self) -> Map<String, Value> {
        [
            ("warehouse", &self.warehouse),
            ("role", &self.role),
            ("database", &self.database),
            ("schema", &self.schema),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| (key.to_owned(), Value::from(value.as_str())))
        .collect()
    }

    /// The context after `change`.
    pub(crate) fn apply(&mut self, change: &Use) {
        match change {
            Use::Warehouse(name) => name.clone_into(&mut self.warehouse),
            Use::Role(name) => name.clone_into(&mut self.role),
            Use::Database(name) => {
                name.clone_into(&mut self.database);
                // As in Snowflake, a new database starts in its default schema.
                self.schema = String::new();
            }
            Use::Schema {
                database: Some(database),
                schema,
            } => {
                database.clone_into(&mut self.database);
                schema.clone_into(&mut self.schema);
            }
            Use::Schema {
                database: None,
                schema,
            } => schema.clone_into(&mut self.schema),
        }
    }
}

/// A `USE` statement the connection answers itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Use {
    Warehouse(String),
    Role(String),
    Database(String),
    Schema {
        database: Option<String>,
        schema: String,
    },
}

impl Use {
    /// The `USE` that `statement` is, or `None` for any other statement, and for the `USE`
    /// forms DbRover doesn't keep (`USE SECONDARY ROLES`), which go to Snowflake as they are.
    pub(crate) fn recognise(statement: &str) -> Option<Self> {
        let words = Words::of(statement)?;
        let (first, rest) = words.0.split_first()?;
        if !first.is_keyword("USE") {
            return None;
        }
        match rest {
            [kind, name] if kind.is_keyword("WAREHOUSE") => Some(Self::Warehouse(name.name()?)),
            [kind, name] if kind.is_keyword("ROLE") => Some(Self::Role(name.name()?)),
            [kind, name] if kind.is_keyword("DATABASE") => Some(Self::Database(name.name()?)),
            [kind, name] if kind.is_keyword("SCHEMA") => Self::schema(name),
            [name] => match name.parts()?.as_slice() {
                [database] => Some(Self::Database(database.clone())),
                [database, schema] => Some(Self::Schema {
                    database: Some(database.clone()),
                    schema: schema.clone(),
                }),
                _ => None,
            },
            _ => None,
        }
    }

    fn schema(name: &Word) -> Option<Self> {
        match name.parts()?.as_slice() {
            [schema] => Some(Self::Schema {
                database: None,
                schema: schema.clone(),
            }),
            [database, schema] => Some(Self::Schema {
                database: Some(database.clone()),
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
            Self::Warehouse(name) => write!(f, "Later statements here run on warehouse {name}."),
            Self::Role(name) => write!(f, "Later statements here run as role {name}."),
            Self::Database(name) => write!(f, "Later statements here run in database {name}."),
            Self::Schema {
                database: Some(database),
                schema,
            } => write!(
                f,
                "Later statements here run in schema {database}.{schema}."
            ),
            Self::Schema {
                database: None,
                schema,
            } => write!(f, "Later statements here run in schema {schema}."),
        }
    }
}

/// A statement's words, split on whitespace outside double quotes, with a final `;` dropped.
struct Words(Vec<Word>);

impl Words {
    /// `None` for a statement with a comment or a string in it, which no `USE` DbRover keeps
    /// has; Snowflake reads those itself.
    fn of(statement: &str) -> Option<Self> {
        let text = statement.trim().trim_end_matches(';').trim_end();
        if text.contains("--") || text.contains("//") || text.contains("/*") || text.contains('\'')
        {
            return None;
        }
        let mut words = Vec::new();
        let mut current = String::new();
        let mut quoted = false;
        for character in text.chars() {
            match character {
                '"' => {
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

    /// The word as one name, folded as Snowflake folds it.
    fn name(&self) -> Option<String> {
        match self.parts()?.as_slice() {
            [name] => Some(name.clone()),
            _ => None,
        }
    }

    /// The word's dot-separated names, each folded as Snowflake folds it: unquoted to upper
    /// case, quoted as written, with `""` read as one quote.
    fn parts(&self) -> Option<Vec<String>> {
        let mut parts = Vec::new();
        let mut rest = self.0.as_str();
        loop {
            let (part, after) = if let Some(quoted) = rest.strip_prefix('"') {
                let end = closing_quote(quoted)?;
                (quoted[..end].replace("\"\"", "\""), &quoted[end + 1..])
            } else {
                let end = rest.find('.').unwrap_or(rest.len());
                let part = &rest[..end];
                if part.is_empty() || part.contains('"') {
                    return None;
                }
                (part.to_uppercase(), &rest[end..])
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

/// Where the quote closing a quoted name is in `text` (which starts after the opening one),
/// passing over the `""` that stands for a quote inside the name.
fn closing_quote(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            if bytes.get(index + 1) == Some(&b'"') {
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

    fn schema(database: Option<&str>, schema: &str) -> Use {
        Use::Schema {
            database: database.map(str::to_owned),
            schema: schema.to_owned(),
        }
    }

    #[test]
    fn use_statements_are_read_with_snowflakes_folding() {
        for (statement, recognised) in [
            (
                "USE WAREHOUSE analytics_wh;",
                Some(Use::Warehouse("ANALYTICS_WH".to_owned())),
            ),
            (
                "use role \"Data Reader\"",
                Some(Use::Role("Data Reader".to_owned())),
            ),
            ("USE DATABASE shop", Some(Use::Database("SHOP".to_owned()))),
            ("USE shop", Some(Use::Database("SHOP".to_owned()))),
            ("USE shop.\"Sales\"", Some(schema(Some("SHOP"), "Sales"))),
            ("USE SCHEMA public", Some(schema(None, "PUBLIC"))),
            (
                "USE SCHEMA shop.public",
                Some(schema(Some("SHOP"), "PUBLIC")),
            ),
            ("USE ROLE \"a\"\"b\"", Some(Use::Role("a\"b".to_owned()))),
            ("USE SECONDARY ROLES ALL", None),
            ("USE WAREHOUSE wh -- the big one", None),
            ("USE a.b.c", None),
            ("SELECT 1", None),
            ("USE ROLE \"unclosed", None),
        ] {
            assert_eq!(Use::recognise(statement), recognised, "{statement}");
        }
    }

    #[test]
    fn a_use_changes_only_its_part_of_the_context() {
        let mut context = Context {
            warehouse: "SMALL_WH".to_owned(),
            role: "ANALYST".to_owned(),
            database: "SHOP".to_owned(),
            schema: "PUBLIC".to_owned(),
        };
        context.apply(&Use::Warehouse("BIG_WH".to_owned()));
        assert_eq!(context.warehouse, "BIG_WH");
        context.apply(&Use::Database("SALES".to_owned()));
        assert_eq!(
            (context.database.as_str(), context.schema.as_str()),
            ("SALES", "")
        );
        context.apply(&schema(None, "EU"));
        assert_eq!(context.role, "ANALYST");
        assert_eq!(
            Value::Object(context.fields()),
            serde_json::json!({"warehouse": "BIG_WH", "role": "ANALYST",
                "database": "SALES", "schema": "EU"})
        );
        assert_eq!(
            Value::Object(Context::default().fields()),
            serde_json::json!({})
        );
    }
}
