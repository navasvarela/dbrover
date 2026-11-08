use std::ops::Range;

use crate::dialect::{Dialect, ScriptRules};
use crate::lexer::{Token, TokenKind, tokenize};

/// Splits a console's text into statements at top-level semicolons.
///
/// Each range covers one statement from its first to its last meaningful token: surrounding
/// whitespace and comments, and the terminating semicolon, are left out. Statements with no
/// meaningful tokens (stray `;`, comment-only stretches) are dropped.
///
/// Semicolons inside strings, quoted identifiers, comments and dollar-quoted bodies don't
/// split. Neither do semicolons inside a `BEGIN … END` block of a `CREATE` statement (SQLite
/// trigger bodies, PostgreSQL `BEGIN ATOMIC` functions, MySQL procedures), or inside
/// `CASE … END`. An unclosed block therefore runs to the end of the text, as the database
/// would read it.
///
/// In a dialect with the client-side `DELIMITER` command (MySQL), a line starting with it
/// isn't a statement: it sets what ends the statements after it, until the next one, and
/// with a delimiter other than `;`, semicolons never split.
///
/// In SQL Server's Transact-SQL, `GO` alone on a line (or followed by a count, which is
/// ignored: the batch runs once) ends the statement before it whatever its blocks, and isn't
/// a statement itself. `BEGIN … END` blocks can appear anywhere there, as control flow, and
/// the body of a procedure, function or trigger runs to the next `GO`, semicolons and all,
/// because the server reads the rest of the batch as the body.
///
/// In CQL, `BEGIN [UNLOGGED | COUNTER] BATCH … APPLY BATCH` is one statement, whose own
/// statements' semicolons don't split.
///
/// In GoogleSQL, a script's blocks are one statement each, nested ones included: `BEGIN …
/// END`, `IF … END IF`, `LOOP`, `WHILE`, `REPEAT` and `FOR … END …`, and a `CASE` statement's
/// `END CASE`, each opened where a statement starts; `BEGIN` alone or `BEGIN TRANSACTION`
/// starts a transaction instead.
pub fn split_statements(sql: &str, dialect: Dialect) -> Vec<Range<usize>> {
    let tokens: Vec<Token> = tokenize(sql, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let rules = dialect.script_rules();
    let mut splitter = Splitter {
        sql,
        tokens: &tokens,
        rules,
        statements: Vec::new(),
        current: None,
        first_word: None,
        words_seen: 0,
        block_depth: 0,
        procedural_blocks: Vec::new(),
        runs_to_batch_end: false,
        delimiter: None,
    };
    let mut index = 0;
    while index < tokens.len() {
        index = if rules.delimiter_command && splitter.starts_delimiter_command(index) {
            splitter.delimiter_command(index)
        } else if rules.go_batches && splitter.is_go_line(index) {
            splitter.end_batch(index)
        } else {
            splitter.step(index)
        };
    }
    splitter.statements.extend(splitter.current);
    splitter.statements
}

struct Splitter<'a> {
    sql: &'a str,
    /// The meaningful tokens: no whitespace or comments.
    tokens: &'a [Token],
    rules: ScriptRules,
    statements: Vec<Range<usize>>,
    current: Option<Range<usize>>,
    first_word: Option<String>,
    /// Words read so far in the current statement.
    words_seen: usize,
    block_depth: usize,
    /// GoogleSQL's open blocks, innermost last.
    procedural_blocks: Vec<ProceduralBlock>,
    /// Whether the current statement is a Transact-SQL routine, whose body ends only with
    /// its batch.
    runs_to_batch_end: bool,
    /// What `DELIMITER` set, when it isn't `;`.
    delimiter: Option<String>,
}

/// What opened one of GoogleSQL's blocks, which decides what follows its `THEN` and `ELSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProceduralBlock {
    /// A statement, such as `IF … THEN`, `WHILE … DO` or a `CASE` statement, whose clauses
    /// hold statements.
    Statements,
    /// A `CASE` expression, whose `THEN` and `ELSE` hold values: `THEN IF(a, b, c)` there is a
    /// function call, not an `IF` statement.
    CaseExpression,
}

/// In a GoogleSQL block of statements, the words after which a statement starts: `IF … THEN`,
/// `ELSE`, `WHILE … DO`, `FOR … DO`, `BEGIN`, `LOOP`, `REPEAT`, and `EXCEPTION WHEN ERROR THEN`.
const BEFORE_PROCEDURAL_STATEMENTS: &[&str] = &["begin", "do", "else", "loop", "repeat", "then"];

