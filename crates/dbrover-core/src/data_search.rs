//! Searching a database's data for a value (W16, `docs/data-search.md`): what is searched for,
//! where, and the tables a driver lists for it.
//!
//! A driver lists the tables of a [`SearchScope`] with their columns in
//! [`Connection::searchable_tables`]; `dbrover-sql` writes one statement per table from them
//! and a [`DataSearch`]; the engine runs those statements one at a time.
//!
//! [`Connection::searchable_tables`]: crate::Connection::searchable_tables

use std::fmt;
use std::num::NonZeroU32;

use uuid::Uuid;

use crate::{CatalogPath, NodeKind, TableName};

/// What a search looks for, and how it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSearch {
    pub value: SearchValue,
    pub matching: MatchKind,
    pub case: CaseMatch,
}

impl DataSearch {
    /// A search for `text`, around which whitespace is dropped (a value pasted from a log often
    /// carries a newline).
    ///
    /// # Errors
    ///
    /// [`EmptySearch`] when nothing is left to search for.
    pub fn new(text: &str, matching: MatchKind, case: CaseMatch) -> Result<Self, EmptySearch> {
        Ok(Self {
            value: SearchValue::read(text)?,
            matching,
            case,
        })
    }
}

/// A search was asked for blank text, which every row would contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("There is nothing to search for.")]
pub struct EmptySearch;

/// How a text column's value has to hold the searched text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MatchKind {
    /// Anywhere in the value.
    #[default]
    Contains,
    /// As the whole value.
    WholeValue,
}

/// Whether a text column's value has to have the searched text's case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CaseMatch {
    #[default]
    Ignore,
    Match,
}

/// The searched text, read once as everything it could be, so each column is compared with
/// what its type can hold: text in text columns, and the number, UUID or date the text spells
/// in columns of those types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchValue {
    text: String,
    number: Option<SearchNumber>,
    uuid: Option<Uuid>,
    date: Option<SearchDate>,
}

impl SearchValue {
    /// Reads `text`, trimmed, as text and as whatever else it spells.
    ///
    /// # Errors
    ///
    /// [`EmptySearch`] for text that is blank once trimmed.
    pub fn read(text: &str) -> Result<Self, EmptySearch> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EmptySearch);
        }
        Ok(Self {
            text: text.to_owned(),
            number: SearchNumber::read(text),
            uuid: Uuid::try_parse(text).ok(),
            date: SearchDate::read(text),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn number(&self) -> Option<&SearchNumber> {
        self.number.as_ref()
    }

    pub fn uuid(&self) -> Option<Uuid> {
        self.uuid
    }

    pub fn date(&self) -> Option<SearchDate> {
        self.date
    }
}

/// A number as the search's text spells it, in plain decimal notation: an optional minus,
/// digits, and an optional fraction. `+5`, `1e3`, `.5`, `1,5` and `007` are left as text only:
/// a leading zero is far more often part of a code (`007`, a postcode) than a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchNumber {
    text: String,
    negative: bool,
    whole: String,
    fraction: String,
}

impl SearchNumber {
    fn read(text: &str) -> Option<Self> {
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        let well_formed = !whole.is_empty()
            && digits(whole)
            && digits(fraction)
            && !(unsigned.contains('.') && fraction.is_empty())
            && !(whole.len() > 1 && whole.starts_with('0'));
        well_formed.then(|| Self {
            text: text.to_owned(),
            negative,
            whole: whole.to_owned(),
            fraction: fraction.to_owned(),
        })
    }

    /// The number as typed, which every SQL dialect reads as a numeric literal.
    pub fn literal(&self) -> &str {
        &self.text
    }

    /// The number as a whole number, `None` when it has a fraction (even `.0`, which a person
    /// types to mean a decimal) or doesn't fit in 128 bits.
    pub fn integer(&self) -> Option<i128> {
        if !self.fraction.is_empty() {
            return None;
        }
        self.text.parse().ok()
    }

