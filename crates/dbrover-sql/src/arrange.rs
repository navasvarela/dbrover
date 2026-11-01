//! Sorting and filtering a statement's rows in the database, by wrapping it in a query of its
//! own:
//!
//! ```sql
//! SELECT * FROM (
//! <the statement>
//! ) AS dbrover_rows
//! WHERE <any column contains the text> ORDER BY <column position> DESC
//! ```
//!
//! The filter means what the grid's own filter means: a row where any value contains the text,
//! ignoring case. `NULL` contains nothing.

use crate::{Dialect, TokenKind, first_word, tokenize};

/// What the wrapping query calls the statement's rows.
const ALIAS: &str = "dbrover_rows";

/// Statements whose rows a query can select from.
const WRAPPABLE: &[&str] = &["select", "with", "values"];

/// The column to sort by, by its position in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOrder {
    pub column: usize,
    pub descending: bool,
}

/// How to sort and filter a statement's rows.
#[derive(Debug, Clone, Copy)]
pub struct Arrangement<'a> {
    /// The statement's result columns, in order.
    pub columns: &'a [String],
    pub sort: Option<SortOrder>,
    /// Text that a row must contain in some value; blank for every row.
    pub filter: &'a str,
}

/// The wrapping query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arranged {
    pub sql: String,
    /// How many bytes come before the statement in `sql`, to place the database's error
    /// positions back in the statement.
    pub prefix_len: usize,
}

/// Whether the statement's rows can be sorted and filtered in the database; `Err` says why not.
pub fn can_arrange(sql: &str, dialect: Dialect, columns: &[String]) -> Result<(), String> {
    if dialect == Dialect::Cassandra {
        return Err(
            "CQL can't select from a query, so rows can't be sorted and filtered in the \
                    database"
                .to_owned(),
        );
    }
    let first = first_word(sql, dialect).unwrap_or_default();
    if !WRAPPABLE.contains(&first.as_str()) {
        return Err(
            "Only the rows of a SELECT, WITH or VALUES can be sorted and filtered in \
                    the database"
                .to_owned(),
        );
    }
    if first == "with" && dialect == Dialect::SqlServer {
        return Err("SQL Server can't select from a statement that starts with WITH".to_owned());
    }
    let repeated = columns
        .iter()
        .enumerate()
        .any(|(index, name)| columns[..index].contains(name));
    if repeated {
        return Err(
            "Two columns have the same name, so the database can't tell them apart".to_owned(),
        );
    }
    Ok(())
}

/// The statement wrapped to sort and filter its rows as `arrangement` says.
///
/// # Errors
///
/// Why the statement can't be wrapped, as [`can_arrange`] says.
pub fn arrange(
    sql: &str,
    dialect: Dialect,
    arrangement: &Arrangement<'_>,
) -> Result<Arranged, String> {
    can_arrange(sql, dialect, arrangement.columns)?;
    // The statement on lines of its own, so a line comment inside it ends where it did.
    let prefix = "SELECT * FROM (\n";
    let mut wrapped = format!(
        "{prefix}{}\n) AS {ALIAS}",
        without_trailing_semicolons(sql, dialect)
    );
    let needle = arrangement.filter.trim().to_lowercase();
    if !needle.is_empty() {
        let quoting = dialect.quoting();
        let literal = quoting.string_literal(&needle);
        let conditions: Vec<String> = arrangement
            .columns
            .iter()
            .map(|column| {
                let value = format!("{ALIAS}.{}", quoting.identifier(column));
                dialect.contains_text(&value, &literal)
            })
            .collect();
        wrapped.push_str(&format!("\nWHERE {}", conditions.join(" OR ")));
    }
    if let Some(sort) = arrangement.sort {
        let direction = if sort.descending { "DESC" } else { "ASC" };
        // By position, which needs no name and works whatever the column is called.
        wrapped.push_str(&format!("\nORDER BY {} {direction}", sort.column + 1));
    }
    Ok(Arranged {
        sql: wrapped,
        prefix_len: prefix.len(),
    })
}

