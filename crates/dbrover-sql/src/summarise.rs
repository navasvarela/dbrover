//! The statements **Summarise** writes below the one whose result is on screen
//! (`docs/charts-as-code.md`, CH5f): a distribution of a column of numbers into buckets, and
//! its percentiles.
//!
//! Both read the statement that produced the result, wrapped as a common table expression, so
//! they see **every row it returns** rather than the page the grid happens to hold. That is the
//! whole reason the affordance writes SQL instead of binning what was fetched: a histogram of a
//! first page is a histogram of a first page, whatever the table holds. Nothing here looks at a
//! row; the range comes from a `MIN`/`MAX` the engine works out.

use crate::ddl::quote_name;
use crate::dialect::Dialect;

/// How many buckets **Distribution** offers, as the menu lists them.
pub const BUCKET_COUNTS: [u32; 3] = [10, 20, 50];

/// The percentiles a summary reports, in hundredths, after the smallest value and before the
/// largest.
const PERCENTILES: [u32; 4] = [50, 90, 95, 99];

/// The name the wrapped statement goes under. Long enough not to collide with a table of the
/// user's by accident, and it reads as what it is in the statement below.
const ROWS: &str = "summarised";

/// A summary of one column of numbers, as a statement of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    /// Each bucket's lower bound and how many rows fall in it, which charts as bars with no
    /// further work.
    Distribution { buckets: u32 },
    /// The median, p90, p95 and p99 with the smallest and largest value, a row each so they
    /// chart.
    Percentiles,
}

/// A statement that summarises a column, and the chart it opens as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summarised {
    /// The statement, without a trailing semicolon or line break.
    pub sql: String,
    /// The `[chart]` block's keys, as TOML without comment markers: the console adds its
    /// language's.
    pub chart: String,
}

impl Summary {
    /// The item's text in the **Summarise** menu.
    pub fn label(self) -> String {
        match self {
            Self::Distribution { buckets } => format!("Distribution into {buckets} buckets"),
            Self::Percentiles => "Percentiles".to_owned(),
        }
    }

    /// Why `dialect` can't be asked for this summary, if it can't: the reason a disabled menu
    /// item gives in its tooltip.
    ///
    /// Better a disabled item with a reason than a statement that fails when it runs.
    pub fn refusal(self, dialect: Dialect) -> Option<String> {
        match self {
            // CQL has no subqueries, no expressions in `GROUP BY` and no `FLOOR`, so there is
            // no statement to write; the rest can all do the arithmetic.
            Self::Distribution { .. } => (dialect == Dialect::Cassandra).then(|| {
                "CQL can't group by an expression, so it can't bucket a column".to_owned()
            }),
            Self::Percentiles => percentiles_of(dialect).is_none().then(|| {
                format!(
                    "{} has no percentile function; a distribution still works",
                    engine_name(dialect)
                )
            }),
        }
    }

    /// The statement that summarises `column` of the rows `statement` returns, with the chart
    /// it opens as. `None` where [`Self::refusal`] says the engine can't.
    pub fn sql(self, dialect: Dialect, statement: &str, column: &str) -> Option<Summarised> {
        if self.refusal(dialect).is_some() {
            return None;
        }
        let name = quote_name(dialect, column);
        let wrapped = wrapped(statement);
        Some(match self {
            Self::Distribution { buckets } => distribution(dialect, &wrapped, &name, buckets),
            Self::Percentiles => percentiles(dialect, &wrapped, &name, column),
        })
    }
}

/// `statement` as the body of the `summarised` common table expression: its own text, trimmed
/// of trailing semicolons and whitespace, indented by two spaces so the statement below reads
/// as one piece.
fn wrapped(statement: &str) -> String {
    let body = statement.trim().trim_end_matches(';').trim_end();
    let indented: Vec<String> = body
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect();
    format!("WITH {ROWS} AS (\n{}\n)", indented.join("\n"))
}

