//! `:name` parameters in every language, and the literals that replace them, for the commands
//! a report sends (`docs/reporting.md`, "Lookups"). SQL's parameters are `dbrover_sql`'s, which
//! a SQL engine's driver may bind; in the other languages a value is always written into the
//! command, as the language reads a literal of its type.

use std::num::NonZeroUsize;

use dbrover_sql::{Dialect, Parameter, Token, TokenKind};

use crate::{QueryLanguage, mongo, redis};

/// A value written into a command, with the type that decides how it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Null,
    Text(String),
    /// Digits as a database writes them, `-12.50`, written unquoted.
    Number(String),
    Boolean(bool),
    /// `2026-10-01`.
    Date(String),
    /// `2026-10-01 12:00:00`, with no offset: a time in UTC.
    Timestamp(String),
}

/// The most values one command's list takes in the languages that don't say otherwise: a
/// thousand short keys keep a statement well under ClickHouse's 256 KiB `max_query_size` and
/// the lengths of `IN` list SQL Server handles, and a MongoDB or Elasticsearch request small.
const DEFAULT_LIST_VALUES: NonZeroUsize = NonZeroUsize::new(1_000).expect("1,000 isn't zero");

/// DynamoDB refuses an `IN` of more operands: "The IN operator is provided with too many
/// operands" (DynamoDB Local, 2026-11, at 101). ScyllaDB's default
/// `max_partition_key_restrictions_per_query` is the same, and Cassandra's guardrails warn
/// past it.
const KEY_LIST_VALUES: NonZeroUsize = NonZeroUsize::new(100).expect("100 isn't zero");

impl QueryLanguage {
    /// Every `:name` parameter in `command`, in order, repeats included. Never one inside a
    /// string or a comment, nor a `:` written against what comes before it (`asset:id`,
    /// `{a:b}`, `x::int`).
    pub fn parameters(self, command: &str) -> Vec<Parameter> {
        match self {
            Self::Sql(dialect) => dbrover_sql::parameters(command, dialect),
            Self::Redis => redis_parameters(command, &self.tokens(command)),
            Self::Mongo | Self::Search | Self::PartiQl => {
                punctuated_parameters(command, &self.tokens(command))
            }
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => punctuated_parameters(command, &self.tokens(command)),
        }
    }

    /// `command` with each parameter `value` gives a replacement for replaced by it; the others
    /// are left as they are, for a driver to bind.
    pub fn replace_parameters(
        self,
        command: &str,
        mut value: impl FnMut(&str) -> Option<String>,
    ) -> String {
        let mut replaced = String::with_capacity(command.len());
        let mut copied = 0;
        for parameter in self.parameters(command) {
            if let Some(text) = value(&parameter.name) {
                replaced.push_str(&command[copied..parameter.range.start]);
                replaced.push_str(&text);
                copied = parameter.range.end;
            }
        }
        replaced.push_str(&command[copied..]);
        replaced
    }

    /// `value` as the language reads a literal of its type.
    pub fn literal(self, value: &Literal) -> String {
        match self {
            Self::Sql(dialect) => sql_literal(dialect, value),
            Self::PartiQl => partiql_literal(value),
            Self::Mongo => mongo_literal(value),
            Self::Search => json_literal(value),
            Self::Redis => redis_literal(value),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => json_literal(value),
        }
    }

    /// `values` as the language writes a list where a parameter stands: `('a', 7)` for SQL's
    /// `IN :ids`, `['a', 7]` for PartiQL's, a JSON array for MongoDB and Elasticsearch, and the
    /// values one after another for a Redis command's arguments.
    pub fn list(self, values: &[Literal]) -> String {
        let items: Vec<String> = values.iter().map(|value| self.literal(value)).collect();
        match self {
            Self::Sql(_) => format!("({})", items.join(", ")),
            Self::Redis => items.join(" "),
            Self::PartiQl | Self::Mongo | Self::Search => format!("[{}]", items.join(", ")),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => format!("[{}]", items.join(", ")),
        }
    }

    /// The most values one command's list should hold, past which a report splits it into
    /// batches.
    pub fn max_list_values(self) -> NonZeroUsize {
        match self {
            Self::PartiQl | Self::Sql(Dialect::Cassandra) => KEY_LIST_VALUES,
            Self::Sql(_) | Self::Redis | Self::Mongo | Self::Search => DEFAULT_LIST_VALUES,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => DEFAULT_LIST_VALUES,
        }
    }
}

