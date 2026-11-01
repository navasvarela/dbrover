//! Lint rules: statements that run, but rarely do what was meant.

use std::ops::Range;

use super::{Problem, ProblemSource, Severity};
use crate::dialect::Dialect;
use crate::lexer::{Token, TokenKind, first_word, tokenize};
use crate::safety::{Hazard, hazard};

/// A lint rule. Its id is what `settings.toml` names to switch it off, so ids never change
/// once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LintRule {
    NullComparison,
    UpdateWithoutWhere,
    DeleteWithoutWhere,
    SelectStarInInsert,
    /// A table the cached catalog doesn't have. Checked by the app, which has the catalog.
    UnknownTable,
    /// A column the cached catalog doesn't have. Checked by the app, which has the catalog.
    UnknownColumn,
}

impl LintRule {
    pub const ALL: [Self; 6] = [
        Self::NullComparison,
        Self::UpdateWithoutWhere,
        Self::DeleteWithoutWhere,
        Self::SelectStarInInsert,
        Self::UnknownTable,
        Self::UnknownColumn,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::NullComparison => "null-comparison",
            Self::UpdateWithoutWhere => "update-without-where",
            Self::DeleteWithoutWhere => "delete-without-where",
            Self::SelectStarInInsert => "select-star-in-insert",
            Self::UnknownTable => "unknown-table",
            Self::UnknownColumn => "unknown-column",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.id() == id)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::NullComparison => "= NULL or <> NULL, which is never true, instead of IS NULL",
            Self::UpdateWithoutWhere => "UPDATE without WHERE, which changes every row",
            Self::DeleteWithoutWhere => "DELETE without WHERE, which removes every row",
            Self::SelectStarInInsert => {
                "INSERT … SELECT *, which depends on the order of the source's columns"
            }
            Self::UnknownTable => "A table or view the explorer's loaded catalog doesn't have",
            Self::UnknownColumn => "A column the explorer's loaded catalog doesn't have",
        }
    }
}

pub(super) fn problems(statement: &str, dialect: Dialect) -> Vec<Problem> {
    let tokens: Vec<Token> = tokenize(statement, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let mut problems = null_comparisons(statement, &tokens);
    problems.extend(missing_where(statement, dialect, &tokens));
    problems.extend(select_star_in_insert(statement, dialect, &tokens));
    problems
}

fn warning(rule: LintRule, range: Range<usize>, message: impl Into<String>) -> Problem {
    Problem {
        range,
        severity: Severity::Warning,
        message: message.into(),
        source: ProblemSource::Lint(rule),
    }
}

fn word(statement: &str, token: &Token) -> Option<String> {
    (token.kind == TokenKind::Word).then(|| statement[token.range.clone()].to_ascii_lowercase())
}

fn is_punctuation(statement: &str, token: &Token, text: &str) -> bool {
    token.kind == TokenKind::Punctuation && &statement[token.range.clone()] == text
}

/// `a = NULL` and `a <> NULL` (or `!=`), outside the assignments of an `UPDATE … SET`.
fn null_comparisons(statement: &str, tokens: &[Token]) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut depth = 0usize;
    // The depth of the `SET` whose assignments are being read.
    let mut assignments: Option<usize> = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        match word(statement, token).as_deref() {
            Some("set") => assignments = Some(depth),
            Some("where" | "from" | "returning") if assignments == Some(depth) => {
                assignments = None;
            }
            _ => {}
        }
        if is_punctuation(statement, token, "(") {
            depth += 1;
        } else if is_punctuation(statement, token, ")") {
            depth = depth.saturating_sub(1);
        }
        let Some((operator, end)) = operator_at(statement, tokens, index) else {
            index += 1;
            continue;
        };
        let is_null = |at: Option<&Token>| {
            at.and_then(|token| word(statement, token)).as_deref() == Some("null")
        };
        let null_after = is_null(tokens.get(end));
        let null_before = index
            .checked_sub(1)
            .is_some_and(|before| is_null(tokens.get(before)));
        let comparison = matches!(operator.as_str(), "=" | "<>" | "!=");
        if comparison && (null_after || null_before) && assignments.is_none() {
            let start = if null_before {
                tokens[index - 1].range.start
            } else {
                token.range.start
            };
            let stop = if null_after {
                tokens[end].range.end
            } else {
                tokens[end - 1].range.end
            };
            let fix = if operator == "=" {
                "IS NULL"
            } else {
                "IS NOT NULL"
            };
            problems.push(warning(
                LintRule::NullComparison,
                start..stop,
                format!("{operator} NULL is never true; use {fix}"),
            ));
        }
        index = end;
    }
    problems
}

/// The operator written at `index` (operator characters with no space between them) and the
/// index after it, or `None` if no operator starts there.
fn operator_at(statement: &str, tokens: &[Token], index: usize) -> Option<(String, usize)> {
    let is_operator = |token: &Token| {
        token.kind == TokenKind::Punctuation
            && statement[token.range.clone()]
                .chars()
                .all(|c| "<>=!:".contains(c))
    };
    if !is_operator(tokens.get(index)?) {
        return None;
    }
    let mut end = index + 1;
    while tokens
        .get(end)
        .is_some_and(|token| is_operator(token) && token.range.start == tokens[end - 1].range.end)
    {
        end += 1;
    }
    let operator = statement[tokens[index].range.start..tokens[end - 1].range.end].to_owned();
    Some((operator, end))
}

fn missing_where(statement: &str, dialect: Dialect, tokens: &[Token]) -> Option<Problem> {
    let found = hazard(statement, dialect)?;
    let rule = match found {
        Hazard::UpdateWithoutWhere => LintRule::UpdateWithoutWhere,
        Hazard::DeleteWithoutWhere => LintRule::DeleteWithoutWhere,
        // Emptying the table is what a `TRUNCATE` is for: it is warned of before it runs, not
        // as it is typed.
        Hazard::Truncate => return None,
    };
    let verb = tokens.first()?;
    Some(warning(rule, verb.range.clone(), found.description()))
}

/// `INSERT … SELECT *`: the source's columns land in the target's in whatever order the source
/// has them.
fn select_star_in_insert(statement: &str, dialect: Dialect, tokens: &[Token]) -> Option<Problem> {
    let verb = first_word(statement, dialect)?;
    if !dialect.insert_verbs().contains(&verb.as_str()) {
        return None;
    }
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if is_punctuation(statement, token, "(") {
            depth += 1;
        } else if is_punctuation(statement, token, ")") {
            depth = depth.saturating_sub(1);
        }
        if depth > 0 || word(statement, token).as_deref() != Some("select") {
            continue;
        }
        let mut next = index + 1;
        while tokens
            .get(next)
            .and_then(|token| word(statement, token))
            .is_some_and(|word| word == "distinct" || word == "all")
        {
            next += 1;
        }
        let star = tokens
            .get(next)
            .filter(|token| is_punctuation(statement, token, "*"))?;
        return Some(warning(
            LintRule::SelectStarInInsert,
            star.range.clone(),
            "INSERT … SELECT * depends on the order of the source's columns; list them",
        ));
    }
    None
}
