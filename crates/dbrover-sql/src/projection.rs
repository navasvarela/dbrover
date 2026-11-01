//! The columns a query returns, read from its text: what completion offers for a CTE or a
//! subquery in `FROM`.

use sqlparser::ast::{
    Expr, ObjectNamePart, Query, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, Statement,
};
use sqlparser::parser::Parser;

use crate::dialect::Dialect;
use crate::lexer::{Token, TokenKind, tokenize};

/// One item of a query's select list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionItem {
    /// A column with a name: `a`, `t.a`, `expr AS a`.
    Named(String),
    /// `*`, or `t.*` with its qualifier: the columns of the query's tables.
    Wildcard { qualifier: Option<String> },
    /// An expression without an alias, whose name the database makes up.
    Unnamed,
}

/// The select list of `query` (a `SELECT`, `WITH … SELECT` or `VALUES`). Read by
/// `sqlparser` when it parses the query, which is the usual case for a finished CTE; otherwise
/// (a query still being typed) from its tokens, between the first `SELECT` and its `FROM`.
pub fn projection(query: &str, dialect: Dialect) -> Vec<ProjectionItem> {
    let parsed = dialect
        .sqlparser_dialect()
        .and_then(|parser| Parser::parse_sql(parser, query).ok());
    match parsed.as_deref() {
        Some([Statement::Query(parsed)]) => from_query(parsed),
        _ => from_tokens(query, dialect),
    }
}

fn from_query(query: &Query) -> Vec<ProjectionItem> {
    from_set_expr(&query.body)
}

fn from_set_expr(body: &SetExpr) -> Vec<ProjectionItem> {
    match body {
        SetExpr::Select(select) => select.projection.iter().map(from_select_item).collect(),
        SetExpr::Query(query) => from_query(query),
        // A set operation's columns are named by its first query.
        SetExpr::SetOperation { left, .. } => from_set_expr(left),
        // Both databases name `VALUES` columns `column1`, `column2`, …
        SetExpr::Values(values) => values
            .rows
            .first()
            .map(|row| {
                (1..=row.content.len())
                    .map(|n| ProjectionItem::Named(format!("column{n}")))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn from_select_item(item: &SelectItem) -> ProjectionItem {
    match item {
        SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
            ProjectionItem::Named(ident.value.clone())
        }
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(idents)) => {
            idents.last().map_or(ProjectionItem::Unnamed, |ident| {
                ProjectionItem::Named(ident.value.clone())
            })
        }
        SelectItem::ExprWithAlias { alias, .. } => ProjectionItem::Named(alias.value.clone()),
        SelectItem::Wildcard(_) => ProjectionItem::Wildcard { qualifier: None },
        SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) => {
            ProjectionItem::Wildcard {
                qualifier: name
                    .0
                    .last()
                    .and_then(ObjectNamePart::as_ident)
                    .map(|ident| ident.value.clone()),
            }
        }
        _ => ProjectionItem::Unnamed,
    }
}

/// The select list from tokens: each top-level comma-separated item between the first
/// `SELECT` and the `FROM` after it (or the end).
fn from_tokens(query: &str, dialect: Dialect) -> Vec<ProjectionItem> {
    let tokens: Vec<Token> = tokenize(query, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let text = |token: &Token| &query[token.range.clone()];
    let is_word = |token: &Token, word: &str| {
        token.kind == TokenKind::Word && text(token).eq_ignore_ascii_case(word)
    };
    let Some(select) = tokens.iter().position(|token| is_word(token, "select")) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut current: Vec<&Token> = Vec::new();
    let mut depth = 0usize;
    for token in &tokens[select + 1..] {
        match text(token) {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && is_word(token, "from") {
            break;
        }
        if depth == 0 && text(token) == "," {
            items.push(item_from_tokens(query, &current));
            current.clear();
        } else if !(current.is_empty() && (is_word(token, "distinct") || is_word(token, "all"))) {
            current.push(token);
        }
    }
    if !current.is_empty() {
        items.push(item_from_tokens(query, &current));
    }
    items
}

fn item_from_tokens(query: &str, tokens: &[&Token]) -> ProjectionItem {
    let text = |token: &Token| &query[token.range.clone()];
    let name = |token: &Token| match token.kind {
        TokenKind::Word => Some(text(token).to_owned()),
        TokenKind::QuotedIdentifier => {
            let written = text(token);
            written
                .get(1..written.len().saturating_sub(1))
                .map(str::to_owned)
        }
        _ => None,
    };
    match tokens {
        [star] if text(star) == "*" => ProjectionItem::Wildcard { qualifier: None },
        [qualifier, dot, star] if text(dot) == "." && text(star) == "*" => {
            ProjectionItem::Wildcard {
                qualifier: name(qualifier),
            }
        }
        [column] => name(column).map_or(ProjectionItem::Unnamed, ProjectionItem::Named),
        [_, dot, column] if text(dot) == "." => {
            name(column).map_or(ProjectionItem::Unnamed, ProjectionItem::Named)
        }
        // `expr AS name`, `expr name`, `a.b.c`.
        [.., before, last]
            if before.kind != TokenKind::Punctuation || matches!(text(before), ")" | ".") =>
        {
            name(last).map_or(ProjectionItem::Unnamed, ProjectionItem::Named)
        }
        _ => ProjectionItem::Unnamed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(items: Vec<ProjectionItem>) -> Vec<String> {
        items
            .into_iter()
            .map(|item| match item {
                ProjectionItem::Named(name) => name,
                ProjectionItem::Wildcard { qualifier } => {
                    format!("{}*", qualifier.unwrap_or_default())
                }
                ProjectionItem::Unnamed => "?".to_owned(),
            })
            .collect()
    }

    #[test]
    fn reads_the_select_list_with_the_parser_or_from_tokens() {
        let cases: [(&str, &[&str]); 7] = [
            (
                "select id, o.customer, total * 2 as doubled, count(*) from orders o",
                &["id", "customer", "doubled", "?"],
            ),
            ("select o.*, * from orders o", &["o*", "*"]),
            ("select a from t union select b from u", &["a"]),
            ("values (1, 'a'), (2, 'b')", &["column1", "column2"]),
            (
                "with x as (select 1 as one) select one, \"Two\" from x",
                &["one", "Two"],
            ),
            // Half-typed, so the tokens are read instead.
            (
                "select id, amount * 2 doubled, t.name, count(*) from t where",
                &["id", "doubled", "name", "?"],
            ),
            ("select distinct a, (b) c", &["a", "c"]),
        ];
        for (query, expected) in cases {
            assert_eq!(
                named(projection(query, Dialect::Postgres)),
                expected,
                "{query}"
            );
        }
    }
}