/// GoogleSQL's statements that open a block, with `BEGIN` and `CASE`, where a statement
/// starts. Elsewhere they are other things: `IF(…)` and `REPEAT(…)` are functions, and `FOR
/// SYSTEM_TIME AS OF` reads a table as it was.
const PROCEDURAL_BLOCK_STATEMENTS: &[&str] = &["for", "if", "loop", "repeat", "while"];

/// After `END`, these close a block that didn't open with `BEGIN` or `CASE`, so they mustn't
/// close one that did: `END IF` inside a MySQL procedure's `BEGIN … END`. Not in
/// Transact-SQL, where `END IF @x = 1 …` closes a block and starts the next statement.
const END_OF_CONTROL_FLOW: &[&str] = &["if", "loop", "repeat", "while"];

/// After Transact-SQL's `BEGIN`, these make it a statement rather than a block: `BEGIN TRAN`,
/// `BEGIN DISTRIBUTED TRANSACTION`, `BEGIN DIALOG CONVERSATION`.
const BEGIN_STATEMENTS: &[&str] = &["tran", "transaction", "distributed", "dialog"];

/// After Transact-SQL's `END`, this makes it a statement rather than a block's end.
const END_STATEMENTS: &[&str] = &["conversation"];

/// What follows `CREATE`, `ALTER` or `CREATE OR ALTER` in a Transact-SQL routine, whose body
/// is the rest of its batch.
const BATCH_BODIED: &[&str] = &["proc", "procedure", "function", "trigger"];

/// How many words into a statement its kind is settled: `create or alter procedure`.
const KIND_WORDS: usize = 4;