/// The statement up to its last token that isn't a semicolon, whitespace or a comment: a
/// semicolon would end the statement inside the wrapper, and a line comment after the last
/// token would swallow what follows it.
fn without_trailing_semicolons(sql: &str, dialect: Dialect) -> &str {
    let end = tokenize(sql, dialect)
        .into_iter()
        .rev()
        .find(|token| !token.kind.is_trivia() && token.kind != TokenKind::Semicolon)
        .map_or(0, |token| token.range.end);
    &sql[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn wraps_a_statement_with_its_filter_and_sort_in_every_dialect() {
        let columns = columns(&["id", "Note"]);
        let arrangement = Arrangement {
            columns: &columns,
            sort: Some(SortOrder {
                column: 1,
                descending: true,
            }),
            filter: " It's ",
        };
        let expected = [
            (
                Dialect::Postgres,
                "WHERE strpos(lower(CAST(dbrover_rows.\"id\" AS text)), 'it''s') > 0 \
                 OR strpos(lower(CAST(dbrover_rows.\"Note\" AS text)), 'it''s') > 0",
            ),
            (
                Dialect::Sqlite,
                "WHERE instr(lower(CAST(dbrover_rows.\"id\" AS TEXT)), 'it''s') > 0 \
                 OR instr(lower(CAST(dbrover_rows.\"Note\" AS TEXT)), 'it''s') > 0",
            ),
            (
                Dialect::Mysql,
                "WHERE LOCATE('it''s', LOWER(CAST(dbrover_rows.`id` AS CHAR))) > 0 \
                 OR LOCATE('it''s', LOWER(CAST(dbrover_rows.`Note` AS CHAR))) > 0",
            ),
            (
                Dialect::SqlServer,
                "WHERE CHARINDEX(N'it''s', LOWER(CAST(dbrover_rows.[id] AS NVARCHAR(MAX)))) > 0 \
                 OR CHARINDEX(N'it''s', LOWER(CAST(dbrover_rows.[Note] AS NVARCHAR(MAX)))) > 0",
            ),
            (
                Dialect::DuckDb,
                "WHERE strpos(lower(CAST(dbrover_rows.\"id\" AS VARCHAR)), 'it''s') > 0 \
                 OR strpos(lower(CAST(dbrover_rows.\"Note\" AS VARCHAR)), 'it''s') > 0",
            ),
            (
                Dialect::ClickHouse,
                "WHERE positionUTF8(lowerUTF8(toString(dbrover_rows.`id`)), 'it''s') > 0 \
                 OR positionUTF8(lowerUTF8(toString(dbrover_rows.`Note`)), 'it''s') > 0",
            ),
        ];
        for (dialect, filter) in expected {
            let arranged = arrange("SELECT id, note AS \"Note\" FROM t", dialect, &arrangement)
                .unwrap_or_else(|error| panic!("{dialect:?}: {error}"));
            assert_eq!(
                arranged.sql,
                format!(
                    "SELECT * FROM (\nSELECT id, note AS \"Note\" FROM t\n) AS dbrover_rows\n\
                     {filter}\nORDER BY 2 DESC"
                ),
                "{dialect:?}"
            );
            assert_eq!(&arranged.sql[arranged.prefix_len..][..6], "SELECT");
        }
    }

    #[test]
    fn leaves_out_what_isnt_asked_for_and_drops_trailing_semicolons() {
        let columns = columns(&["n"]);
        let arrangement = Arrangement {
            columns: &columns,
            sort: None,
            filter: "  ",
        };
        let cases = [
            ("select 1 as n;", "select 1 as n"),
            ("select 1 as n ; ;\n", "select 1 as n"),
            ("select 1 as n -- the answer", "select 1 as n"),
            (
                "select 1 -- the answer\n as n",
                "select 1 -- the answer\n as n",
            ),
            ("select ';' as n; -- done", "select ';' as n"),
        ];
        for (sql, kept) in cases {
            assert_eq!(
                arrange(sql, Dialect::Postgres, &arrangement).unwrap().sql,
                format!("SELECT * FROM (\n{kept}\n) AS dbrover_rows"),
                "{sql}"
            );
        }
    }

    #[test]
    fn says_which_statements_it_cant_wrap() {
        let unique = columns(&["a", "b"]);
        assert!(
            can_arrange(
                "WITH x AS (SELECT 1) SELECT * FROM x",
                Dialect::Postgres,
                &unique
            )
            .is_ok()
        );
        assert!(can_arrange("values (1, 2)", Dialect::Sqlite, &unique).is_ok());
        for (sql, dialect, columns) in [
            ("SHOW TABLES", Dialect::Mysql, unique.clone()),
            ("EXPLAIN SELECT 1", Dialect::Postgres, unique.clone()),
            (
                "WITH x AS (SELECT 1 AS a) SELECT * FROM x",
                Dialect::SqlServer,
                unique.clone(),
            ),
            (
                "SELECT a.id, b.id FROM a, b",
                Dialect::Postgres,
                columns(&["id", "id"]),
            ),
            ("SELECT * FROM t", Dialect::Cassandra, unique.clone()),
        ] {
            assert!(can_arrange(sql, dialect, &columns).is_err(), "{sql}");
        }
    }
}
