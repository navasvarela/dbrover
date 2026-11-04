//! A `[combine]`'s PRQL, compiled to the DuckDB SQL the run sends (`docs/reporting.md`, "PRQL
//! in `[combine]`").

use dbrover_sql::{Dialect, TokenKind, tokenize};
use prqlc::{DisplayOptions, ErrorMessage, Options, Target};

/// `prql` as DuckDB's SQL, laid out on several lines as `prqlc` lays it out, its `$name`
/// parameters written as the `:name` the run binds.
///
/// # Errors
///
/// What `prqlc` found, a sentence an error, each with the place in `prql` it points at.
pub(crate) fn compile(prql: &str) -> Result<String, Vec<String>> {
    let options = Options::default()
        .with_target(Target::Sql(Some(prqlc::sql::Dialect::DuckDb)))
        .with_display(DisplayOptions::Plain)
        .no_signature();
    prqlc::compile(prql, &options)
        .map(|sql| bind_parameters(sql.trim_end()))
        .map_err(|errors| {
            errors
                .inner
                .iter()
                .map(|error| describe(prql, error))
                .collect()
        })
}

/// `sql` with each `$name` written as `:name`. PRQL's parameters are `$name`, which `prqlc`
/// keeps as they are; DbRover's are `:name`, found and bound by `dbrover-sql`. The lexer tells a
/// parameter from a `$` in a string or a quoted name, and `$1`'s number isn't a name.
fn bind_parameters(sql: &str) -> String {
    let tokens = tokenize(sql, Dialect::DuckDb);
    let mut bound = String::with_capacity(sql.len());
    for (index, token) in tokens.iter().enumerate() {
        let text = &sql[token.range.clone()];
        let names_parameter = tokens.get(index + 1).is_some_and(|next| {
            next.kind == TokenKind::Word && next.range.start == token.range.end
        });
        if token.kind == TokenKind::Punctuation && text == "$" && names_parameter {
            bound.push(':');
        } else {
            bound.push_str(text);
        }
    }
    bound
}

/// One of `prqlc`'s errors, as a file's problem: where it is in the PRQL, the line itself with a
/// caret under the place, and `prqlc`'s hints.
fn describe(prql: &str, error: &ErrorMessage) -> String {
    let mut sentence = match &error.location {
        Some(location) => {
            let (line, column) = location.start;
            let mut placed = format!(
                "prql, line {}, column {}: {}",
                line + 1,
                column + 1,
                error.reason
            );
            if let Some(text) = prql.lines().nth(line) {
                let caret: String = text
                    .chars()
                    .take(column)
                    .map(|c| if c == '\t' { '\t' } else { ' ' })
                    .collect();
                placed.push_str(&format!("\n    {text}\n    {caret}^"));
            }
            placed
        }
        None => format!("prql: {}", error.reason),
    };
    for hint in &error.hints {
        sentence.push_str(&format!("\n    {hint}"));
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_to_duckdb_sql() {
        let sql = compile("from lines\nfilter amount > 0\nsort {-amount}\ntake 3").unwrap();
        let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            one_line,
            "SELECT * FROM lines WHERE amount > 0 ORDER BY amount DESC LIMIT 3"
        );
    }

    #[test]
    fn writes_parameters_as_the_run_binds_them() {
        let cases = [
            (
                "from tx | filter executed_at >= $since && executed_at < $until",
                "executed_at >= :since AND executed_at < :until",
            ),
            // A `$` in a string is text.
            (
                "from tx | filter note == 'costs $since'",
                "note = 'costs $since'",
            ),
            // A `prql target:` line naming another dialect doesn't change it: TOP would be SQL
            // Server's.
            ("prql target:sql.mssql\nfrom tx | take 3", "LIMIT 3"),
        ];
        for (prql, expected) in cases {
            let sql = compile(prql).unwrap();
            let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(one_line.contains(expected), "{prql}: {one_line}");
        }
    }

    #[test]
    fn points_into_the_prql() {
        let problems = compile("from lines\nderive net = amount *\n").unwrap_err();
        let [problem] = problems.as_slice() else {
            panic!("{problems:?}");
        };
        assert!(problem.starts_with("prql, line 2, column "), "{problem}");
        assert!(
            problem.contains("\n    derive net = amount *\n"),
            "{problem}"
        );
        let caret = problem.lines().nth(2).unwrap();
        assert!(caret.trim_start().starts_with('^'), "{problem}");
    }

    #[test]
    fn says_an_unknown_name_and_its_hint() {
        let problems = compile("from lines\nselect {id}\nfilter amont > 0").unwrap_err();
        let problem = problems.join("\n");
        assert!(problem.contains("prql, line 3"), "{problem}");
        assert!(problem.contains("amont"), "{problem}");
    }
}