    /// Whether a `DECIMAL(precision, scale)` column can hold the number exactly: a number with
    /// more digits after the point than the scale, or before it than the precision leaves,
    /// would only ever equal a rounded value. Trailing zeros of the fraction don't count.
    pub fn fits_decimal(&self, precision: u32, scale: u32) -> bool {
        let fraction = self.fraction.trim_end_matches('0').len();
        let whole = self.whole.trim_start_matches('0').len();
        let fits =
            |digits: usize, room: u32| u32::try_from(digits).is_ok_and(|digits| digits <= room);
        fits(fraction, scale) && fits(whole, precision.saturating_sub(scale))
    }

    pub fn is_negative(&self) -> bool {
        self.negative
    }
}

/// A calendar date, read from the search's text when it is written `YYYY-MM-DD`, the one form
/// every engine reads the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchDate {
    year: u16,
    month: u8,
    day: u8,
}

impl SearchDate {
    fn read(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let shaped = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
        if !shaped {
            return None;
        }
        let year = text.get(0..4)?.parse().ok()?;
        let month = text.get(5..7)?.parse().ok()?;
        let day = text.get(8..10)?.parse().ok()?;
        Self::new(year, month, day)
    }

    /// The date, if there is one: `2026-02-30` isn't.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        let valid =
            year >= 1 && (1..=12).contains(&month) && day >= 1 && day <= days_in(year, month);
        valid.then_some(Self { year, month, day })
    }

    /// The day after, where a timestamp on this date stops; `None` after 9999-12-31, which no
    /// engine's dates reach past.
    pub fn next_day(self) -> Option<Self> {
        if self.day < days_in(self.year, self.month) {
            return Some(Self {
                day: self.day + 1,
                ..self
            });
        }
        if self.month < 12 {
            return Some(Self {
                month: self.month + 1,
                day: 1,
                ..self
            });
        }
        (self.year < 9999).then(|| Self {
            year: self.year + 1,
            month: 1,
            day: 1,
        })
    }
}

impl fmt::Display for SearchDate {
    /// `2026-11-25`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn days_in(year: u16, month: u8) -> u8 {
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Where a search looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    /// Every table under a node of the catalog: the root (the whole data source, without its
    /// system schemas), a database, a schema, or a folder of tables.
    Under(CatalogPath),
    /// These tables, by their catalog paths, as ticked in the scope's list.
    Tables(Vec<CatalogPath>),
}

impl SearchScope {
    fn paths(&self) -> &[CatalogPath] {
        match self {
            Self::Under(path) => std::slice::from_ref(path),
            Self::Tables(paths) => paths,
        }
    }

    /// Whether the scope takes in the relation the explorer shows at `path`: a path under the
    /// node searched, or one of the tables ticked.
    pub fn includes(&self, path: &CatalogPath) -> bool {
        match self {
            Self::Under(under) => path.starts_with(under),
            Self::Tables(paths) => paths.contains(path),
        }
    }

    /// The schemas the scope names, when every part of it names one, so a driver need read
    /// only their columns; `None` when a part is the whole data source or a database.
    pub fn schemas(&self) -> Option<Vec<&str>> {
        let mut schemas: Vec<&str> = Vec::new();
        for path in self.paths() {
            let schema = path.name_of(NodeKind::Schema)?;
            if !schemas.contains(&schema) {
                schemas.push(schema);
            }
        }
        Some(schemas)
    }

    /// The databases the scope names, in the order it names them; empty when a part is the
    /// whole data source.
    pub fn databases(&self) -> Vec<&str> {
        let mut databases: Vec<&str> = Vec::new();
        for path in self.paths() {
            if let Some(database) = path.name_of(NodeKind::Database)
                && !databases.contains(&database)
            {
                databases.push(database);
            }
        }
        databases
    }

