//! Everything that differs between SQL dialects, in one place: the lexer's rules, how scripts
//! split into statements, the `sqlparser` dialect, the built-in functions, how to explain a
//! statement, quoting and the catalog's system names.
//!
//! The rest of the crate asks a [`Dialect`] for these facts instead of matching on it, so a
//! new dialect is a new variant here, its rows in each method and, if its lexical rules are
//! new, the lexer code that applies them.

mod capabilities;
mod functions;
mod value_kind;

pub use capabilities::{
    Capabilities, ForeignKeySwitch, SchemaCreation, TableChange, UserModel, ViewReplacement,
};

use dbrover_core::{Quoting, ServerInfo};
use dbrover_plan::PlanFormat;
use sqlparser::dialect::{
    ClickHouseDialect, DuckDbDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
};

use crate::ParameterBinding;

/// The SQL flavour of a data source.
///
/// A closed enum, like `DriverKind`: see `docs/adr/0002-native-drivers-behind-a-trait.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Sqlite,
    /// MySQL and MariaDB, which one driver serves; their differences depend on the server's
    /// version, which only a connection knows.
    Mysql,
    /// Microsoft SQL Server and Azure SQL: Transact-SQL.
    SqlServer,
    /// DuckDB, whose parser derives from PostgreSQL's.
    DuckDb,
    ClickHouse,
    /// CQL, the Cassandra Query Language, for Apache Cassandra and ScyllaDB. It lexes and
    /// splits like SQL, but `sqlparser` has no dialect for it, so its statements are classified
    /// by a reader of their own (`classify::cql`) and checked by the server alone.
    Cassandra,
}

/// The lexical rules that differ between dialects. Strings in `'…'`, `--` line comments and
/// `/* … */` block comments are common to every dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LexicalRules {
    /// `E'…'` strings, in which a backslash escapes the next character.
    pub escape_strings: bool,
    /// Whether a backslash escapes the next character in every `'…'` (and `"…"`) string, as
    /// in MySQL's default SQL mode.
    pub backslash_escapes: bool,
    /// Whether `"…"` is a string, as in MySQL without `ANSI_QUOTES`, rather than an
    /// identifier.
    pub double_quoted_strings: bool,
    /// `$tag$ … $tag$` strings.
    pub dollar_quoting: bool,
    /// `$` inside identifiers after their first character.
    pub dollar_in_identifiers: bool,
    /// Whether `/* /* */ */` is one comment, rather than ending at the first `*/`.
    pub nested_block_comments: bool,
    /// `# …` line comments.
    pub hash_comments: bool,
    /// `// …` line comments.
    pub slash_comments: bool,
    /// Whether `--` starts a comment only before whitespace, as in MySQL, where `1--1` is
    /// arithmetic.
    pub dash_comments_need_space: bool,
    /// `` `backtick` `` identifiers.
    pub backtick_identifiers: bool,
    /// `[bracketed]` identifiers, in which `]]` stands for `]`.
    pub bracket_identifiers: bool,
    /// `N'…'` strings of national (Unicode) characters, one token like any string.
    pub national_strings: bool,
    /// Transact-SQL's names that start with `@` (variables), `@@` (system functions), `#` or
    /// `##` (temporary tables), and may hold `@`, `#` and `$` after their first character.
    pub sigil_identifiers: bool,
}

/// How a script divides into the statements DbRover sends one at a time, beyond splitting at
/// top-level semicolons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScriptRules {
    /// The MySQL client's `DELIMITER //` command, which changes the statement terminator so
    /// that a procedure's body can hold semicolons.
    pub delimiter_command: bool,
    /// `GO` alone on a line ends a batch, as in SQL Server's own tools. A procedure, function
    /// or trigger's body then runs to the end of its batch, as the server reads it.
    pub go_batches: bool,
    /// Whether `BEGIN … END` blocks (whose semicolons don't split) can appear in any
    /// statement, as Transact-SQL's control flow, rather than only in a `CREATE` statement's
    /// body.
    pub blocks_anywhere: bool,
    /// CQL's `BEGIN [UNLOGGED | COUNTER] BATCH … APPLY BATCH`, one statement whose semicolons
    /// don't split. Its only block: CQL has no `BEGIN … END` or `CASE … END`, and `end` may name
    /// a column.
    pub batches: bool,
}

/// The statements that mark a point inside an open transaction, so that a group of changes can
/// be undone without ending it: what `keep` and `undo` run after the changes succeed or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Savepoint {
    pub open: &'static str,
    pub keep: &'static [&'static str],
    pub undo: &'static [&'static str],
}

/// A session setting that makes the server describe statements instead of running them,
/// switched on before the statement in a batch of its own and off again after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplainSetting {
    pub on: &'static str,
    pub off: &'static str,
}

