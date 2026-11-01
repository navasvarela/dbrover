//! What a statement does, read from its parse tree: whether it only reads, or changes data,
//! the schema or the server, and which schemas it names. The MCP server refuses what a
//! project doesn't allow before it reaches the database (see `docs/mcp-server.md`).
//!
//! Unlike the rest of this crate, this is strict: a statement the parser can't read is not
//! guessed at, it is [`Unclassified`], because what it does can't be known.

mod cql;
mod names;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, ObjectName, ObjectType, Query, SetExpr, Statement, TableFactor, Visit, Visitor,
};
use sqlparser::parser::Parser;

use self::names::NameCollector;
pub use self::names::{NamePart, NamedSchemas};
use crate::dialect::Dialect;
use crate::lexer::tokenize;

/// What a statement may change, from least to most. A statement holding others (a `WITH`
/// clause with a `DELETE`, an `EXPLAIN ANALYZE`) is the most demanding of them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatementKind {
    /// Browsing the catalog, `SELECT`, `EXPLAIN`, `SHOW`.
    Read,
    /// `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE`, and reads that lock rows.
    Write,
    /// `CREATE`, `ALTER`, `DROP`, `COMMENT ON`, `SELECT … INTO` a new table.
    Schema,
    /// `BEGIN`, `COMMIT`, `ROLLBACK`, savepoints: whoever runs the statement owns the
    /// transaction, so an MCP client may never send these.
    Transaction,
    /// Everything else: grants, roles and users, databases, `SET` and other server settings,
    /// `COPY`, `VACUUM`, `PRAGMA`, `ATTACH`, `CALL`, killing sessions, and any statement this
    /// classifier doesn't know.
    Admin,
}

impl StatementKind {
    pub fn description(self) -> &'static str {
        match self {
            Self::Read => "reads",
            Self::Write => "changes data",
            Self::Schema => "changes the schema",
            Self::Transaction => "controls the transaction",
            Self::Admin => "administers the database",
        }
    }
}

/// Why a statement has no kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unclassified {
    #[error("there is no statement")]
    Empty,
    #[error("send one statement at a time; this text holds {0}")]
    Several(usize),
    #[error("the statement can't be parsed, so what it does is unknown: {0}")]
    Unparseable(String),
}

/// What [`analyze`] reads from one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub kind: StatementKind,
    /// The schemas the statement's tables, views and functions are qualified with.
    pub schemas: NamedSchemas,
}

/// Classifies `sql`, which must hold exactly one statement.
///
/// # Errors
///
/// [`Unclassified`] when the text holds no statement, several, or one the dialect's parser
/// can't read.
pub fn classify(sql: &str, dialect: Dialect) -> Result<StatementKind, Unclassified> {
    analyze(sql, dialect).map(|analysis| analysis.kind)
}

/// Classifies `sql`, which must hold exactly one statement, and finds the schemas it names.
///
/// # Errors
///
/// [`Unclassified`] when the text holds no statement, several, or one the dialect's parser
/// can't read.
pub fn analyze(sql: &str, dialect: Dialect) -> Result<Analysis, Unclassified> {
    let Some(parser) = dialect.sqlparser_dialect() else {
        return cql::analyze(sql);
    };
    let parsed = as_parsed(sql, dialect);
    let statements = Parser::parse_sql(parser, &parsed).map_err(|error| {
        let message = error.to_string();
        let message = message
            .strip_prefix("sql parser error: ")
            .unwrap_or(&message);
        Unclassified::Unparseable(message.to_owned())
    })?;
    let statement = match statements.as_slice() {
        [] => return Err(Unclassified::Empty),
        [statement] => statement,
        several => return Err(Unclassified::Several(several.len())),
    };
    let mut classifier = Classifier {
        kind: StatementKind::Read,
        names: NameCollector::default(),
    };
    // The visitor never breaks, so the flow it returns says nothing.
    let _ = statement.visit(&mut classifier);
    Ok(Analysis {
        kind: classifier.kind,
        schemas: classifier.names.finish(),
    })
}