    /// Whether a part of the scope names a schema, so its relations are searched even if the
    /// schema is the system's own: a search of the whole data source leaves those out.
    pub fn names_schema(&self, schema: &str) -> bool {
        self.paths()
            .iter()
            .any(|path| path.name_of(NodeKind::Schema) == Some(schema))
    }
}

/// What [`Connection::searchable_tables`] is asked for.
///
/// [`Connection::searchable_tables`]: crate::Connection::searchable_tables
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablesToSearch {
    pub scope: SearchScope,
    /// Views too, which read their tables again, often with joins: off unless asked for.
    pub views: Relations,
}

/// Which relations a search reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Relations {
    #[default]
    TablesOnly,
    TablesAndViews,
}

/// A table or view a search may read, as the driver found it in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchableTable {
    /// The database the table is in, where one connection reaches several and a statement
    /// has to name it (DuckDB's attached databases); `None` where the connection's own is the
    /// only one a statement reaches.
    pub database: Option<String>,
    pub name: TableName,
    /// The node the explorer shows it as.
    pub path: CatalogPath,
    pub relation: RelationKind,
    /// The primary key's columns, in key order; empty for a table without one, or a view.
    pub key: Vec<String>,
    /// How many rows the catalog's statistics say it has, which orders the search from the
    /// smallest table; `None` where the engine keeps no estimate.
    pub row_estimate: Option<u64>,
    /// In table order.
    pub columns: Vec<SearchColumn>,
}

/// Whether a relation stores its rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RelationKind {
    #[default]
    Table,
    View,
}

/// A column of a [`SearchableTable`], with what `dbrover-sql` needs to compare it with the
/// searched value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchColumn {
    pub name: String,
    /// As the catalog spells it, with its parameters and modifiers (`varchar(20)`,
    /// `int(10) unsigned`, `Nullable(String)`). A user-defined enumerated type (PostgreSQL's
    /// `CREATE TYPE … AS ENUM`) is reported as `enum`, since its own name says nothing of
    /// what it holds; a domain as the type it is based on.
    pub data_type: String,
    /// The column's collation where it decides how text compares (MySQL, MariaDB, SQL
    /// Server), so a case-sensitive column can still be searched ignoring case.
    pub collation: Option<String>,
}

impl SearchColumn {
    pub fn new(name: impl Into<String>, data_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: data_type.into(),
            collation: None,
        }
    }

    pub fn with_collation(self, collation: impl Into<String>) -> Self {
        Self {
            collation: Some(collation.into()),
            ..self
        }
    }
}

/// The most rows a search reads from one table: the statement asks for one more, to tell
/// whether there were more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowsPerTable(NonZeroU32);

impl RowsPerTable {
    /// 100: enough to see what a table holds, few enough that a search over a thousand tables
    /// stays small in memory. Settings › Data search changes it.
    pub const DEFAULT: Self = match NonZeroU32::new(100) {
        Some(rows) => Self(rows),
        None => panic!("the default rows per table must not be zero"),
    };

    pub fn new(rows: NonZeroU32) -> Self {
        Self(rows)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }

    /// What the statement asks for: one more than are shown.
    pub fn asked(self) -> u64 {
        u64::from(self.0.get()) + 1
    }
}

impl Default for RowsPerTable {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_text_is_refused() {
        assert_eq!(SearchValue::read("  \n"), Err(EmptySearch));
        assert_eq!(
            SearchValue::read("  o'brien\n").map(|value| value.text().to_owned()),
            Ok("o'brien".to_owned())
        );
    }

    #[test]
    fn reads_numbers_in_plain_decimal_notation_only() {
        let number = |text: &str| SearchValue::read(text).ok().and_then(|value| value.number);
        for text in ["1042", "-7", "0", "42.50", "0.5", "-0.25"] {
            assert!(number(text).is_some(), "{text} is a number");
        }
        for text in [
            "007", "+5", "1e3", ".5", "5.", "1,5", "12a", "-", "--1", "1.2.3",
        ] {
            assert!(number(text).is_none(), "{text} is text only");
        }
        assert_eq!(number("1042").and_then(|n| n.integer()), Some(1042));
        assert_eq!(number("-7").and_then(|n| n.integer()), Some(-7));
        assert_eq!(number("42.0").and_then(|n| n.integer()), None);
    }