/// How far a plan goes: what the database expects, or also what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainDepth {
    /// The database's estimates, without running the statement.
    Estimated,
    /// The estimates and what each step did, by running the statement: `EXPLAIN ANALYZE`.
    Analysed,
}

/// How to have the database explain a statement with a plan DbRover can draw as a tree, and
/// what it answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanRequest {
    /// What to put before the statement.
    pub prefix: &'static str,
    /// The format of the one value the database answers with.
    pub format: PlanFormat,
}

/// A construct `sqlparser` rejects although the dialect's database accepts it. A statement
/// with one isn't syntax-checked, because a false error is worse than a missed one; the
/// check that finds each kind is in `check::syntax`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParserGap {
    /// A sequence of words, compared lowercase.
    Words(&'static [&'static str]),
    /// PostgreSQL's XML functions, which take keyword arguments: `xmlelement(name foo)`.
    XmlFunctionArguments,
    /// `JOIN t USING (id) AS j` (PostgreSQL 14), which names the join's columns.
    JoinUsingAlias,
    /// `a IS b` with any expression after `IS`, which SQLite accepts.
    IsAnyExpression,
    /// A keyword used as an infix operator, `a MOD 3`, rather than as the function of the
    /// same name, `MOD(a, 3)`.
    InfixOperator(&'static str),
    /// An operator of several characters written together, such as DuckDB's `**`; the lexer
    /// reads each character as a token of its own.
    Operator(&'static str),
    /// A tuple's element by position, `t.2` or `tuple(1, 'a').2`: a number starting with a dot,
    /// written straight after what it indexes.
    ElementByPosition,
    /// ClickHouse's `WITH 5 AS n`, which names an expression, rather than the standard
    /// `WITH n AS (SELECT …)`.
    WithExpressionAlias,
}

impl Dialect {
    pub const ALL: [Self; 7] = [
        Self::Postgres,
        Self::Sqlite,
        Self::Mysql,
        Self::SqlServer,
        Self::DuckDb,
        Self::ClickHouse,
        Self::Cassandra,
    ];

    /// The dialect's name in messages.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL",
            Self::SqlServer => "SQL Server",
            Self::DuckDb => "DuckDB",
            Self::ClickHouse => "ClickHouse",
            Self::Cassandra => "CQL",
        }
    }

    pub(crate) fn lexical_rules(self) -> LexicalRules {
        match self {
            // DuckDB reads text with PostgreSQL's lexer: checked against DuckDB 1.5.6 for
            // dollar quoting, nested comments, `E'…'`, `N'…'` and `$` inside names.
            Self::Postgres | Self::DuckDb => LexicalRules {
                escape_strings: true,
                backslash_escapes: false,
                double_quoted_strings: false,
                dollar_quoting: true,
                dollar_in_identifiers: true,
                nested_block_comments: true,
                hash_comments: false,
                slash_comments: false,
                dash_comments_need_space: false,
                backtick_identifiers: false,
                bracket_identifiers: false,
                national_strings: true,
                sigil_identifiers: false,
            },
            Self::Sqlite => LexicalRules {
                escape_strings: false,
                backslash_escapes: false,
                double_quoted_strings: false,
                dollar_quoting: false,
                dollar_in_identifiers: false,
                nested_block_comments: false,
                hash_comments: false,
                slash_comments: false,
                dash_comments_need_space: false,
                backtick_identifiers: true,
                bracket_identifiers: true,
                national_strings: false,
                sigil_identifiers: false,
            },
            Self::Mysql => LexicalRules {
                escape_strings: false,
                backslash_escapes: true,
                double_quoted_strings: true,
                dollar_quoting: false,
                dollar_in_identifiers: true,
                nested_block_comments: false,
                hash_comments: true,
                slash_comments: false,
                dash_comments_need_space: true,
                backtick_identifiers: true,
                bracket_identifiers: false,
                national_strings: true,
                sigil_identifiers: false,
            },
            // `"…"` is an identifier while `QUOTED_IDENTIFIER` is on, as every client library
            // sets it and the driver does on connecting.
            // Checked against ClickHouse 25.8: `#` comments, nested block comments, dollar
            // quoting, backslash escapes, and `--` starting a comment even in `1--1`.
            Self::ClickHouse => LexicalRules {
                escape_strings: false,
                backslash_escapes: true,
                double_quoted_strings: false,
                dollar_quoting: true,
                dollar_in_identifiers: true,
                nested_block_comments: true,
                hash_comments: true,
                slash_comments: false,
                dash_comments_need_space: false,
                backtick_identifiers: true,
                bracket_identifiers: false,
                national_strings: false,
                sigil_identifiers: false,
            },
            Self::SqlServer => LexicalRules {
                escape_strings: false,
                backslash_escapes: false,
                double_quoted_strings: false,
                dollar_quoting: false,
                dollar_in_identifiers: true,
                nested_block_comments: true,
                hash_comments: false,
                slash_comments: false,
                dash_comments_need_space: false,
                backtick_identifiers: false,
                bracket_identifiers: true,
                national_strings: true,
                sigil_identifiers: true,
            },
            // Checked against ScyllaDB 2026.3 and Cassandra 5.0: `//` comments, `$$` strings
            // without a tag, `''` for a quote and no backslash escapes, and block comments
            // that end at the first `*/`.
            Self::Cassandra => LexicalRules {
                escape_strings: false,
                backslash_escapes: false,
                double_quoted_strings: false,
                dollar_quoting: true,
                dollar_in_identifiers: false,
                nested_block_comments: false,
                hash_comments: false,
                slash_comments: true,
                dash_comments_need_space: false,
                backtick_identifiers: false,
                bracket_identifiers: false,
                national_strings: false,
                sigil_identifiers: false,
            },
        }
    }

    /// The client-side commands and blocks that decide where statements end: MySQL's
    /// `DELIMITER`, as the `mysql` client and every MySQL tool accept it, and SQL Server's
    /// `GO`, as SQL Server Management Studio and `sqlcmd` do.
    pub(crate) fn script_rules(self) -> ScriptRules {
        match self {
            Self::Postgres | Self::Sqlite | Self::DuckDb | Self::ClickHouse => ScriptRules {
                delimiter_command: false,
                go_batches: false,
                blocks_anywhere: false,
                batches: false,
            },
            Self::Mysql => ScriptRules {
                delimiter_command: true,
                go_batches: false,
                blocks_anywhere: false,
                batches: false,
            },
            Self::SqlServer => ScriptRules {
                delimiter_command: false,
                go_batches: true,
                blocks_anywhere: true,
                batches: false,
            },
            Self::Cassandra => ScriptRules {
                delimiter_command: false,
                go_batches: false,
                blocks_anywhere: false,
                batches: true,
            },
        }
    }

    /// The dialect `sqlparser` parses this one with, for the syntax check and for reading the
    /// columns of a query; `None` for CQL, which it has no dialect for.
    pub(crate) fn sqlparser_dialect(self) -> Option<&'static dyn sqlparser::dialect::Dialect> {
        Some(match self {
            Self::Postgres => &PostgreSqlDialect {},
            Self::Sqlite => &SQLiteDialect {},
            Self::Mysql => &MySqlDialect {},
            Self::SqlServer => &MsSqlDialect {},
            Self::DuckDb => &DuckDbDialect {},
            Self::ClickHouse => &ClickHouseDialect {},
            Self::Cassandra => return None,
        })
    }

    /// What `sqlparser` 0.63 rejects in valid statements of this dialect. The list came from
    /// running a corpus of valid statements through the parser (`check::tests`); a statement
    /// the parser wrongly rejects is a bug to add here.
    pub(crate) fn parser_gaps(self) -> &'static [ParserGap] {
        match self {
            Self::Postgres => &[
                // `SELECT * FROM ROWS FROM (f(), g())`
                ParserGap::Words(&["rows", "from"]),
                // `FOR NO KEY UPDATE`, `FOR KEY SHARE`
                ParserGap::Words(&["no", "key"]),
                ParserGap::Words(&["key", "share"]),
                ParserGap::Words(&["between", "symmetric"]),
                ParserGap::XmlFunctionArguments,
                ParserGap::JoinUsingAlias,
            ],
            Self::Sqlite => &[
                // `FROM t INDEXED BY i`, `FROM t NOT INDEXED`
                ParserGap::Words(&["indexed", "by"]),
                ParserGap::Words(&["not", "indexed"]),
                ParserGap::IsAnyExpression,
            ],
            Self::Mysql => &[
                ParserGap::Words(&["sounds", "like"]),
                ParserGap::Words(&["lock", "in", "share", "mode"]),
                // `UPDATE LOW_PRIORITY IGNORE t`, `DELETE QUICK IGNORE FROM t`
                ParserGap::Words(&["low_priority", "ignore"]),
                ParserGap::Words(&["quick", "ignore"]),
                ParserGap::InfixOperator("mod"),
            ],
            Self::SqlServer => &[
                // `… OPTION (MAXRECURSION 10)`: query hints.
                ParserGap::Words(&["option"]),
                // `INSERT INTO t EXEC p`
                ParserGap::Words(&["exec"]),
                ParserGap::Words(&["execute"]),
                ParserGap::Words(&["delete", "top"]),
            ],
            Self::ClickHouse => &[
                ParserGap::ElementByPosition,
                ParserGap::WithExpressionAlias,
                ParserGap::Words(&["global", "in"]),
                ParserGap::Words(&["global", "not", "in"]),
            ],
            Self::DuckDb => &[
                // `JOIN … ON t.ts >= q.ts`, a join on the nearest earlier row
                ParserGap::Words(&["asof", "join"]),
                // `FROM t USING SAMPLE 10%`
                ParserGap::Words(&["using", "sample"]),
                // `INSERT INTO t BY NAME SELECT …`; with `select` after it, so that `ORDER BY
                // name` is still checked.
                ParserGap::Words(&["by", "name", "select"]),
                ParserGap::Operator("**"),
            ],
            Self::Cassandra => &[],
        }
    }

    /// First words of statements that insert rows: `insert`, and SQLite's and MySQL's
    /// `replace` (which deletes a row with the same key first).
    pub(crate) fn insert_verbs(self) -> &'static [&'static str] {
        match self {
            Self::Postgres
            | Self::SqlServer
            | Self::DuckDb
            | Self::ClickHouse
            | Self::Cassandra => &["insert"],
            Self::Sqlite | Self::Mysql => &["insert", "replace"],
        }
    }

    /// First words of statements that commit the open transaction before they run, and
    /// commit themselves at once, so `ROLLBACK` undoes neither: MySQL's DDL and account
    /// statements. PostgreSQL, SQLite and SQL Server run DDL inside the transaction.
    pub(crate) fn implicit_commit_verbs(self) -> &'static [&'static str] {
        match self {
            Self::Postgres
            | Self::Sqlite
            | Self::SqlServer
            | Self::DuckDb
            | Self::ClickHouse
            | Self::Cassandra => &[],
            Self::Mysql => &[
                "alter", "create", "drop", "grant", "rename", "revoke", "truncate",
            ],
        }
    }

    /// What to put before a statement to have the database describe how it would run it,
    /// without running it: `EXPLAIN` on PostgreSQL, `EXPLAIN QUERY PLAN` on SQLite (whose
    /// plain `EXPLAIN` lists virtual machine opcodes instead). On MySQL 8.0.16 and later,
    /// `EXPLAIN FORMAT=TREE`, which reads like PostgreSQL's plan; MariaDB, older MySQL, and a
    /// console that hasn't connected yet (`server` is `None`) get plain `EXPLAIN`, which they
    /// all accept. Nothing on SQL Server, which has no such prefix: see
    /// [`explain_setting`](Self::explain_setting).
    pub fn explain_prefix(self, server: Option<&ServerInfo>) -> &'static str {
        match self {
            Self::Postgres | Self::DuckDb | Self::ClickHouse => "EXPLAIN ",
            Self::Sqlite => "EXPLAIN QUERY PLAN ",
            Self::Mysql if server.is_some_and(has_tree_explain) => "EXPLAIN FORMAT=TREE ",
            Self::Mysql => "EXPLAIN ",
            Self::SqlServer | Self::Cassandra => "",
        }
    }

    /// Whether the database can describe how it would run a statement. CQL has no `EXPLAIN`:
    /// a query's partitions are its plan.
    pub fn explains(self) -> bool {
        self != Self::Cassandra
    }

    /// How to ask the database for a plan DbRover draws as a tree, for a console connected to
    /// `server` (`None` before it has connected):
    ///
    /// - PostgreSQL: `EXPLAIN (FORMAT JSON)`, with `ANALYZE` when analysed.
    /// - SQL Server: no prefix; [`explain_setting`](Self::explain_setting) has it answer with
    ///   its XML showplan, estimated or actual.
    /// - MySQL and MariaDB: `EXPLAIN FORMAT=JSON`. Analysed, `EXPLAIN ANALYZE FORMAT=JSON` on
    ///   MySQL 8.3 and later, where [`explain_setting`](Self::explain_setting) switches on the
    ///   iterator format that `EXPLAIN ANALYZE` needs, and MariaDB's `ANALYZE FORMAT=JSON`.
    ///   Older MySQL, and a console not connected yet, only get the estimate.
    ///
    /// `None` where the database's plan is text without costs to weigh, or where DbRover
    /// doesn't read its format yet: [`explain_prefix`](Self::explain_prefix) then explains the
    /// statement as rows, and it can't be analysed.
    pub fn plan_request(
        self,
        depth: ExplainDepth,
        server: Option<&ServerInfo>,
    ) -> Option<PlanRequest> {
        let prefix = match (self, depth) {
            (Self::Postgres, ExplainDepth::Estimated) => "EXPLAIN (FORMAT JSON) ",
            (Self::Postgres, ExplainDepth::Analysed) => "EXPLAIN (ANALYZE, FORMAT JSON) ",
            (Self::Mysql, ExplainDepth::Estimated) => "EXPLAIN FORMAT=JSON ",
            (Self::Mysql, ExplainDepth::Analysed) => match server.map(mysql_flavour) {
                Some(MysqlFlavour::MariaDb) => "ANALYZE FORMAT=JSON ",
                Some(MysqlFlavour::Iterators) => "EXPLAIN ANALYZE FORMAT=JSON ",
                Some(MysqlFlavour::QueryBlocks) | None => return None,
            },
            (Self::SqlServer, _) => "",
            (Self::Sqlite | Self::DuckDb | Self::ClickHouse | Self::Cassandra, _) => return None,
        };
        let format = match self {
            Self::Mysql => PlanFormat::MysqlJson,
            Self::SqlServer => PlanFormat::SqlServerXml,
            _ => PlanFormat::PostgresJson,
        };
        Some(PlanRequest { prefix, format })
    }

    /// Whether the database has transactions a console can open and end. ClickHouse's are
    /// experimental and off unless the server is configured for them, so a ClickHouse console
    /// has no manual commit mode. CQL has none: a batch applies its statements together, and a
    /// lightweight transaction is one statement's condition.
    pub fn has_transactions(self) -> bool {
        !matches!(self, Self::ClickHouse | Self::Cassandra)
    }

    /// Opens a transaction. SQL Server needs the keyword: a bare `BEGIN` opens a block there.
    pub fn begin_transaction(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite | Self::Mysql | Self::DuckDb => "BEGIN",
            Self::SqlServer | Self::ClickHouse | Self::Cassandra => "BEGIN TRANSACTION",
        }
    }

    /// A condition that holds when `value`, written as text in lowercase, contains `needle`
    /// (a string literal, already lowercase). Each dialect has its own way to make any value
    /// text and to find text in text; finding rather than `LIKE` means no character in the
    /// needle is a wildcard.
    pub fn contains_text(self, value: &str, needle: &str) -> String {
        match self {
            Self::Postgres => format!("strpos(lower(CAST({value} AS text)), {needle}) > 0"),
            Self::Sqlite => format!("instr(lower(CAST({value} AS TEXT)), {needle}) > 0"),
            Self::Mysql => format!("LOCATE({needle}, LOWER(CAST({value} AS CHAR))) > 0"),
            Self::SqlServer => {
                format!("CHARINDEX({needle}, LOWER(CAST({value} AS NVARCHAR(MAX)))) > 0")
            }
            Self::DuckDb => format!("strpos(lower(CAST({value} AS VARCHAR)), {needle}) > 0"),
            Self::ClickHouse => {
                format!("positionUTF8(lowerUTF8(toString({value})), {needle}) > 0")
            }
            // Never asked for: CQL can't select from a query, so its rows are never arranged
            // in the database (`arrange::can_arrange`). `LIKE` is the nearest it has, and only
            // on a column with a SASI or vector index.
            Self::Cassandra => format!("{value} LIKE {needle}"),
        }
    }

    /// Why rows can't be imported from a file here, or `None` where they can. An import runs
    /// in one transaction, so that a failure undoes it, and ClickHouse and CQL have none.
    pub fn import_refusal(self) -> Option<&'static str> {
        match self {
            Self::ClickHouse => Some(
                "ClickHouse has no transactions, so an import that failed halfway couldn't be undone",
            ),
            Self::Cassandra => {
                Some("CQL has no transactions, so an import that failed halfway couldn't be undone")
            }
            Self::Postgres | Self::Sqlite | Self::Mysql | Self::SqlServer | Self::DuckDb => None,
        }
    }

    /// What follows `INSERT INTO table` to add a row with every column at its default. MySQL
    /// and MariaDB have no `DEFAULT VALUES`, but read an empty column list as all defaults.
    /// ClickHouse is grouped with them, though the grid never inserts there (see
    /// [`read_only_results`](Self::read_only_results)).
    pub fn default_row(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite | Self::SqlServer | Self::DuckDb => "DEFAULT VALUES",
            // CQL has neither, and its grid inserts name every key column (N4·5).
            Self::Mysql | Self::ClickHouse | Self::Cassandra => "() VALUES ()",
        }
    }

    /// Why the grid never edits this database's rows, or `None` where it may. A ClickHouse
    /// table has no key that tells its rows apart (its `PRIMARY KEY` is its sort order and
    /// may repeat), and its `UPDATE` rewrites data in the background rather than change a row
    /// at once.
    pub fn read_only_results(self) -> Option<&'static str> {
        match self {
            Self::ClickHouse => Some(
                "ClickHouse tables have no key that tells their rows apart, so rows can't be edited here",
            ),
            // TODO(juan): N4·5 edits CQL rows by their primary key, one statement at a time.
            Self::Cassandra => Some("Editing CQL rows in the grid isn't available yet"),
            Self::Postgres | Self::Sqlite | Self::Mysql | Self::SqlServer | Self::DuckDb => None,
        }
    }

    /// The savepoint grid edits use inside a transaction the console already has open, and an
    /// import around each batch of rows, to undo it alone and find the row that failed. SQL
    /// Server writes it `SAVE TRANSACTION` and has no way to release one: it lasts until the
    /// transaction ends. DuckDB has no savepoints at all, so `None`.
    pub fn edits_savepoint(self) -> Option<Savepoint> {
        Some(match self {
            Self::DuckDb | Self::ClickHouse | Self::Cassandra => return None,
            Self::Postgres | Self::Sqlite | Self::Mysql => Savepoint {
                open: "SAVEPOINT dbrover_edits",
                keep: &["RELEASE SAVEPOINT dbrover_edits"],
                undo: &[
                    "ROLLBACK TO SAVEPOINT dbrover_edits",
                    "RELEASE SAVEPOINT dbrover_edits",
                ],
            },
            Self::SqlServer => Savepoint {
                open: "SAVE TRANSACTION dbrover_edits",
                keep: &[],
                undo: &["ROLLBACK TRANSACTION dbrover_edits"],
            },
        })
    }

    /// The setting to switch on around a statement, after its prefix, to have it explained to
    /// `depth` on `server`, which is how the database the engine is connected to answers:
    ///
    /// - SQL Server's `SHOWPLAN_XML`, which answers with the estimated plan as one XML document
    ///   and must be set in a batch of its own, or, analysed, `STATISTICS XML`, which runs the
    ///   statement and follows its results with its actual plan, which the driver reads in
    ///   their place.
    /// - MySQL 8.3 and later's `explain_json_format_version = 2`, for plans as a tree of the
    ///   iterators MySQL runs, which `EXPLAIN ANALYZE FORMAT=JSON` requires. It is a session
    ///   variable, so its value is kept in a user variable and put back afterwards.
    ///
    /// `None` where the prefix alone explains.
    pub fn explain_setting(
        self,
        depth: ExplainDepth,
        server: Option<&ServerInfo>,
    ) -> Option<ExplainSetting> {
        match (self, depth) {
            (Self::SqlServer, ExplainDepth::Estimated) => Some(ExplainSetting {
                on: "SET SHOWPLAN_XML ON",
                off: "SET SHOWPLAN_XML OFF",
            }),
            (Self::SqlServer, ExplainDepth::Analysed) => Some(ExplainSetting {
                on: "SET STATISTICS XML ON",
                off: "SET STATISTICS XML OFF",
            }),
            (Self::Mysql, _) if server.map(mysql_flavour) == Some(MysqlFlavour::Iterators) => {
                Some(ExplainSetting {
                    on: "SET @dbrover_explain_json_format_version = \
                         @@SESSION.explain_json_format_version, \
                         @@SESSION.explain_json_format_version = 2",
                    off: "SET @@SESSION.explain_json_format_version = \
                          @dbrover_explain_json_format_version",
                })
            }
            (
                Self::Postgres
                | Self::Sqlite
                | Self::Mysql
                | Self::DuckDb
                | Self::ClickHouse
                | Self::Cassandra,
                _,
            ) => None,
        }
    }

    /// The built-in functions completion offers: the common ones, not every function the
    /// database has (the explorer's catalog adds the database's own). Sorted, and lowercase
    /// where the database ignores the case of function names (see
    /// [`names_ignore_case`](Self::names_ignore_case)).
    pub fn builtin_functions(self) -> &'static [&'static str] {
        match self {
            Self::Postgres => functions::POSTGRES,
            Self::Sqlite => functions::SQLITE,
            Self::Mysql => functions::MYSQL,
            Self::SqlServer => functions::SQL_SERVER,
            Self::DuckDb => functions::DUCKDB,
            Self::ClickHouse => functions::CLICKHOUSE,
            Self::Cassandra => functions::CQL,
        }
    }

    /// Whether names match whatever their case, so the formatter may re-case keywords and
    /// completion may write functions in the case the user is typing in. In ClickHouse,
    /// functions (`toDate`), types (`Array`) and columns are case-sensitive, and a column named
    /// like a keyword (`date`, `user`) is common: re-casing it would name another column.
    pub fn names_ignore_case(self) -> bool {
        !matches!(self, Self::ClickHouse)
    }

    /// How SQL that DbRover writes (qualified names, completions, grid edits) quotes
    /// identifiers and strings. Brackets on SQL Server, which mean an identifier whatever
    /// the session's `QUOTED_IDENTIFIER`.
    pub fn quoting(self) -> Quoting {
        match self {
            Self::Postgres | Self::Sqlite | Self::DuckDb | Self::Cassandra => Quoting::STANDARD,
            Self::Mysql | Self::ClickHouse => Quoting {
                identifier_open: '`',
                identifier_close: '`',
                backslash_escapes: true,
                string_prefix: "",
            },
            Self::SqlServer => Quoting {
                identifier_open: '[',
                identifier_close: ']',
                backslash_escapes: false,
                string_prefix: "N",
            },
        }
    }

    /// How the dialect's driver takes values for `:name` parameters (see
    /// [`crate::bind`]).
    ///
    /// PostgreSQL's consoles run over the simple query protocol, which returns every value as
    /// `psql` prints it but has no parameters; binding would need the extended protocol and
    /// decoding every type from binary. PostgreSQL types an untyped `'…'` literal from its
    /// context exactly as it would an untyped parameter. ClickHouse's own parameters need a
    /// type in the placeholder (`{name:String}`), and a `String` one doesn't compare with a
    /// number, where a literal is converted. CQL's bound values must be encoded in each
    /// column's type, which an unprepared statement's driver doesn't know.
    pub fn parameter_binding(self) -> ParameterBinding {
        match self {
            Self::Sqlite => ParameterBinding::Numbered("?"),
            Self::DuckDb => ParameterBinding::Numbered("$"),
            Self::SqlServer => ParameterBinding::Numbered("@P"),
            Self::Mysql => ParameterBinding::Positional,
            Self::Postgres | Self::ClickHouse | Self::Cassandra => ParameterBinding::Literal,
        }
    }

    /// Whether a table name belongs to the system catalog, which the explorer doesn't list:
    /// PostgreSQL's `pg_*` tables (always on the search path) and its catalog schemas,
    /// SQLite's `sqlite_*` tables, `pragma_*` functions and `temp` schema, MySQL's own
    /// databases, SQL Server's and DuckDB's catalog schemas, ClickHouse's `system` database,
    /// and the keyspaces Cassandra and ScyllaDB keep for themselves. Names are compared
    /// case-insensitively.
    pub fn is_system_name(self, schema: Option<&str>, table: &str) -> bool {
        let (table_prefixes, schemas): (&[&str], &[&str]) = match self {
            Self::Postgres => (&["pg_"], &["pg_catalog", "information_schema", "pg_toast"]),
            Self::Sqlite => (&["sqlite_", "pragma_"], &["temp"]),
            Self::Mysql => (
                &[],
                &["mysql", "information_schema", "performance_schema", "sys"],
            ),
            Self::SqlServer => (&[], &["sys", "INFORMATION_SCHEMA"]),
            Self::DuckDb => (&[], &["information_schema", "pg_catalog"]),
            Self::ClickHouse => (&[], &["system", "information_schema"]),
            Self::Cassandra => (&[], CQL_SYSTEM_KEYSPACES),
        };
        let table = table.to_ascii_lowercase();
        table_prefixes
            .iter()
            .any(|prefix| table.starts_with(prefix))
            || schema.is_some_and(|schema| {
                schemas
                    .iter()
                    .any(|system| schema.eq_ignore_ascii_case(system))
            })
    }
}