/// Each bucket's lower bound and its count.
///
/// The bucket is worked out in a table expression of its own and grouped by in the next, rather
/// than grouped by the expression or by `GROUP BY 1`: an ordinal in `GROUP BY` is not SQL
/// Server's, and repeating a long expression there reads worse than naming it once. The width
/// divides by a decimal literal so an integer column doesn't divide as integers, and the
/// largest value is pulled back into the last bucket rather than opening one of its own.
fn distribution(dialect: Dialect, wrapped: &str, column: &str, buckets: u32) -> Summarised {
    let last = buckets - 1;
    let which = floor(
        dialect,
        &format!("({ROWS}.{column} - bounds.low) / bounds.width"),
    );
    let sql = format!(
        "{wrapped}, bounds AS (
  SELECT MIN({column}) AS low, (MAX({column}) - MIN({column})) / {buckets}.0 AS width
  FROM {ROWS}
), bucketed AS (
  SELECT bounds.low + bounds.width * CASE
           WHEN {which} > {last} THEN {last}
           ELSE {which}
         END AS bucket
  FROM {ROWS} CROSS JOIN bounds
  WHERE {ROWS}.{column} IS NOT NULL AND bounds.width > 0
)
SELECT bucket, COUNT(*) AS row_count
FROM bucketed
GROUP BY bucket
ORDER BY bucket"
    );
    Summarised {
        sql,
        chart: "[chart]\nkind = \"bars\"\nx = \"bucket\"\ny = \"row_count\"".to_owned(),
    }
}

/// The smallest value, the percentiles, and the largest, a row each.
fn percentiles(dialect: Dialect, wrapped: &str, column: &str, name: &str) -> Summarised {
    let shape = percentiles_of(dialect).unwrap_or(Percentiles {
        expression: "NULL",
        distinct: false,
    });
    let select = if shape.distinct {
        "SELECT DISTINCT"
    } else {
        "SELECT"
    };
    let mut rows = vec![format!(
        "SELECT 'smallest' AS statistic, MIN({column}) AS value FROM {ROWS}"
    )];
    rows.extend(PERCENTILES.map(|percent| {
        let expression = shape
            .expression
            .replace("{column}", column)
            .replace("{share}", &share(percent))
            .replace("{percent}", &percent.to_string());
        format!("{select} 'p{percent}', {expression} FROM {ROWS}")
    }));
    rows.push(format!("SELECT 'largest', MAX({column}) FROM {ROWS}"));
    let sql = format!("{wrapped}\n{}", rows.join("\nUNION ALL\n"));
    Summarised {
        sql,
        chart: format!(
            "[chart]\nkind = \"bars\"\nx = \"statistic\"\ny = \"value\"\ntitle = \"Percentiles of {name}\""
        ),
    }
}

/// A percentile in hundredths as a share: 50 is `0.5`, 95 is `0.95`.
fn share(percent: u32) -> String {
    format!("0.{percent:02}")
}

/// How a dialect asks for a percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Percentiles {
    /// The expression, with `{column}`, `{share}` (`0.95`) and `{percent}` (`95`) to fill in.
    expression: &'static str,
    /// The select needs `DISTINCT`, because the engine's percentile is a window function over
    /// every row rather than an aggregate of them, so it answers once per row.
    distinct: bool,
}

/// `expression` rounded down to a whole number, which is where a value falls among the buckets.
///
/// `FLOOR` everywhere but SQLite, whose bundled build has no maths functions: `libsqlite3-sys`
/// doesn't define `SQLITE_ENABLE_MATH_FUNCTIONS`, so a statement using `FLOOR` fails with "no
/// such function" against DbRover's own SQLite, though it runs against a system `sqlite3` that
/// has them. A cast to a whole number truncates towards zero, which is a floor for a value that
/// can't be negative — and this one can't, since it is a value's distance above the smallest.
///
/// Not a cast everywhere: BigQuery's `CAST(… AS INT64)` *rounds*, so half the values would land
/// in the bucket above.
fn floor(dialect: Dialect, expression: &str) -> String {
    match dialect {
        Dialect::Sqlite => format!("CAST({expression} AS INTEGER)"),
        _ => format!("FLOOR({expression})"),
    }
}

/// `dialect`'s percentile, or `None` where it has none.
fn percentiles_of(dialect: Dialect) -> Option<Percentiles> {
    let aggregate = |expression| {
        Some(Percentiles {
            expression,
            distinct: false,
        })
    };
    match dialect {
        Dialect::Postgres | Dialect::Snowflake => {
            aggregate("percentile_cont({share}) WITHIN GROUP (ORDER BY {column})")
        }
        Dialect::DuckDb => aggregate("quantile_cont({column}, {share})"),
        Dialect::ClickHouse => aggregate("quantile({share})({column})"),
        Dialect::Trino => aggregate("approx_percentile({column}, {share})"),
        Dialect::BigQuery => aggregate("APPROX_QUANTILES({column}, 100)[OFFSET({percent})]"),
        // Transact-SQL's is a window function, so it reports the same value on every row and
        // the select has to ask for it once.
        Dialect::SqlServer => Some(Percentiles {
            expression: "PERCENTILE_CONT({share}) WITHIN GROUP (ORDER BY {column}) OVER ()",
            distinct: true,
        }),
        Dialect::Sqlite | Dialect::Mysql | Dialect::Cassandra => None,
    }
}

