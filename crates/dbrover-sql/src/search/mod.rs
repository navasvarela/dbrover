//! The statements a data search runs (W16, `docs/data-search.md`): one per table, on the columns
//! whose types can hold the searched value, reading only the table's key and those columns.
//!
//! ```sql
//! SELECT "id", "name", "email"
//! FROM "public"."customers"
//! WHERE "name" ILIKE '%o''brien%' ESCAPE '!'
//!    OR "email" ILIKE '%o''brien%' ESCAPE '!'
//! LIMIT 101
//! ```
//!
//! Text is compared with `LIKE`, not the position functions the grid's filter uses
//! (`Dialect::contains_text`): PostgreSQL's trigram indexes, ClickHouse's n-gram skip indexes
//! and Snowflake's search optimisation answer `LIKE`, and none answers `strpos`. Numbers,
//! UUIDs and dates are compared as what they are, with `=`, which an index on them answers, so
//! `1042` is never found inside `21042`.

mod kind;
mod pattern;

use dbrover_core::{
    CaseMatch, DataSearch, ManagedService, MatchKind, QuoteEscape, Quoting, RowsPerTable,
    SearchColumn, SearchDate, SearchNumber, SearchableTable,
};

pub use kind::SearchKind;
use pattern::like_pattern;

use crate::Dialect;

/// Whether the explorer offers **Search data…** on a data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSupport {
    Unsupported,
    /// Searched on request.
    Searchable,
    /// Searched only after a question, since the engine bills each read (BigQuery, Athena) or
    /// the time its warehouse runs (Snowflake, Databricks); and only a schema or a table at a
    /// time, so one click never scans a whole project.
    AsksFirst,
}

impl Dialect {
    /// Whether a data source of this dialect, on `service`, can be searched, and how.
    /// Redshift, reached through PostgreSQL's driver, keeps its columns in views of its own
    /// and bills Serverless compute: it waits for C7.
    pub fn data_search(self, service: Option<ManagedService>) -> SearchSupport {
        match (self, service) {
            (Self::Postgres, Some(ManagedService::Redshift)) => SearchSupport::Unsupported,
            // Synapse's serverless pools bill the bytes they scan, and a warehouse's catalog
            // differs: both wait, as Redshift does.
            (
                Self::SqlServer,
                Some(
                    ManagedService::SynapseDedicated
                    | ManagedService::SynapseServerless
                    | ManagedService::Fabric,
                ),
            ) => SearchSupport::Unsupported,
            (
                Self::Postgres
                | Self::Sqlite
                | Self::DuckDb
                | Self::Mysql
                | Self::SqlServer
                | Self::ClickHouse,
                _,
            ) => SearchSupport::Searchable,
            _ => SearchSupport::Unsupported,
        }
    }
}

/// One table's statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStatement {
    /// What the search runs: at most [`RowsPerTable::asked`] rows.
    pub sql: String,
    /// The same, with no cap, for **Open all**.
    pub every_row: String,
    /// The columns it reads, in its select list's order: the key's, then the others searched.
    pub selected: Vec<String>,
    /// The columns it searches, in table order.
    pub searched: Vec<String>,
}

/// The statement that searches `table` as `search` says, or `None` when none of its columns
/// can hold the value (a table of numbers, searched for a name).
pub fn search_statement(
    dialect: Dialect,
    table: &SearchableTable,
    search: &DataSearch,
    rows: RowsPerTable,
) -> Option<SearchStatement> {
    if dialect == Dialect::Cassandra {
        return None;
    }
    let writer = Writer::new(dialect);
    let conditions: Vec<(&str, String)> = table
        .columns
        .iter()
        .filter_map(|column| {
            writer
                .condition(column, search)
                .map(|condition| (column.name.as_str(), condition))
        })
        .collect();
    if conditions.is_empty() {
        return None;
    }
    let searched: Vec<String> = conditions
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    let mut selected = table.key.clone();
    selected.extend(
        searched
            .iter()
            .filter(|name| !table.key.contains(name))
            .cloned(),
    );
    let parts = Parts {
        select: selected
            .iter()
            .map(|name| writer.names.identifier(name))
            .collect::<Vec<_>>()
            .join(", "),
        from: [
            table.database.as_deref(),
            Some(&table.name.schema),
            Some(&table.name.name),
        ]
        .into_iter()
        .flatten()
        .map(|name| writer.names.identifier(name))
        .collect::<Vec<_>>()
        .join("."),
        condition: conditions
            .into_iter()
            .map(|(_, condition)| condition)
            .collect::<Vec<_>>()
            .join("\n   OR "),
    };
    Some(SearchStatement {
        sql: writer.statement(&parts, Some(rows.asked())),
        every_row: writer.statement(&parts, None),
        selected,
        searched,
    })
}