/// The keyspaces Cassandra 5 and ScyllaDB 2026 create for themselves.
pub(crate) const CQL_SYSTEM_KEYSPACES: &[&str] = &[
    "system",
    "system_auth",
    "system_distributed",
    "system_distributed_everywhere",
    "system_replicated_keys",
    "system_schema",
    "system_traces",
    "system_views",
    "system_virtual_schema",
    "audit",
];

/// `EXPLAIN FORMAT=TREE` arrived in MySQL 8.0.16; MariaDB has never had it.
fn has_tree_explain(server: &ServerInfo) -> bool {
    server.product == "MySQL" && version(server) >= (8, 0, 16)
}

/// Which JSON plans a MySQL driver's server writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MysqlFlavour {
    /// MariaDB: query blocks, and `ANALYZE` for actual figures.
    MariaDb,
    /// MySQL 8.3 and later: iterator plans, once `explain_json_format_version` is 2, which
    /// `EXPLAIN ANALYZE FORMAT=JSON` needs.
    Iterators,
    /// Older MySQL: query blocks, and no `EXPLAIN ANALYZE` in JSON.
    QueryBlocks,
}

/// `explain_json_format_version` arrived in MySQL 8.3.0.
fn mysql_flavour(server: &ServerInfo) -> MysqlFlavour {
    match server.product.as_str() {
        "MySQL" if version(server) >= (8, 3, 0) => MysqlFlavour::Iterators,
        "MySQL" => MysqlFlavour::QueryBlocks,
        _ => MysqlFlavour::MariaDb,
    }
}

