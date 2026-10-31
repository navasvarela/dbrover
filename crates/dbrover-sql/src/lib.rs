//! SQL text utilities for the query console: a forgiving lexer, statement splitting,
//! statement-under-cursor, a check for destructive statements, a formatter, a syntax
//! checker and linter, a classifier of what a statement may change, the DDL the object
//! helpers write, the statements that change a table's rows from the grid, the query that
//! sorts and filters a statement's rows in the database, the SCRAM verifiers role passwords
//! are sent as, and the `:name` parameters a statement takes.
//!
//! Everything works on text as typed, which is usually half-finished: nothing here fails on
//! invalid input. Unterminated strings and comments run to the end of the text. All
//! positions are byte offsets into the input. Only the syntax checker parses, with
//! `sqlparser`, one statement at a time.
//!
//! A hand-written lexer rather than `sqlparser`'s tokenizer: that one rejects unterminated
//! input and reports line/column positions, while the editor needs byte ranges for any text,
//! and the same tokens also drive syntax highlighting.

mod arrange;
mod check;
mod classify;
mod ddl;
mod dialect;
mod dml;
mod format;
mod keywords;
mod lexer;
mod parameters;
mod password;
mod projection;
mod safety;
mod split;
mod transaction;

pub use arrange::{Arranged, Arrangement, SortOrder, arrange, can_arrange};
pub use check::{LintRule, Problem, ProblemSource, Severity, check_statement, is_checkable};
pub use classify::{
    Analysis, NamePart, NamedSchemas, StatementKind, Unclassified, analyze, classify,
};
pub use ddl::{
    Check, ColumnSpec, DdlError, DdlOperation, ExistingTable, ForeignKey, IndexMethod, IndexSpec,
    ObjectName, OwnedObjects, PasswordChange, ReferentialAction, RoleSpec, SchemaSpec,
    TableAlteration, TableSpec, UniqueKey, ViewKind, ViewSpec, ddl_statements, quote_name,
    rebuild_refusal, view_query,
};
pub use dialect::{
    Capabilities, Dialect, ExplainSetting, ForeignKeySwitch, PlanRequest, Savepoint,
    SchemaCreation, TableChange, UserModel, ViewReplacement,
};
pub use dml::{ColumnValue, RowSql, Table, TypedColumn};
pub use format::{FormatError, FormatOptions, Formatted, KeywordCase, format_ranges, same_meaning};
pub use keywords::{is_keyword, keywords};
pub use lexer::{Token, TokenKind, first_word, tokenize};
pub use parameters::{
    MissingValue, Parameter, ParameterBinding, ParameterValues, Prepared, bind, inline,
    parameter_names, parameters, values_comment,
};
pub use password::ScramVerifier;
pub use projection::{ProjectionItem, projection};
pub use safety::{Hazard, hazard};
pub use split::{split_statements, statement_at};
pub use transaction::{commits_implicitly, controls_transactions};
