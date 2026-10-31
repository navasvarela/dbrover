//! The query languages DbRover's consoles speak, and what each knows about its own commands:
//! how a script splits into them, the tokens to highlight, the words to complete and what a
//! command may change.
//!
//! [`QueryLanguage`] is what a driver speaks. SQL is one language among several, with a
//! [`Dialect`] per engine; the features that only make sense in SQL (formatting, the linter,
//! parameters, explain, the grid's edits, the SQL export formats) ask a language for its
//! dialect and stay hidden when it has none.
//!
//! Code outside this crate asks a language through its methods and never matches on its
//! variants, so a language added for a new engine changes no other crate (see
//! `docs/nosql.md`, N0a).

pub mod mongo;
pub mod redis;
pub mod search;
#[cfg(any(test, feature = "test-language"))]
mod test_language;

use std::ops::Range;

use dbrover_core::{DriverKind, Quoting, ResultShape, ServerInfo};
use dbrover_sql::Dialect;
pub use dbrover_sql::{Analysis, NamedSchemas, StatementKind, Token, TokenKind, Unclassified};

/// How the grid's edits of rows are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditsRun {
    /// In a transaction of the dialect's, all or none.
    Transaction(Dialect),
    /// One command at a time, each taking effect as it runs: Redis and MongoDB have no
    /// transaction to undo the ones before a command that fails.
    OneByOne,
}

/// A command made to show its plan rather than run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    pub text: String,
    /// Where the original command starts in `text`, so an error's position can be moved back
    /// onto it.
    pub prefix_len: usize,
}

/// The language of a data source's console, of the MCP server's statements, and of the
/// commands the safety checks classify.
///
/// A closed enum, like `DriverKind`: see `docs/adr/0002-native-drivers-behind-a-trait.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryLanguage {
    Sql(Dialect),
    /// Redis's commands, for Redis, Valkey and Dragonfly.
    Redis,
    /// A subset of the MongoDB shell's syntax, read by DbRover itself.
    Mongo,
    /// Requests to Elasticsearch's and OpenSearch's REST APIs, as Kibana's Dev Tools write them.
    Search,
    /// A language of one command per line, built only for tests: it has no dialect, so it runs
    /// through the paths a non-SQL engine will take.
    #[cfg(any(test, feature = "test-language"))]
    Test,
}