/// The server's major, minor and patch version; a part that isn't a number counts as 0.
fn version(server: &ServerInfo) -> (u32, u32, u32) {
    let mut numbers = server
        .version
        .split(['.', '-'])
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
    )
}

/// The dialect each engine speaks.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_system_names_case_insensitively() {
        let cases = [
            (Dialect::Postgres, None, "pg_class", true),
            (
                Dialect::Postgres,
                Some("Information_Schema"),
                "tables",
                true,
            ),
            (Dialect::Postgres, Some("public"), "orders", false),
            (Dialect::Sqlite, None, "SQLITE_master", true),
            (Dialect::Sqlite, None, "pragma_table_info", true),
            (Dialect::Sqlite, Some("temp"), "scratch", true),
            (Dialect::Sqlite, Some("main"), "orders", false),
            (Dialect::Mysql, Some("Performance_Schema"), "threads", true),
            (Dialect::Mysql, Some("shop"), "orders", false),
            (Dialect::Mysql, None, "orders", false),
            (Dialect::SqlServer, Some("sys"), "objects", true),
            (Dialect::SqlServer, Some("dbo"), "orders", false),
            (Dialect::DuckDb, Some("information_schema"), "tables", true),
            (Dialect::DuckDb, Some("main"), "orders", false),
            (Dialect::ClickHouse, Some("system"), "tables", true),
            (Dialect::ClickHouse, Some("default"), "events", false),
        ];
        for (dialect, schema, table, expected) in cases {
            assert_eq!(
                dialect.is_system_name(schema, table),
                expected,
                "{dialect:?} {schema:?}.{table}"
            );
        }
    }

    #[test]
    fn asks_for_a_plan_to_draw_where_dbrover_reads_the_format() {
        let server = |product: &str, version: &str| ServerInfo {
            product: product.to_owned(),
            version: version.to_owned(),
        };
        let mysql_84 = server("MySQL", "8.4.13");
        let mysql_80 = server("MySQL", "8.0.40");
        let mariadb = server("MariaDB", "11.4.13");
        let cases = [
            (
                Dialect::Postgres,
                None,
                "EXPLAIN (FORMAT JSON) ",
                Some("EXPLAIN (ANALYZE, FORMAT JSON) "),
            ),
            (
                Dialect::Mysql,
                Some(&mysql_84),
                "EXPLAIN FORMAT=JSON ",
                Some("EXPLAIN ANALYZE FORMAT=JSON "),
            ),
            (
                Dialect::Mysql,
                Some(&mysql_80),
                "EXPLAIN FORMAT=JSON ",
                None,
            ),
            (
                Dialect::Mysql,
                Some(&mariadb),
                "EXPLAIN FORMAT=JSON ",
                Some("ANALYZE FORMAT=JSON "),
            ),
            (Dialect::Mysql, None, "EXPLAIN FORMAT=JSON ", None),
        ];
        for (dialect, server, estimated, analysed) in cases {
            let prefix = |depth| {
                dialect
                    .plan_request(depth, server)
                    .map(|request| request.prefix)
            };
            assert_eq!(
                prefix(ExplainDepth::Estimated),
                Some(estimated),
                "{server:?}"
            );
            assert_eq!(prefix(ExplainDepth::Analysed), analysed, "{server:?}");
        }
        assert_eq!(
            Dialect::Mysql
                .plan_request(ExplainDepth::Estimated, None)
                .map(|request| request.format),
            Some(PlanFormat::MysqlJson)
        );
        for dialect in [Dialect::Sqlite, Dialect::DuckDb, Dialect::ClickHouse] {
            for depth in [ExplainDepth::Estimated, ExplainDepth::Analysed] {
                assert_eq!(dialect.plan_request(depth, None), None, "{dialect:?}");
            }
        }
        for depth in [ExplainDepth::Estimated, ExplainDepth::Analysed] {
            let request = Dialect::SqlServer.plan_request(depth, None);
            assert_eq!(
                request.map(|request| (request.prefix, request.format)),
                Some(("", PlanFormat::SqlServerXml))
            );
        }
        // Only MySQL with iterator plans needs its format switched, for either depth.
        let switched = |server| {
            Dialect::Mysql
                .explain_setting(ExplainDepth::Estimated, server)
                .is_some()
        };
        assert!(switched(Some(&mysql_84)));
        assert!(!switched(Some(&mysql_80)));
        assert!(!switched(Some(&mariadb)));
        assert!(
            Dialect::SqlServer
                .explain_setting(ExplainDepth::Estimated, None)
                .is_some()
        );
    }

    #[test]
    fn explains_as_tree_on_mysql_8_only() {
        let server = |product: &str, version: &str| ServerInfo {
            product: product.to_owned(),
            version: version.to_owned(),
        };
        let cases = [
            (Some(server("MySQL", "8.4.3")), "EXPLAIN FORMAT=TREE "),
            (Some(server("MySQL", "8.0.16")), "EXPLAIN FORMAT=TREE "),
            (Some(server("MySQL", "8.0.15")), "EXPLAIN "),
            (Some(server("MariaDB", "11.4.4")), "EXPLAIN "),
            (None, "EXPLAIN "),
        ];
        for (server, expected) in cases {
            assert_eq!(
                Dialect::Mysql.explain_prefix(server.as_ref()),
                expected,
                "{server:?}"
            );
        }
    }
}