impl Splitter<'_> {
    fn word(&self, index: usize) -> Option<String> {
        self.tokens
            .get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| self.sql[token.range.clone()].to_ascii_lowercase())
    }

    fn starts_delimiter_command(&self, index: usize) -> bool {
        self.current.is_none() && self.word(index).as_deref() == Some("delimiter")
    }

    /// Reads `DELIMITER <text>` up to the end of its line, and returns the index of the first
    /// token after that line.
    fn delimiter_command(&mut self, index: usize) -> usize {
        let Some(command) = self.tokens.get(index) else {
            return index + 1;
        };
        let rest = &self.sql[command.range.end..];
        let line_end = command.range.end + rest.find('\n').unwrap_or(rest.len());
        let delimiter = self.sql[command.range.end..line_end].trim();
        self.delimiter = (!delimiter.is_empty() && delimiter != ";").then(|| delimiter.to_owned());
        self.first_token_from(line_end)
    }

    /// Whether the token at `index` is a `GO` on a line of its own, optionally followed by a
    /// count and a `--` comment.
    fn is_go_line(&self, index: usize) -> bool {
        let Some(token) = self.tokens.get(index) else {
            return false;
        };
        if self.word(index).as_deref() != Some("go") {
            return false;
        }
        let line_start = self.sql[..token.range.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let rest = &self.sql[token.range.end..];
        let line_rest = &rest[..rest.find('\n').unwrap_or(rest.len())];
        let after = line_rest
            .split_once("--")
            .map_or(line_rest, |(code, _)| code)
            .trim();
        self.sql[line_start..token.range.start].trim().is_empty()
            && after.chars().all(|c| c.is_ascii_digit())
    }

    /// Ends the statement before a `GO` line, and returns the index of the first token after
    /// that line.
    fn end_batch(&mut self, index: usize) -> usize {
        let Some(go) = self.tokens.get(index) else {
            return index + 1;
        };
        let rest = &self.sql[go.range.end..];
        let line_end = go.range.end + rest.find('\n').unwrap_or(rest.len());
        self.end_statement();
        self.first_token_from(line_end)
    }

    fn first_token_from(&self, offset: usize) -> usize {
        self.tokens
            .iter()
            .position(|token| token.range.start >= offset)
            .unwrap_or(self.tokens.len())
    }

    /// Takes in the token at `index` and returns the index of the next one to read.
    fn step(&mut self, index: usize) -> usize {
        let Some(token) = self.tokens.get(index) else {
            return index + 1;
        };
        if let Some(delimiter) = &self.delimiter {
            if self.sql[token.range.start..].starts_with(delimiter.as_str()) {
                let end = token.range.start + delimiter.len();
                self.end_statement();
                return self.first_token_from(end);
            }
            self.extend(token.range.clone());
            return index + 1;
        }
        match token.kind {
            TokenKind::Semicolon
                if self.block_depth == 0
                    && self.procedural_blocks.is_empty()
                    && !self.runs_to_batch_end =>
            {
                self.end_statement();
            }
            kind => {
                if kind == TokenKind::Word {
                    self.track_blocks(index);
                }
                self.extend(token.range.clone());
            }
        }
        index + 1
    }

    fn track_blocks(&mut self, index: usize) {
        let Some(word) = self.word(index) else {
            return;
        };
        let next = self.word(index + 1);
        let next_is = |words: &[&str]| next.as_deref().is_some_and(|next| words.contains(&next));
        let (starts_block, ends_block) = if self.rules.procedural_blocks {
            self.track_procedural_blocks(index, &word);
            (false, false)
        } else if self.rules.batches {
            (
                word == "begin"
                    && self.words_seen == 0
                    && next_is(&["batch", "unlogged", "counter"]),
                word == "apply" && next_is(&["batch"]),
            )
        } else if self.rules.blocks_anywhere {
            (
                word == "case" || (word == "begin" && !next_is(BEGIN_STATEMENTS)),
                word == "end" && !next_is(END_STATEMENTS),
            )
        } else {
            (
                word == "case" || (word == "begin" && self.first_word.as_deref() == Some("create")),
                word == "end" && !next_is(END_OF_CONTROL_FLOW),
            )
        };
        if starts_block {
            self.block_depth += 1;
        } else if ends_block {
            self.block_depth = self.block_depth.saturating_sub(1);
        }
        self.words_seen += 1;
        if self.rules.go_batches
            && self.words_seen <= KIND_WORDS
            && matches!(self.first_word.as_deref(), Some("create" | "alter"))
            && BATCH_BODIED.contains(&word.as_str())
        {
            self.runs_to_batch_end = true;
        }
        self.first_word.get_or_insert(word);
    }

    /// Opens or closes a GoogleSQL block at the word at `index`, `word` lowercased. Read from
    /// the words alone, without GoogleSQL's grammar: `END` closes the innermost block whatever
    /// follows it (`END IF`, `END LOOP`, a label), and a block left open runs to the end of
    /// the text, as BigQuery would read it.
    fn track_procedural_blocks(&mut self, index: usize, word: &str) {
        // The `CASE` of `END CASE` opens nothing.
        if index
            .checked_sub(1)
            .is_some_and(|before| self.word(before).as_deref() == Some("end"))
        {
            return;
        }
        let starts_statement = self.starts_procedural_statement(index);
        let opens = match word {
            "case" if starts_statement => Some(ProceduralBlock::Statements),
            "case" => Some(ProceduralBlock::CaseExpression),
            // A procedure's body follows its `CREATE PROCEDURE … ()`.
            "begin"
                if (starts_statement || self.first_word.as_deref() == Some("create"))
                    && !self.begins_transaction(index) =>
            {
                Some(ProceduralBlock::Statements)
            }
            _ if starts_statement && PROCEDURAL_BLOCK_STATEMENTS.contains(&word) => {
                Some(ProceduralBlock::Statements)
            }
            _ => None,
        };
        if let Some(block) = opens {
            self.procedural_blocks.push(block);
        } else if word == "end" {
            self.procedural_blocks.pop();
        }
    }

    /// Whether the word at `index` starts a GoogleSQL statement: the script's first word, one
    /// after a `;` or a label's `:`, or one after the words that lead into a block's
    /// statements.
    fn starts_procedural_statement(&self, index: usize) -> bool {
        let Some(before) = index
            .checked_sub(1)
            .and_then(|before| self.tokens.get(before))
        else {
            return true;
        };
        if before.kind == TokenKind::Semicolon || &self.sql[before.range.clone()] == ":" {
            return true;
        }
        self.procedural_blocks.last() == Some(&ProceduralBlock::Statements)
            && self
                .word(index - 1)
                .is_some_and(|word| BEFORE_PROCEDURAL_STATEMENTS.contains(&word.as_str()))
    }

    /// Whether the `BEGIN` at `index` starts a transaction (`BEGIN;`, `BEGIN TRANSACTION`)
    /// rather than a block.
    fn begins_transaction(&self, index: usize) -> bool {
        self.tokens
            .get(index + 1)
            .is_none_or(|next| next.kind == TokenKind::Semicolon)
            || self.word(index + 1).as_deref() == Some("transaction")
    }

    fn extend(&mut self, range: Range<usize>) {
        self.current = Some(match self.current.take() {
            Some(current) => current.start..range.end,
            None => range,
        });
    }

    fn end_statement(&mut self) {
        self.statements.extend(self.current.take());
        self.first_word = None;
        self.words_seen = 0;
        self.block_depth = 0;
        self.procedural_blocks.clear();
        self.runs_to_batch_end = false;
    }
}