    #[test]
    fn a_number_fits_a_decimal_by_its_digits() {
        let number = SearchNumber::read("123.450").expect("a number");
        assert!(number.fits_decimal(5, 2));
        assert!(!number.fits_decimal(5, 1));
        assert!(!number.fits_decimal(4, 2));
        let small = SearchNumber::read("0.5").expect("a number");
        assert!(small.fits_decimal(1, 1));
    }

    #[test]
    fn reads_uuids_in_any_of_their_spellings() {
        let uuid = |text: &str| SearchValue::read(text).ok().and_then(|value| value.uuid());
        let expected = Uuid::try_parse("3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10").ok();
        for text in [
            "3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10",
            "3F2C8E1A-9B7D-4C3E-8A1F-0D2B4C6E8F10",
            "{3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10}",
            "3f2c8e1a9b7d4c3e8a1f0d2b4c6e8f10",
        ] {
            assert_eq!(uuid(text), expected, "{text}");
        }
        assert_eq!(uuid("3f2c8e1a-9b7d"), None);
    }

    #[test]
    fn reads_dates_written_year_first() {
        let date = |text: &str| SearchValue::read(text).ok().and_then(|value| value.date());
        assert_eq!(
            date("2026-11-25").map(|d| d.to_string()),
            Some("2026-11-25".into())
        );
        assert_eq!(
            date("2024-02-29").map(|d| d.to_string()),
            Some("2024-02-29".into())
        );
        for text in [
            "2026-02-29",
            "2026-13-01",
            "2026-11-31",
            "25/11/2026",
            "2026-1-25",
        ] {
            assert_eq!(date(text), None, "{text}");
        }
    }

    #[test]
    fn the_next_day_crosses_months_and_years() {
        let next = |y, m, d| {
            SearchDate::new(y, m, d)
                .and_then(SearchDate::next_day)
                .map(|day| day.to_string())
        };
        assert_eq!(next(2026, 11, 25), Some("2026-11-26".into()));
        assert_eq!(next(2026, 11, 30), Some("2026-12-01".into()));
        assert_eq!(next(2024, 2, 28), Some("2024-02-29".into()));
        assert_eq!(next(2026, 12, 31), Some("2027-01-01".into()));
        assert_eq!(next(9999, 12, 31), None);
    }

    fn orders_path() -> CatalogPath {
        CatalogPath::root()
            .child(NodeKind::Database, "shop")
            .child(NodeKind::Schema, "public")
            .child(NodeKind::Folder(crate::FolderKind::Tables), "Tables")
            .child(NodeKind::Table, "orders")
    }

    #[test]
    fn a_scope_takes_in_what_is_under_it_or_ticked() {
        let orders = orders_path();
        let database = CatalogPath::root().child(NodeKind::Database, "shop");
        let other = database.child(NodeKind::Schema, "audit");
        assert!(SearchScope::Under(CatalogPath::root()).includes(&orders));
        assert!(SearchScope::Under(database.clone()).includes(&orders));
        assert!(!SearchScope::Under(other.clone()).includes(&orders));
        assert!(SearchScope::Tables(vec![orders.clone()]).includes(&orders));
        assert_eq!(SearchScope::Under(CatalogPath::root()).schemas(), None);
        assert_eq!(SearchScope::Under(database.clone()).schemas(), None);
        assert_eq!(SearchScope::Under(database).databases(), vec!["shop"]);
        assert_eq!(
            SearchScope::Tables(vec![orders, other]).schemas(),
            Some(vec!["public", "audit"])
        );
    }

    #[test]
    fn a_table_is_asked_for_one_more_row_than_it_shows() {
        assert_eq!(RowsPerTable::DEFAULT.asked(), 101);
    }
}
