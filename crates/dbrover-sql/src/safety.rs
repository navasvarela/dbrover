use crate::dialect::Dialect;
use crate::lexer::{TokenKind, tokenize};

/// A statement that changes every row of a table, which is rarely what was meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hazard {
    UpdateWithoutWhere,
    DeleteWithoutWhere,
    /// CQL's `TRUNCATE`, its only way to change every row: it refuses an `UPDATE` or a
    /// `DELETE` without `WHERE`.
    Truncate,
}

impl Hazard {
    pub fn description(self) -> &'static str {
        match self {
            Self::UpdateWithoutWhere => "UPDATE without WHERE changes every row of the table.",
            Self::DeleteWithoutWhere => "DELETE without WHERE removes every row of the table.",
            Self::Truncate => "TRUNCATE removes every row of the table.",
        }
    }
}

/// Checks one statement for an `UPDATE` or `DELETE` without a top-level `WHERE`, and in CQL
/// for a `TRUNCATE`.
///
/// Only looks at the statement's first word, so `WITH … DELETE …` isn't caught; a `WHERE`
/// inside a subquery doesn't count as the statement's own.
pub fn hazard(statement: &str, dialect: Dialect) -> Option<Hazard> {
    let mut words = Vec::new();
    let mut paren_depth = 0usize;
    for token in tokenize(statement, dialect) {
        match token.kind {
            TokenKind::Punctuation if &statement[token.range.clone()] == "(" => paren_depth += 1,
            TokenKind::Punctuation if &statement[token.range.clone()] == ")" => {
                paren_depth = paren_depth.saturating_sub(1);
            }
            TokenKind::Word => {
                words.push((statement[token.range].to_ascii_lowercase(), paren_depth))
            }
            _ => {}
        }
    }

    let hazard = match words.first().map(|(word, _)| word.as_str()) {
        Some("update") => Hazard::UpdateWithoutWhere,
        Some("delete") => Hazard::DeleteWithoutWhere,
        Some("truncate") if dialect == Dialect::Cassandra => return Some(Hazard::Truncate),
        _ => return None,
    };
    let has_where = words
        .iter()
        .any(|(word, depth)| word == "where" && *depth == 0);
    (!has_where).then_some(hazard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_only_unfiltered_updates_and_deletes() {
        let cases = [
            ("UPDATE t SET a = 1", Some(Hazard::UpdateWithoutWhere)),
            ("delete from t", Some(Hazard::DeleteWithoutWhere)),
            ("delete from t where id in (select id from u)", None),
            (
                "delete from t using (select id from u where x) s",
                Some(Hazard::DeleteWithoutWhere),
            ),
            (
                "update t set note = 'where'",
                Some(Hazard::UpdateWithoutWhere),
            ),
            ("UPDATE t SET a = 1 WHERE id = 2", None),
            ("select * from t", None),
        ];

        for (sql, expected) in cases {
            assert_eq!(hazard(sql, Dialect::Postgres), expected, "{sql}");
        }
    }

    #[test]
    fn flags_truncate_in_cql_only() {
        assert_eq!(
            hazard("TRUNCATE TABLE shop.orders", Dialect::Cassandra),
            Some(Hazard::Truncate)
        );
        assert_eq!(hazard("truncate t", Dialect::Postgres), None);
        assert_eq!(
            hazard("DELETE FROM t WHERE id = 1", Dialect::Cassandra),
            None
        );
    }
}