/// The index of the statement to run for a cursor at byte offset `cursor`: the statement
/// containing it, else the nearest one before it (the cursor usually sits just after the
/// statement's `;`), else the first one after it.
pub fn statement_at(statements: &[Range<usize>], cursor: usize) -> Option<usize> {
    statements
        .iter()
        .rposition(|statement| statement.start <= cursor)
        .or_else(|| (!statements.is_empty()).then_some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statements(sql: &str, dialect: Dialect) -> Vec<&str> {
        split_statements(sql, dialect)
            .into_iter()
            .map(|range| &sql[range])
            .collect()
    }

    #[test]
    fn splits_only_at_top_level_semicolons() {
        let cases: [(&str, Dialect, &[&str]); 7] = [
            (
                "select 1; select 2",
                Dialect::Postgres,
                &["select 1", "select 2"],
            ),
            (
                "  -- leading\n select ';' ; ;; /* only a comment */ ;",
                Dialect::Postgres,
                &["select ';'"],
            ),
            (
                "create function f() returns int as $$ select 1; $$ language sql; select f()",
                Dialect::Postgres,
                &[
                    "create function f() returns int as $$ select 1; $$ language sql",
                    "select f()",
                ],
            ),
            (
                "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET n = 1; DELETE FROM c; END; SELECT 1",
                Dialect::Sqlite,
                &[
                    "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET n = 1; DELETE FROM c; END",
                    "SELECT 1",
                ],
            ),
            (
                "BEGIN; update t set a = case when b then 1 else 2 end; COMMIT",
                Dialect::Postgres,
                &[
                    "BEGIN",
                    "update t set a = case when b then 1 else 2 end",
                    "COMMIT",
                ],
            ),
            (
                "select 'unterminated; select 2",
                Dialect::Sqlite,
                &["select 'unterminated; select 2"],
            ),
            ("", Dialect::Sqlite, &[]),
        ];

        for (sql, dialect, expected) in cases {
            assert_eq!(statements(sql, dialect), expected, "{sql}");
        }
    }

    #[test]
    fn follows_mysql_delimiter_commands_and_control_flow_blocks() {
        let script = "DELIMITER //\n\
            CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\n\
            CALL p()//\n\
            delimiter ;\n\
            SELECT 3; SELECT 4";
        let block = "CREATE PROCEDURE q(n INT) BEGIN IF n > 0 THEN SELECT 1; END IF; \
            WHILE n > 0 DO SET n = n - 1; END WHILE; END; SELECT 5";
        let cases: [(&str, Dialect, &[&str]); 3] = [
            (
                script,
                Dialect::Mysql,
                &[
                    "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                    "CALL p()",
                    "SELECT 3",
                    "SELECT 4",
                ],
            ),
            (
                block,
                Dialect::Mysql,
                &[
                    "CREATE PROCEDURE q(n INT) BEGIN IF n > 0 THEN SELECT 1; END IF; \
                    WHILE n > 0 DO SET n = n - 1; END WHILE; END",
                    "SELECT 5",
                ],
            ),
            // Only MySQL has the command; elsewhere it is an ordinary word.
            (
                "delimiter //\nselect 1; select 2",
                Dialect::Postgres,
                &["delimiter //\nselect 1", "select 2"],
            ),
        ];

        for (sql, dialect, expected) in cases {
            assert_eq!(statements(sql, dialect), expected, "{sql}");
        }
    }

    #[test]
    fn follows_sql_server_batches_and_blocks() {
        let routines = "CREATE PROCEDURE p AS SELECT 1; SELECT 2;\n\
            go\n\
            create or alter function f() returns int as begin return 1; end\n\
            GO 3 -- three times\n\
            SELECT 3; SELECT 4";
        let control_flow = "IF @n > 0 BEGIN SELECT 1; SELECT 2; END IF @n < 0 SELECT 3; \
            BEGIN TRY SELECT 1/0; END TRY BEGIN CATCH SELECT ERROR_MESSAGE(); END CATCH; \
            BEGIN TRAN; COMMIT";
        let cases: [(&str, Dialect, &[&str]); 4] = [
            (
                routines,
                Dialect::SqlServer,
                &[
                    "CREATE PROCEDURE p AS SELECT 1; SELECT 2;",
                    "create or alter function f() returns int as begin return 1; end",
                    "SELECT 3",
                    "SELECT 4",
                ],
            ),
            (
                control_flow,
                Dialect::SqlServer,
                &[
                    "IF @n > 0 BEGIN SELECT 1; SELECT 2; END IF @n < 0 SELECT 3",
                    "BEGIN TRY SELECT 1/0; END TRY BEGIN CATCH SELECT ERROR_MESSAGE(); END CATCH",
                    "BEGIN TRAN",
                    "COMMIT",
                ],
            ),
            // `GO` only separates on a line of its own, and never inside a string.
            (
                "SELECT 'a\nGO\n' AS go\nGO\nSELECT go FROM t GO",
                Dialect::SqlServer,
                &["SELECT 'a\nGO\n' AS go", "SELECT go FROM t GO"],
            ),
            // Only SQL Server has batches; elsewhere `go` is an ordinary word.
            (
                "select 1\ngo\nselect 2",
                Dialect::Postgres,
                &["select 1\ngo\nselect 2"],
            ),
        ];

        for (sql, dialect, expected) in cases {
            assert_eq!(statements(sql, dialect), expected, "{sql}");
        }
    }

    /// A CQL batch is one statement, whatever its statements' semicolons; `BEGIN` alone opens
    /// nothing, and `end` is an ordinary name.
    #[test]
    fn keeps_a_cql_batch_whole() {
        let batch = "BEGIN UNLOGGED BATCH\n  INSERT INTO a (id) VALUES (1);\n  \
                     UPDATE b SET end = 2 WHERE id = 1;\nAPPLY BATCH";
        let script = format!("select * from a; {batch}; select end from b");
        assert_eq!(
            statements(&script, Dialect::Cassandra),
            ["select * from a", batch, "select end from b"]
        );
        assert_eq!(
            statements("begin transaction; select 1", Dialect::Cassandra),
            ["begin transaction", "select 1"]
        );
    }

    /// A GoogleSQL script's blocks are one statement each, nested or not, while `IF(…)` and
    /// `REPEAT(…)` are functions, a `CASE` expression's `THEN IF(…)` is a call, and `BEGIN` alone
    /// is a transaction.
    #[test]
    fn keeps_googlesql_blocks_whole() {
        let declare = "DECLARE n INT64 DEFAULT 3";
        let block = "BEGIN\n  IF n > 0 THEN\n    SELECT IF(n > 1, 'many', 'one');\n  \
                     ELSEIF n < 0 THEN\n    SELECT 'negative';\n  ELSE\n    \
                     SELECT CASE WHEN n = 0 THEN IF(true, 1, 2) ELSE 3 END;\n  END IF;\n  \
                     outer: LOOP\n    SET n = n - 1;\n    IF n <= 0 THEN LEAVE outer; END IF;\n  \
                     END LOOP outer;\nEXCEPTION WHEN ERROR THEN\n  SELECT @@error.message;\nEND";
        let loops = "WHILE n > 0 DO SET n = n - 1; END WHILE";
        let repeat = "REPEAT SET n = n + 1; SELECT REPEAT('a', n); UNTIL n >= 3 END REPEAT";
        let for_in = "FOR row IN (SELECT x FROM t FOR SYSTEM_TIME AS OF CURRENT_TIMESTAMP()) DO \
                      SELECT row.x; END FOR";
        let case = "CASE n WHEN 1 THEN SELECT 1; ELSE SELECT 2; END CASE";
        let procedure = "CREATE PROCEDURE ds.p(x INT64) BEGIN SELECT x; SELECT x + 1; END";
        let script = format!(
            "{declare};\n{block};\n{loops};\n{repeat};\n{for_in};\n{case};\n{procedure};\n\
             BEGIN TRANSACTION;\nBEGIN;\nCOMMIT TRANSACTION;\nSELECT 1"
        );
        assert_eq!(
            statements(&script, Dialect::BigQuery),
            [
                declare,
                block,
                loops,
                repeat,
                for_in,
                case,
                procedure,
                "BEGIN TRANSACTION",
                "BEGIN",
                "COMMIT TRANSACTION",
                "SELECT 1",
            ]
        );
    }

    #[test]
    fn picks_the_statement_under_or_before_the_cursor() {
        let sql = "  select 1;\n\nselect 2;  ";
        let ranges = split_statements(sql, Dialect::Postgres);

        let cases = [(0, 0), (4, 0), (11, 0), (13, 1), (sql.len(), 1)];
        for (cursor, expected) in cases {
            assert_eq!(
                statement_at(&ranges, cursor),
                Some(expected),
                "cursor {cursor}"
            );
        }
        assert_eq!(statement_at(&[], 3), None);
    }
}