impl QueryLanguage {
    /// The language's name in messages.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Sql(dialect) => dialect.display_name(),
            Self::Redis => "Redis",
            Self::Mongo => "MongoDB",
            Self::Search => "Elasticsearch",
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => "the test language",
        }
    }

    /// The SQL dialect, for features with no meaning outside SQL. `None` hides them.
    pub fn dialect(self) -> Option<Dialect> {
        match self {
            Self::Sql(dialect) => Some(dialect),
            Self::Redis | Self::Mongo | Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// How the grid's edits are applied, for a language whose results the grid can edit;
    /// `None` for one whose results it can't.
    pub fn edits_run(self) -> Option<EditsRun> {
        match self {
            Self::Sql(dialect) => Some(EditsRun::Transaction(dialect)),
            Self::Redis | Self::Mongo => Some(EditsRun::OneByOne),
            Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// The key `command` reads whole, which the grid edits a command at a time and the key bar
    /// acts on; `None` for any other command, and in a language without keys.
    pub fn key_view(self, command: &str) -> Option<redis::KeyView> {
        match self {
            Self::Redis => redis::KeyView::of(command),
            Self::Sql(_) | Self::Mongo | Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// Whether the language has a way to show a command's plan.
    pub fn can_explain(self) -> bool {
        self.explain_style().is_some()
    }

    /// `command` made to show its plan rather than run, on `server`: SQL's `EXPLAIN` prefix,
    /// or a MongoDB `find` or `aggregate` with `.explain("executionStats")` after it. `None`
    /// for a command, or a language, that has no plan to show.
    pub fn explain(self, command: &str, server: Option<&ServerInfo>) -> Option<Explained> {
        match self.explain_style()? {
            ExplainStyle::Prefix(dialect) => {
                let prefix = dialect.explain_prefix(server);
                Some(Explained {
                    text: format!("{prefix}{command}"),
                    prefix_len: prefix.len(),
                })
            }
            ExplainStyle::MongoShell => mongo::explain(command).map(|text| Explained {
                text,
                prefix_len: 0,
            }),
        }
    }

    fn explain_style(self) -> Option<ExplainStyle> {
        match self {
            Self::Sql(dialect) => Some(ExplainStyle::Prefix(dialect)),
            Self::Mongo => Some(ExplainStyle::MongoShell),
            Self::Redis | Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// What `command` returns, as Run to file must know before it runs: rows for SQL, and
    /// documents for a MongoDB `find` or `aggregate`. `None` for a command whose result DbRover
    /// doesn't write straight to a file.
    pub fn result_shape(self, command: &str) -> Option<ResultShape> {
        match self {
            Self::Sql(_) => Some(ResultShape::Rows),
            Self::Mongo => mongo::returns_documents(command).then_some(ResultShape::Documents),
            Self::Redis | Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// The collection `command` reads, when its documents can be edited in the grid a command
    /// at a time: a MongoDB `find` on one collection. `None` for any other command, and in
    /// another language.
    pub fn collection_view(self, command: &str) -> Option<mongo::CollectionView> {
        match self {
            Self::Mongo => mongo::CollectionView::of(command),
            Self::Sql(_) | Self::Redis | Self::Search => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// How names are quoted when DbRover writes them in a message: the dialect's way, or
    /// standard SQL's for a language without one.
    pub fn quoting(self) -> Quoting {
        self.dialect().map_or(Quoting::STANDARD, Dialect::quoting)
    }

    /// Splits a console's text into the commands DbRover sends one at a time.
    ///
    /// Each range covers one command from its first to its last meaningful token, as
    /// [`dbrover_sql::split_statements`] does for SQL: surrounding whitespace, comments and
    /// terminators are left out, and a stretch with no command in it is dropped.
    pub fn split(self, script: &str) -> Vec<Range<usize>> {
        match self {
            Self::Sql(dialect) => dbrover_sql::split_statements(script, dialect),
            Self::Redis => redis::split(script),
            Self::Mongo => mongo::split(script),
            Self::Search => search::split(script),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::split(script),
        }
    }

    /// Splits `text` into tokens that together cover every byte of it, in order: what the
    /// editor highlights and the splitter reads.
    pub fn tokens(self, text: &str) -> Vec<Token> {
        match self {
            Self::Sql(dialect) => dbrover_sql::tokenize(text, dialect),
            Self::Redis => redis::tokenize(text),
            Self::Mongo => mongo::tokenize(text),
            Self::Search => search::tokenize(text),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::tokenize(text),
        }
    }

    /// Whether `word`, a [`TokenKind::Word`], is highlighted as one of the language's own
    /// words: ignoring case, except in MongoDB's case-sensitive shell and a search request's
    /// endpoints.
    pub fn is_keyword(self, word: &str) -> bool {
        match self {
            Self::Sql(_) => dbrover_sql::is_keyword(word),
            Self::Redis => redis::is_command(word),
            Self::Mongo => mongo::is_keyword(word),
            Self::Search => search::is_keyword(word),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::is_keyword(word),
        }
    }

    /// The language's own words, sorted, for completion: lowercase, except where
    /// [`Self::completion_keeps_case`] says they are written as listed.
    pub fn completion_words(self) -> &'static [&'static str] {
        match self {
            Self::Sql(_) => dbrover_sql::keywords(),
            Self::Redis => redis::completion_words(),
            Self::Mongo => mongo::completion_words(),
            Self::Search => search::completion_words(),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::KEYWORDS,
        }
    }

    /// Whether completion inserts the language's words as listed, whatever the case typed:
    /// MongoDB's methods and operators are case-sensitive (`insertOne`), and so are a search
    /// request's endpoints and keys (`_search`, `match_all`), while SQL's keywords and Redis's
    /// commands follow the case being typed.
    pub fn completion_keeps_case(self) -> bool {
        matches!(self, Self::Mongo | Self::Search)
    }

    /// What completion shows beside `word`, one of [`Self::completion_words`]: the arguments
    /// a Redis command or a MongoDB method takes, or how a search endpoint is called. `None`
    /// where the language has nothing to add.
    pub fn completion_detail(self, word: &str) -> Option<&'static str> {
        match self {
            Self::Redis => redis::syntax(word),
            Self::Mongo => mongo::syntax(word),
            Self::Search => search::syntax(word),
            Self::Sql(_) => None,
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => None,
        }
    }

    /// Why `command` may touch everything, which is rarely what was meant, for the warning
    /// before it runs: an `UPDATE` or `DELETE` without `WHERE` in SQL. `None` for any other
    /// command. Each language lists its own.
    pub fn hazard(self, command: &str) -> Option<&'static str> {
        match self {
            Self::Sql(dialect) => {
                dbrover_sql::hazard(command, dialect).map(dbrover_sql::Hazard::description)
            }
            Self::Redis => redis::hazard(command),
            Self::Mongo => mongo::hazard(command),
            Self::Search => search::hazard(command),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::hazard(command),
        }
    }

    /// What `command`, which must hold exactly one command, may change.
    ///
    /// # Errors
    ///
    /// [`Unclassified`] when the text holds no command, several, or one the language can't
    /// read. What such a command does is unknown, and the safety checks refuse it.
    pub fn classify(self, command: &str) -> Result<StatementKind, Unclassified> {
        self.analyze(command).map(|analysis| analysis.kind)
    }

    /// What `command` may change, as [`Self::classify`] says, and the schemas it names, for
    /// the MCP server's allow-list. A language without schemas names none it can be sure of,
    /// which the allow-list treats as unknown.
    ///
    /// # Errors
    ///
    /// [`Unclassified`], as [`Self::classify`].
    pub fn analyze(self, command: &str) -> Result<Analysis, Unclassified> {
        match self {
            Self::Sql(dialect) => dbrover_sql::analyze(command, dialect),
            Self::Redis => redis::classify(command).map(|kind| Analysis {
                kind,
                schemas: NamedSchemas::Unknown,
            }),
            Self::Mongo => mongo::classify(command).map(|kind| Analysis {
                kind,
                schemas: NamedSchemas::Unknown,
            }),
            Self::Search => search::classify(command).map(|kind| Analysis {
                kind,
                schemas: NamedSchemas::Unknown,
            }),
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => test_language::classify(command).map(|kind| Analysis {
                kind,
                schemas: NamedSchemas::Unknown,
            }),
        }
    }
}

/// How a language shows a command's plan.
#[derive(Debug, Clone, Copy)]
enum ExplainStyle {
    /// A prefix before the statement, the dialect's own.
    Prefix(Dialect),
    /// `.explain("executionStats")` after a `find` or `aggregate`.
    MongoShell,
}

impl From<DriverKind> for QueryLanguage {
    fn from(driver: DriverKind) -> Self {
        match driver {
            DriverKind::Postgres => Self::Sql(Dialect::Postgres),
            DriverKind::Sqlite => Self::Sql(Dialect::Sqlite),
            DriverKind::Mysql => Self::Sql(Dialect::Mysql),
            DriverKind::SqlServer => Self::Sql(Dialect::SqlServer),
            DriverKind::DuckDb => Self::Sql(Dialect::DuckDb),
            DriverKind::ClickHouse => Self::Sql(Dialect::ClickHouse),
            DriverKind::Redis => Self::Redis,
            DriverKind::Mongo => Self::Mongo,
            DriverKind::Elasticsearch => Self::Search,
        }
    }
}

/// The index of the command to run for a cursor at byte offset `cursor`, among `commands` as
/// [`QueryLanguage::split`] gives them: the command containing it, else the nearest one before
/// it, else the first one after it. The same for every language.
pub fn command_at(commands: &[Range<usize>], cursor: usize) -> Option<usize> {
    dbrover_sql::statement_at(commands, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sql_driver_speaks_its_own_dialect() {
        let dialects: Vec<Option<Dialect>> = DriverKind::ALL
            .into_iter()
            .map(|driver| QueryLanguage::from(driver).dialect())
            .collect();
        assert_eq!(
            dialects,
            [
                Some(Dialect::Postgres),
                Some(Dialect::Sqlite),
                Some(Dialect::Mysql),
                Some(Dialect::SqlServer),
                Some(Dialect::DuckDb),
                Some(Dialect::ClickHouse),
                None,
                None,
                None,
            ]
        );
        assert_eq!(QueryLanguage::from(DriverKind::Redis), QueryLanguage::Redis);
        assert_eq!(QueryLanguage::from(DriverKind::Mongo), QueryLanguage::Mongo);
        assert_eq!(
            QueryLanguage::from(DriverKind::Elasticsearch),
            QueryLanguage::Search
        );
    }

    #[test]
    fn sql_splits_and_classifies_as_dbrover_sql_does() {
        let language = QueryLanguage::Sql(Dialect::Postgres);
        let script = "select 1;\n-- note\ndelete from t";
        let commands = language.split(script);
        assert_eq!(
            commands,
            dbrover_sql::split_statements(script, Dialect::Postgres)
        );
        let kinds: Vec<_> = commands
            .iter()
            .map(|range| language.classify(&script[range.clone()]))
            .collect();
        assert_eq!(kinds, [Ok(StatementKind::Read), Ok(StatementKind::Write)]);
        assert_eq!(command_at(&commands, script.len()), Some(1));
    }

    #[test]
    fn sql_warns_of_what_dbrover_sql_does() {
        let language = QueryLanguage::Sql(Dialect::Mysql);
        assert_eq!(
            language.hazard("DELETE FROM orders"),
            Some(dbrover_sql::Hazard::DeleteWithoutWhere.description())
        );
        assert_eq!(language.hazard("DELETE FROM orders WHERE id = 1"), None);
    }

    #[test]
    fn the_test_language_has_no_dialect() {
        assert_eq!(QueryLanguage::Test.dialect(), None);
    }
}