/// The text to parse for `sql`. MariaDB's `ANALYZE` before a statement runs the statement and
/// explains it, as MySQL's `EXPLAIN ANALYZE` does, but `sqlparser` only reads `ANALYZE TABLE`;
/// so such a statement is read as the `EXPLAIN ANALYZE` it is, which classifies it by the
/// statement it runs. `ANALYZE TABLE`, and its `LOCAL` and `NO_WRITE_TO_BINLOG` forms, are
/// left as they are.
fn as_parsed(sql: &str, dialect: Dialect) -> Cow<'_, str> {
    if dialect != Dialect::Mysql {
        return Cow::Borrowed(sql);
    }
    let words: Vec<String> = tokenize(sql, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .take(2)
        .map(|token| sql[token.range].to_ascii_lowercase())
        .collect();
    match words.as_slice() {
        [first, second]
            if first == "analyze"
                && !matches!(second.as_str(), "table" | "local" | "no_write_to_binlog") =>
        {
            Cow::Owned(format!("EXPLAIN {sql}"))
        }
        _ => Cow::Borrowed(sql),
    }
}

/// Walks every statement, query, expression and table in the tree, nested ones included,
/// keeping the most demanding kind seen. Nesting is how writes hide: `WITH d AS (DELETE …)
/// SELECT …` is a query whose `WITH` holds a `DELETE` statement.
struct Classifier {
    kind: StatementKind,
    names: NameCollector,
}

impl Classifier {
    fn raise(&mut self, kind: StatementKind) {
        self.kind = self.kind.max(kind);
    }
}

impl Visitor for Classifier {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        self.raise(statement_kind(statement));
        self.names.statement(statement);
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        // `FOR UPDATE` and its kin take row locks, which only writers need.
        if !query.locks.is_empty() {
            self.raise(StatementKind::Write);
        }
        if selects_into(&query.body) {
            self.raise(StatementKind::Schema);
        }
        self.names.query(query);
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        self.names.relation(relation);
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, table: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Function { name, .. } = table {
            self.function(name);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::Function(function) = expr {
            self.function(&function.name);
        }
        ControlFlow::Continue(())
    }
}

impl Classifier {
    fn function(&mut self, name: &ObjectName) {
        if is_admin_function(name) {
            self.raise(StatementKind::Admin);
        }
        self.names.function(name);
    }
}

/// `SELECT … INTO new_table` (PostgreSQL) creates a table; MySQL's `INTO @variable` sets
/// variables. Neither only reads.
fn selects_into(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.into.is_some(),
        SetExpr::SetOperation { left, right, .. } => selects_into(left) || selects_into(right),
        _ => false,
    }
}

/// Built-in functions that do what [`StatementKind::Admin`] stands for, though called from
/// an innocent `SELECT`: end other sessions, change server settings, read and write the
/// server's files, or reach other servers. Mostly PostgreSQL's, plus SQLite's
/// `load_extension`; MySQL does these with statements, which are classified as such.
///
/// Not exhaustive, and it can't be: a user-defined function can do any of it too. It catches
/// the well-known ones even where a read-only transaction doesn't (`pg_terminate_backend`
/// and `set_config` both work in one), and the database's grants on a dedicated MCP login
/// remain what makes a project airtight.
const ADMIN_FUNCTIONS: &[&str] = &[
    // Sessions.
    "pg_cancel_backend",
    "pg_terminate_backend",
    // Settings and the server itself.
    "set_config",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_promote",
    "pg_switch_wal",
    "pg_create_restore_point",
    "pg_backup_start",
    "pg_backup_stop",
    "pg_start_backup",
    "pg_stop_backup",
    "pg_create_physical_replication_slot",
    "pg_create_logical_replication_slot",
    "pg_drop_replication_slot",
    "pg_logical_slot_get_changes",
    // Files on the server.
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "pg_file_write",
    "lo_import",
    "lo_export",
    // Other servers.
    "dblink",
    "dblink_exec",
    // SQLite: loads native code into the process.
    "load_extension",
];