/// Parameters in a language whose lexer makes `:` a token of its own, as SQL's does: a `:`
/// right before a name, and not right after a name, a value or another `:`.
fn punctuated_parameters(command: &str, tokens: &[Token]) -> Vec<Parameter> {
    let text = |token: &Token| &command[token.range.clone()];
    let mut found = Vec::new();
    for (index, colon) in tokens.iter().enumerate() {
        if colon.kind != TokenKind::Punctuation || text(colon) != ":" {
            continue;
        }
        let Some(name) = tokens.get(index + 1) else {
            continue;
        };
        if name.kind != TokenKind::Word
            || name.range.start != colon.range.end
            || !starts_name(text(name))
        {
            continue;
        }
        let attached = index
            .checked_sub(1)
            .map(|before| &tokens[before])
            .filter(|before| before.range.end == colon.range.start);
        let follows_value = attached.is_some_and(|before| match before.kind {
            TokenKind::Word
            | TokenKind::Number
            | TokenKind::String
            | TokenKind::QuotedIdentifier => true,
            _ => matches!(text(before), ":" | ")" | "]" | "}"),
        });
        if !follows_value {
            found.push(Parameter {
                name: text(name).to_owned(),
                range: colon.range.start..name.range.end,
            });
        }
    }
    found
}

/// Parameters in Redis's commands, whose lexer reads a whole argument as one word: an
/// argument that is a `:` and a name, as `MGET :keys`. A key such as `asset:btc` isn't one.
fn redis_parameters(command: &str, tokens: &[Token]) -> Vec<Parameter> {
    tokens
        .iter()
        .filter(|token| token.kind == TokenKind::Word)
        .filter_map(|token| {
            let name = command[token.range.clone()].strip_prefix(':')?;
            let plain =
                starts_name(name) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            plain.then(|| Parameter {
                name: name.to_owned(),
                range: token.range.clone(),
            })
        })
        .collect()
}

fn starts_name(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_alphabetic() || first == '_')
}

fn sql_literal(dialect: Dialect, value: &Literal) -> String {
    let quoting = dialect.quoting();
    match value {
        Literal::Null => "NULL".to_owned(),
        Literal::Number(digits) => digits.clone(),
        // SQL Server has no boolean literal; its `bit` compares with 1 and 0.
        Literal::Boolean(flag) if dialect == Dialect::SqlServer => u8::from(*flag).to_string(),
        Literal::Boolean(flag) => if *flag { "TRUE" } else { "FALSE" }.to_owned(),
        Literal::Text(text) | Literal::Date(text) | Literal::Timestamp(text) => {
            quoting.string_literal(text)
        }
    }
}

fn partiql_literal(value: &Literal) -> String {
    match value {
        Literal::Null => "NULL".to_owned(),
        Literal::Number(digits) => digits.clone(),
        Literal::Boolean(flag) => flag.to_string(),
        Literal::Text(text) | Literal::Date(text) | Literal::Timestamp(text) => {
            format!("'{}'", text.replace('\'', "''"))
        }
    }
}

/// As JSON, but a date or time as the shell's `ISODate`, which compares with a stored date
/// where a string wouldn't.
fn mongo_literal(value: &Literal) -> String {
    match value {
        Literal::Date(date) => format!("ISODate({})", mongo::quote(&format!("{date}T00:00:00Z"))),
        Literal::Timestamp(time) => {
            let iso = time.replacen(' ', "T", 1);
            format!("ISODate({})", mongo::quote(&format!("{iso}Z")))
        }
        other => json_literal(other),
    }
}

fn json_literal(value: &Literal) -> String {
    match value {
        Literal::Null => "null".to_owned(),
        Literal::Number(digits) => digits.clone(),
        Literal::Boolean(flag) => flag.to_string(),
        // The shell's quoting is JSON's: escapes and `\u` for control characters.
        Literal::Text(text) | Literal::Date(text) | Literal::Timestamp(text) => mongo::quote(text),
    }
}