/// The engine's name as a refusal says it.
fn engine_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Postgres => "PostgreSQL",
        Dialect::Sqlite => "SQLite",
        Dialect::Mysql => "MySQL",
        Dialect::SqlServer => "SQL Server",
        Dialect::DuckDb => "DuckDB",
        Dialect::ClickHouse => "ClickHouse",
        Dialect::Cassandra => "Cassandra",
        Dialect::Trino => "Trino",
        Dialect::BigQuery => "BigQuery",
        Dialect::Snowflake => "Snowflake",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &str = "SELECT price FROM products WHERE price > 0";

    #[test]
    fn a_distribution_reads_every_row_the_statement_returns() {
        let written = Summary::Distribution { buckets: 20 }
            .sql(Dialect::Postgres, QUERY, "price")
            .expect("every engine but CQL buckets");

        assert_eq!(
            written.sql,
            "WITH summarised AS (
  SELECT price FROM products WHERE price > 0
), bounds AS (
  SELECT MIN(price) AS low, (MAX(price) - MIN(price)) / 20.0 AS width
  FROM summarised
), bucketed AS (
  SELECT bounds.low + bounds.width * CASE
           WHEN FLOOR((summarised.price - bounds.low) / bounds.width) > 19 THEN 19
           ELSE FLOOR((summarised.price - bounds.low) / bounds.width)
         END AS bucket
  FROM summarised CROSS JOIN bounds
  WHERE summarised.price IS NOT NULL AND bounds.width > 0
)
SELECT bucket, COUNT(*) AS row_count
FROM bucketed
GROUP BY bucket
ORDER BY bucket"
        );
        assert_eq!(
            written.chart,
            "[chart]\nkind = \"bars\"\nx = \"bucket\"\ny = \"row_count\""
        );
    }

    /// The property the affordance exists for: the range is the engine's to work out, over the
    /// statement's own rows, and nothing in the statement names a number of rows.
    #[test]
    fn the_range_is_never_a_number_the_app_worked_out() {
        for buckets in BUCKET_COUNTS {
            let written = Summary::Distribution { buckets }
                .sql(Dialect::DuckDb, QUERY, "price")
                .unwrap();
            assert!(written.sql.contains("MIN(price)") && written.sql.contains("MAX(price)"));
            assert!(
                written.sql.contains(&format!("/ {buckets}.0 AS width")),
                "{}",
                written.sql
            );
            // The statement the user ran, inside, untouched.
            assert!(written.sql.contains(QUERY), "{}", written.sql);
        }
    }

    /// SQLite's bundled build has no maths functions, so it gets a truncating cast; BigQuery's
    /// cast rounds, so it keeps `FLOOR`.
    #[test]
    fn rounding_down_uses_whatever_the_engine_has() {
        let of = |dialect| {
            Summary::Distribution { buckets: 10 }
                .sql(dialect, QUERY, "price")
                .unwrap()
                .sql
        };
        assert!(
            of(Dialect::Sqlite)
                .contains("CAST((summarised.price - bounds.low) / bounds.width AS INTEGER) > 9"),
            "{}",
            of(Dialect::Sqlite)
        );
        assert!(!of(Dialect::Sqlite).contains("FLOOR"));
        for dialect in [Dialect::Postgres, Dialect::BigQuery, Dialect::DuckDb] {
            assert!(
                of(dialect).contains("FLOOR((summarised.price - bounds.low) / bounds.width) > 9"),
                "{dialect:?}: {}",
                of(dialect)
            );
        }
    }

    #[test]
    fn a_statements_trailing_semicolon_and_blank_lines_are_tidied_away() {
        let written = Summary::Distribution { buckets: 10 }
            .sql(Dialect::Sqlite, "SELECT a\nFROM t ;  \n\n", "a")
            .unwrap();

        assert!(
            written
                .sql
                .starts_with("WITH summarised AS (\n  SELECT a\n  FROM t\n)"),
            "{}",
            written.sql
        );
    }

    #[test]
    fn percentiles_come_from_each_engines_own_function() {
        let cases = [
            (
                Dialect::Postgres,
                "SELECT 'p50', percentile_cont(0.50) WITHIN GROUP (ORDER BY price) FROM summarised",
            ),
            (
                Dialect::DuckDb,
                "SELECT 'p50', quantile_cont(price, 0.50) FROM summarised",
            ),
            (
                Dialect::ClickHouse,
                "SELECT 'p50', quantile(0.50)(price) FROM summarised",
            ),
            (
                Dialect::Trino,
                "SELECT 'p50', approx_percentile(price, 0.50) FROM summarised",
            ),
            (
                Dialect::BigQuery,
                "SELECT 'p50', APPROX_QUANTILES(price, 100)[OFFSET(50)] FROM summarised",
            ),
            (
                Dialect::SqlServer,
                "SELECT DISTINCT 'p50', PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY price) \
                 OVER () FROM summarised",
            ),
        ];
        for (dialect, expected) in cases {
            let written = Summary::Percentiles
                .sql(dialect, QUERY, "price")
                .unwrap_or_else(|| panic!("{dialect:?} has percentiles"));
            assert!(
                written.sql.contains(expected),
                "{dialect:?}: {}",
                written.sql
            );
            // The smallest and the largest frame them, a row each, so they chart.
            assert!(written.sql.contains("MIN(price) AS value"));
            assert!(written.sql.contains("SELECT 'largest', MAX(price)"));
            for percent in PERCENTILES {
                assert!(
                    written.sql.contains(&format!("'p{percent}'")),
                    "{dialect:?} reports p{percent}"
                );
            }
        }
    }

    /// Snowflake folds unquoted names to upper case, so its result's `PRICE` is written bare
    /// and a column called `price` quoted, which a bare `price` wouldn't name.
    #[test]
    fn snowflake_names_its_column_as_snowflake_folds_it() {
        let of = |column| {
            Summary::Percentiles
                .sql(Dialect::Snowflake, QUERY, column)
                .unwrap()
                .sql
        };
        assert!(
            of("PRICE").contains("percentile_cont(0.50) WITHIN GROUP (ORDER BY PRICE)"),
            "{}",
            of("PRICE")
        );
        assert!(of("price").contains("MAX(\"price\")"), "{}", of("price"));
    }

    #[test]
    fn an_engine_without_percentiles_says_so_rather_than_writing_a_statement_that_fails() {
        for dialect in [Dialect::Sqlite, Dialect::Mysql, Dialect::Cassandra] {
            assert_eq!(Summary::Percentiles.sql(dialect, QUERY, "price"), None);
            let refusal = Summary::Percentiles
                .refusal(dialect)
                .unwrap_or_else(|| panic!("{dialect:?} says why"));
            assert!(
                refusal.ends_with("has no percentile function; a distribution still works"),
                "{refusal}"
            );
        }
        // CQL can't bucket either, and says that too.
        assert_eq!(
            Summary::Distribution { buckets: 10 }.sql(Dialect::Cassandra, QUERY, "price"),
            None
        );
        assert!(
            Summary::Distribution { buckets: 10 }
                .refusal(Dialect::Cassandra)
                .is_some()
        );
        // Everything else buckets, whatever its percentiles.
        for dialect in [
            Dialect::Postgres,
            Dialect::Sqlite,
            Dialect::Mysql,
            Dialect::SqlServer,
            Dialect::DuckDb,
            Dialect::ClickHouse,
            Dialect::Trino,
            Dialect::BigQuery,
            Dialect::Snowflake,
        ] {
            assert!(
                Summary::Distribution { buckets: 10 }
                    .refusal(dialect)
                    .is_none(),
                "{dialect:?} buckets"
            );
        }
    }

    #[test]
    fn a_column_that_needs_quoting_gets_it() {
        let written = Summary::Distribution { buckets: 10 }
            .sql(Dialect::Postgres, QUERY, "order total")
            .unwrap();

        assert!(
            written.sql.contains("MIN(\"order total\")"),
            "{}",
            written.sql
        );
    }

    #[test]
    fn the_menus_items_name_what_they_write() {
        assert_eq!(
            Summary::Distribution { buckets: 50 }.label(),
            "Distribution into 50 buckets"
        );
        assert_eq!(Summary::Percentiles.label(), "Percentiles");
    }
}