struct Parts {
    select: String,
    from: String,
    condition: String,
}

/// Writes the parts of one dialect's search statements.
struct Writer {
    dialect: Dialect,
    names: Quoting,
}

impl Writer {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            names: dialect.quoting(),
        }
    }

    fn statement(&self, parts: &Parts, limit: Option<u64>) -> String {
        let Parts {
            select,
            from,
            condition,
        } = parts;
        match (self.dialect, limit) {
            (Dialect::SqlServer, Some(limit)) => {
                format!("SELECT TOP ({limit}) {select}\nFROM {from}\nWHERE {condition}")
            }
            (_, Some(limit)) => {
                format!("SELECT {select}\nFROM {from}\nWHERE {condition}\nLIMIT {limit}")
            }
            (_, None) => format!("SELECT {select}\nFROM {from}\nWHERE {condition}"),
        }
    }

    /// The condition on `column` that holds where it holds the searched value, or `None` when
    /// its type can't hold it.
    fn condition(&self, column: &SearchColumn, search: &DataSearch) -> Option<String> {
        let kind = self.dialect.search_kind(&column.data_type);
        let name = self.names.identifier(&column.name);
        match kind {
            SearchKind::Text | SearchKind::Enum | SearchKind::Document => {
                self.text_condition(column, kind, search)
            }
            SearchKind::Integer { .. } | SearchKind::Decimal { .. } | SearchKind::Float => search
                .value
                .number()
                .and_then(|number| self.number_condition(&name, kind, number)),
            SearchKind::Uuid => search
                .value
                .uuid()
                .map(|uuid| format!("{name} = {}", self.uuid_literal(&uuid.to_string()))),
            SearchKind::Date | SearchKind::Timestamp | SearchKind::TimestampWithZone => search
                .value
                .date()
                .map(|date| self.date_condition(&name, kind, date)),
            SearchKind::Never => None,
        }
    }

    fn text_condition(
        &self,
        column: &SearchColumn,
        kind: SearchKind,
        search: &DataSearch,
    ) -> Option<String> {
        if kind == SearchKind::Document && search.matching == MatchKind::WholeValue {
            return None;
        }
        let text = TextColumn {
            expression: self.as_text(column, kind),
            column,
            kind,
        };
        let value = search.value.text();
        Some(match search.case {
            CaseMatch::Ignore => self.ignoring_case(&text, value, search.matching),
            CaseMatch::Match => self.matching_case(&text, value, search.matching),
        })
    }

    /// Ignoring case as each engine can: `ILIKE` where it has one; the column's collation on
    /// MySQL, MariaDB and SQL Server, which ignores case unless the column's says otherwise;
    /// `LOWER()` on both sides elsewhere. SQLite's `LIKE` ignores the case of ASCII letters
    /// only, as its `lower()` does without the ICU extension, so it is the best SQLite has.
    fn ignoring_case(&self, text: &TextColumn<'_>, value: &str, matching: MatchKind) -> String {
        let expression = &text.expression;
        match self.dialect {
            Dialect::Postgres
            | Dialect::DuckDb
            | Dialect::ClickHouse
            | Dialect::Snowflake
            | Dialect::Databricks => self.like(text, expression, value, matching, "ILIKE"),
            Dialect::Sqlite => self.like(text, expression, value, matching, "LIKE"),
            Dialect::Mysql | Dialect::SqlServer if !compares_case(text.column) => {
                self.compare(text, expression, value, matching)
            }
            Dialect::Mysql
            | Dialect::SqlServer
            | Dialect::Trino
            | Dialect::BigQuery
            | Dialect::Cassandra => {
                let lowered = format!("LOWER({expression})");
                self.compare(text, &lowered, &value.to_lowercase(), matching)
            }
        }
    }

    /// Matching case: `LIKE` is case-sensitive everywhere but MySQL, MariaDB and SQL Server,
    /// where a binary collation makes it so, and SQLite, where nothing does, so `instr` finds
    /// the text instead.
    fn matching_case(&self, text: &TextColumn<'_>, value: &str, matching: MatchKind) -> String {
        let expression = &text.expression;
        let binary = match self.dialect {
            Dialect::Mysql if !compares_case(text.column) => Some(format!(
                "{expression} COLLATE {}_bin",
                mysql_charset(text.column, text.kind)
            )),
            Dialect::SqlServer if !compares_case(text.column) => {
                Some(format!("{expression} COLLATE Latin1_General_100_BIN2"))
            }
            _ => None,
        };
        let expression = binary.as_deref().unwrap_or(expression);
        match (self.dialect, matching) {
            (Dialect::Sqlite, MatchKind::Contains) => {
                format!(
                    "instr({expression}, {}) > 0",
                    self.text_literal(text, value)
                )
            }
            (_, MatchKind::Contains) => self.like(text, expression, value, matching, "LIKE"),
            (_, MatchKind::WholeValue) => {
                format!("{expression} = {}", self.text_literal(text, value))
            }
        }
    }

    /// `LIKE` for **Contains**, `=` for **Whole value**, on a comparison that already ignores
    /// case.
    fn compare(
        &self,
        text: &TextColumn<'_>,
        expression: &str,
        value: &str,
        matching: MatchKind,
    ) -> String {
        match matching {
            MatchKind::Contains => self.like(text, expression, value, matching, "LIKE"),
            MatchKind::WholeValue => format!("{expression} = {}", self.text_literal(text, value)),
        }
    }

    fn like(
        &self,
        text: &TextColumn<'_>,
        expression: &str,
        value: &str,
        matching: MatchKind,
        operator: &str,
    ) -> String {
        let pattern = like_pattern(self.dialect, value, matching);
        format!(
            "{expression} {operator} {}{}",
            self.text_literal(text, &pattern),
            self.dialect.like_escape().clause()
        )
    }

    /// The column as text: enums and documents cast where the engine's `LIKE` refuses them.
    fn as_text(&self, column: &SearchColumn, kind: SearchKind) -> String {
        let name = self.names.identifier(&column.name);
        if kind == SearchKind::Text {
            return name;
        }
        match self.dialect {
            Dialect::Postgres => format!("CAST({name} AS text)"),
            Dialect::Mysql | Dialect::Sqlite => format!("CAST({name} AS CHAR)"),
            Dialect::SqlServer => format!("CAST({name} AS NVARCHAR(MAX))"),
            Dialect::ClickHouse => format!("toString({name})"),
            Dialect::Trino if kind == SearchKind::Document => format!("json_format({name})"),
            Dialect::DuckDb | Dialect::Trino | Dialect::Cassandra => {
                format!("CAST({name} AS VARCHAR)")
            }
            Dialect::BigQuery => format!("TO_JSON_STRING({name})"),
            Dialect::Snowflake => format!("TO_VARCHAR({name})"),
            Dialect::Databricks => format!("CAST({name} AS STRING)"),
        }
    }

    /// `value` as a string literal compared with the column: on SQL Server `N'…'` for the
    /// Unicode types and plain `'…'` for the others, so the column is never converted to the
    /// literal's type, which would keep its index from answering a **Whole value** search.
    fn text_literal(&self, text: &TextColumn<'_>, value: &str) -> String {
        let quoting = match self.dialect {
            Dialect::SqlServer if !is_unicode(text.column, text.kind) => Quoting {
                string_prefix: "",
                ..self.names
            },
            _ => self.string_quoting(),
        };
        quoting.string_literal(value)
    }

    /// The dialect's quoting for strings. GoogleSQL refuses the doubled quote `Quoting` writes
    /// (`'it''s'`), so a search for `O'Brien` there escapes it with a backslash.
    fn string_quoting(&self) -> Quoting {
        match self.dialect {
            Dialect::BigQuery => Quoting {
                quote_escape: QuoteEscape::Backslash,
                ..self.names
            },
            _ => self.names,
        }
    }

    fn number_condition(
        &self,
        name: &str,
        kind: SearchKind,
        number: &SearchNumber,
    ) -> Option<String> {
        let fits = match kind {
            SearchKind::Integer { min, max } => number
                .integer()
                .is_some_and(|value| (min..=max).contains(&value)),
            SearchKind::Decimal { digits } => {
                digits.is_none_or(|(precision, scale)| number.fits_decimal(precision, scale))
            }
            SearchKind::Float => true,
            _ => false,
        };
        fits.then(|| format!("{name} = {}", self.number_literal(kind, number)))
    }

    /// The number as the column's type compares with it. ClickHouse doesn't compare a
    /// `Decimal` with a literal it reads as a `Float64`, and BigQuery would compare a `NUMERIC`
    /// as one, losing digits; both are given a decimal of their own.
    fn number_literal(&self, kind: SearchKind, number: &SearchNumber) -> String {
        let quoting = self.string_quoting();
        match (self.dialect, kind) {
            (
                Dialect::ClickHouse,
                SearchKind::Decimal {
                    digits: Some((_, scale)),
                },
            ) => format!(
                "toDecimal128({}, {scale})",
                quoting.string_literal(number.literal())
            ),
            (Dialect::BigQuery, SearchKind::Decimal { .. }) => {
                format!("BIGNUMERIC {}", quoting.string_literal(number.literal()))
            }
            _ => number.literal().to_owned(),
        }
    }

    /// A UUID, written in lower case with its hyphens, as each engine reads one: Trino has no
    /// implicit conversion from text, and ClickHouse's is surer with `toUUID`.
    fn uuid_literal(&self, uuid: &str) -> String {
        let literal = self.string_quoting().string_literal(uuid);
        match self.dialect {
            Dialect::Trino => format!("UUID {literal}"),
            Dialect::ClickHouse => format!("toUUID({literal})"),
            _ => literal,
        }
    }

    /// A date column equal to the date; a timestamp on that day, from its midnight to the
    /// next day's.
    fn date_condition(&self, name: &str, kind: SearchKind, date: SearchDate) -> String {
        if kind == SearchKind::Date {
            return format!("{name} = {}", self.date_literal(date));
        }
        let from = self.timestamp_literal(date, kind);
        match date.next_day() {
            Some(next) => format!(
                "({name} >= {from} AND {name} < {})",
                self.timestamp_literal(next, kind)
            ),
            None => format!("{name} >= {from}"),
        }
    }

    /// SQL Server has no `DATE '…'`, and reads `'2026-11-25'` as a `datetime` by the session's
    /// `DATEFORMAT`; `'20261125'` reads the same whatever it is.
    fn date_literal(&self, date: SearchDate) -> String {
        match self.dialect {
            Dialect::SqlServer => format!("'{}'", date.to_string().replace('-', "")),
            Dialect::ClickHouse => format!("toDate('{date}')"),
            _ => format!("DATE '{date}'"),
        }
    }

    /// Midnight on `date`. BigQuery doesn't compare a `DATETIME` with a `TIMESTAMP`, so each is
    /// given its own.
    fn timestamp_literal(&self, date: SearchDate, kind: SearchKind) -> String {
        match (self.dialect, kind) {
            (Dialect::SqlServer, _) => self.date_literal(date),
            (Dialect::ClickHouse, _) => format!("toDateTime('{date} 00:00:00')"),
            (Dialect::BigQuery, SearchKind::Timestamp) => format!("DATETIME '{date} 00:00:00'"),
            _ => format!("TIMESTAMP '{date} 00:00:00'"),
        }
    }
}