/// An argument: Redis has no types, so every value is its text, and `NULL` an empty one.
fn redis_literal(value: &Literal) -> String {
    match value {
        Literal::Null => "\"\"".to_owned(),
        Literal::Boolean(flag) => flag.to_string(),
        Literal::Number(text)
        | Literal::Text(text)
        | Literal::Date(text)
        | Literal::Timestamp(text) => redis::quote(text.as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(language: QueryLanguage, command: &str) -> Vec<String> {
        language
            .parameters(command)
            .into_iter()
            .map(|parameter| parameter.name)
            .collect()
    }

    #[test]
    fn finds_parameters_in_each_language_but_not_in_strings_comments_or_keys() {
        let cases: [(QueryLanguage, &str, &[&str]); 9] = [
            (
                QueryLanguage::Sql(Dialect::Postgres),
                "SELECT * FROM t WHERE id IN :ids AND d >= :since AND x::int = 1 -- :no",
                &["ids", "since"],
            ),
            (
                QueryLanguage::PartiQl,
                "SELECT * FROM \"t\" WHERE pk IN :ids AND d >= :since -- :no\n AND s = ':no'",
                &["ids", "since"],
            ),
            (
                QueryLanguage::Mongo,
                "db.assets.find({_id: {$in: :ids}, d: {$gte: :since}, s: ':no', a:b}) // :no",
                &["ids", "since"],
            ),
            (
                QueryLanguage::Mongo,
                "db.t.find({a::b, c: [:first, :second]})",
                &["first", "second"],
            ),
            (
                QueryLanguage::Search,
                "GET assets/_search\n{\"query\": {\"terms\": {\"id\": :ids}}, \"s\": \":no\"}",
                &["ids"],
            ),
            (
                QueryLanguage::Redis,
                "MGET :ids asset:abc \":no\" :2x :_ok",
                &["ids", "_ok"],
            ),
            (QueryLanguage::Redis, "HGETALL asset:btc", &[]),
            (
                QueryLanguage::PartiQl,
                "SELECT * FROM t WHERE a = :a1",
                &["a1"],
            ),
            (QueryLanguage::Mongo, "db.t.find({n: :7})", &[]),
        ];
        for (language, command, expected) in cases {
            assert_eq!(names(language, command), expected, "{command}");
        }
    }

    #[test]
    fn parameter_ranges_cover_the_colon_and_the_name() {
        let command = "db.t.find({a: :ids})";
        let found = QueryLanguage::Mongo.parameters(command);
        assert_eq!(found.len(), 1);
        assert_eq!(&command[found[0].range.clone()], ":ids");
        let command = "MGET :keys";
        let found = QueryLanguage::Redis.parameters(command);
        assert_eq!(&command[found[0].range.clone()], ":keys");
    }

    fn sample() -> Vec<Literal> {
        vec![
            Literal::Text("it's \"x\"".to_owned()),
            Literal::Number("-12.50".to_owned()),
            Literal::Boolean(true),
            Literal::Date("2026-10-01".to_owned()),
            Literal::Timestamp("2026-10-01 12:00:00.5".to_owned()),
            Literal::Null,
        ]
    }

    #[test]
    fn writes_literals_and_lists_as_each_language_reads_them() {
        let cases = [
            (
                QueryLanguage::Sql(Dialect::Postgres),
                r#"('it''s "x"', -12.50, TRUE, '2026-10-01', '2026-10-01 12:00:00.5', NULL)"#,
            ),
            (
                QueryLanguage::Sql(Dialect::SqlServer),
                r#"(N'it''s "x"', -12.50, 1, N'2026-10-01', N'2026-10-01 12:00:00.5', NULL)"#,
            ),
            (
                QueryLanguage::Sql(Dialect::Mysql),
                r#"('it''s "x"', -12.50, TRUE, '2026-10-01', '2026-10-01 12:00:00.5', NULL)"#,
            ),
            (
                QueryLanguage::PartiQl,
                r#"['it''s "x"', -12.50, true, '2026-10-01', '2026-10-01 12:00:00.5', NULL]"#,
            ),
            (
                QueryLanguage::Mongo,
                r#"["it's \"x\"", -12.50, true, ISODate("2026-10-01T00:00:00Z"), ISODate("2026-10-01T12:00:00.5Z"), null]"#,
            ),
            (
                QueryLanguage::Search,
                r#"["it's \"x\"", -12.50, true, "2026-10-01", "2026-10-01 12:00:00.5", null]"#,
            ),
            (
                QueryLanguage::Redis,
                r#""it's \"x\"" -12.50 true 2026-10-01 "2026-10-01 12:00:00.5" """#,
            ),
        ];
        for (language, expected) in cases {
            assert_eq!(language.list(&sample()), expected, "{language:?}");
        }
    }

    #[test]
    fn replaces_the_parameters_it_is_given_values_for() {
        let language = QueryLanguage::Sql(Dialect::Postgres);
        let command = "SELECT * FROM t WHERE id IN :ids AND d >= :since AND e IN :ids";
        let list = language.list(&[
            Literal::Number("1".to_owned()),
            Literal::Text("b".to_owned()),
        ]);
        let replaced =
            language.replace_parameters(command, |name| (name == "ids").then(|| list.clone()));
        assert_eq!(
            replaced,
            "SELECT * FROM t WHERE id IN (1, 'b') AND d >= :since AND e IN (1, 'b')"
        );

        let mongo = QueryLanguage::Mongo;
        let replaced = mongo.replace_parameters("db.t.find({d: {$gte: :since}})", |_| {
            Some(mongo.literal(&Literal::Date("2026-10-01".to_owned())))
        });
        assert_eq!(
            replaced,
            "db.t.find({d: {$gte: ISODate(\"2026-10-01T00:00:00Z\")}})"
        );
    }

    #[test]
    fn keeps_key_lists_short_where_the_engine_refuses_long_ones() {
        assert_eq!(QueryLanguage::PartiQl.max_list_values().get(), 100);
        assert_eq!(
            QueryLanguage::Sql(Dialect::Cassandra)
                .max_list_values()
                .get(),
            100
        );
        assert_eq!(
            QueryLanguage::Sql(Dialect::Postgres)
                .max_list_values()
                .get(),
            1_000
        );
        assert_eq!(QueryLanguage::Mongo.max_list_values().get(), 1_000);
    }
}