/// Matches the last part of the name, so a schema-qualified call (`pg_catalog.set_config`)
/// is caught too. Unquoted names fold case in every dialect served; a quoted one only
/// names the built-in when spelt in lower case.
fn is_admin_function(name: &ObjectName) -> bool {
    let Some(last) = name.0.last().and_then(|part| part.as_ident()) else {
        return false;
    };
    let folded = if last.quote_style.is_some() {
        last.value.clone()
    } else {
        last.value.to_ascii_lowercase()
    };
    ADMIN_FUNCTIONS.contains(&folded.as_str())
}

/// The kind of one statement on its own; what it holds is visited separately.
fn statement_kind(statement: &Statement) -> StatementKind {
    use StatementKind::{Admin, Read, Schema, Transaction, Write};
    match statement {
        // EXPLAIN's own kind is reading; the statement it explains is visited too, which is
        // what makes `EXPLAIN ANALYZE DELETE …` (which runs the DELETE) a write.
        Statement::Query(_)
        | Statement::Explain { .. }
        | Statement::ExplainTable { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowVariable { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCatalogs { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowCharset(_)
        | Statement::ShowObjects(_)
        | Statement::ShowTables { .. }
        | Statement::ShowViews { .. }
        | Statement::ShowCollation { .. } => Read,

        Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Truncate(_)
        | Statement::Lock(_)
        | Statement::LockTables { .. }
        | Statement::UnlockTables => Write,

        // Dropping a database, a role or a user is administration, like creating one (which
        // falls through to the last arm).
        Statement::Drop {
            object_type: ObjectType::Database | ObjectType::Role | ObjectType::User,
            ..
        } => Admin,

        Statement::CreateView(_)
        | Statement::CreateTable(_)
        | Statement::CreateVirtualTable { .. }
        | Statement::CreateIndex(_)
        | Statement::CreateSchema { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::CreateProcedure { .. }
        | Statement::CreateSequence { .. }
        | Statement::CreateDomain(_)
        | Statement::CreateType { .. }
        | Statement::CreatePolicy(_)
        | Statement::CreateCollation(_)
        | Statement::CreateOperator(_)
        | Statement::CreateOperatorFamily(_)
        | Statement::CreateOperatorClass(_)
        | Statement::CreateTextSearch(_)
        | Statement::AlterTable(_)
        | Statement::AlterSchema(_)
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterFunction(_)
        | Statement::AlterType(_)
        | Statement::AlterCollation(_)
        | Statement::AlterOperator(_)
        | Statement::AlterOperatorFamily(_)
        | Statement::AlterOperatorClass(_)
        | Statement::AlterTextSearch(_)
        | Statement::AlterPolicy(_)
        | Statement::Drop { .. }
        | Statement::DropFunction(_)
        | Statement::DropDomain(_)
        | Statement::DropProcedure { .. }
        | Statement::DropPolicy(_)
        | Statement::DropOperator(_)
        | Statement::DropOperatorFamily(_)
        | Statement::DropOperatorClass(_)
        | Statement::DropTrigger(_)
        | Statement::Comment { .. }
        | Statement::RenameTable(_) => Schema,

        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::ReleaseSavepoint { .. } => Transaction,

        // Not listed, so administration:
        // - `CALL`: a procedure's body can do anything, grants and DDL included, and the
        //   parser can't see into it, which is the rule for statements it can't read.
        // - `LOAD DATA`, like `COPY`, reads files.
        // - Extensions load code into the server.
        // - Anything else, deliberately not listed one by one: a statement added to
        //   sqlparser later, or one overlooked here, must not slip through as anything less.
        _ => Admin,
    }
}