/// A text column as a statement compares it.
struct TextColumn<'a> {
    /// The column, cast to text where it needs to be.
    expression: String,
    column: &'a SearchColumn,
    kind: SearchKind,
}

/// Whether the column's collation tells case apart (`utf8mb4_bin`, `latin1_general_cs`,
/// `Latin1_General_CS_AS`, `…_BIN2`), so ignoring case needs `LOWER()`. Without a
/// collation, the server's default, which ignores case on both engines.
fn compares_case(column: &SearchColumn) -> bool {
    let Some(collation) = column.collation.as_deref() else {
        return false;
    };
    let collation = collation.to_lowercase();
    collation == "binary"
        || collation.ends_with("_bin")
        || collation.ends_with("_bin2")
        || collation.contains("_cs")
}

/// The character set a MySQL column's binary collation is named after: a collation's name
/// starts with its character set's (`utf8mb4_0900_ai_ci`). A document cast to text is in the
/// connection's character set, `utf8mb4` for DbRover's connections.
fn mysql_charset(column: &SearchColumn, kind: SearchKind) -> &str {
    let collation = (kind == SearchKind::Text)
        .then_some(column.collation.as_deref())
        .flatten();
    collation
        .and_then(|collation| collation.split('_').next())
        .filter(|charset| !charset.is_empty())
        .unwrap_or("utf8mb4")
}

/// SQL Server's Unicode text types, and documents cast to `NVARCHAR(MAX)`.
fn is_unicode(column: &SearchColumn, kind: SearchKind) -> bool {
    let name = column.data_type.trim().to_lowercase();
    kind != SearchKind::Text || name.starts_with('n') || name == "sysname"
}

#[cfg(test)]
mod tests;
