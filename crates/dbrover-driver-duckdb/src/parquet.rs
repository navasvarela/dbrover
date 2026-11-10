//! Writing rows to a Parquet file, with DuckDB's writer, for exports.
//!
//! The rows go into a table of text columns in a DuckDB database of their own, in a temporary
//! folder that DuckDB spills to rather than holding every row in memory. Once the last row is
//! in, each column is given the narrowest type its values fit, and `COPY … TO` writes the file.
//!
//! A column takes a type only when every one of its values converts to it and back to the
//! same text, so the file says exactly what the database printed: `007` stays text rather
//! than becoming 7, and `2024-01-31T08:00:00+02` stays text rather than moving to UTC. A
//! declared type is tried first, then the others from the narrowest.

use std::io;
use std::path::Path;

use dbrover_core::{Cell, ValueKind};
use duckdb::{Connection, appender_params_from_iter};
use tempfile::TempDir;

/// What DuckDB may use of memory before it spills to the temporary folder: enough to convert
/// a large table quickly, little enough not to weigh on the app.
const MEMORY_LIMIT: &str = "512MB";

/// The types a column can take, from the narrowest. `DECIMAL` is sized to the values once
/// they are known to be plain decimals.
const CANDIDATES: [&str; 7] = [
    "BIGINT",
    "DECIMAL",
    "DOUBLE",
    "BOOLEAN",
    "DATE",
    "TIMESTAMP",
    "TIMESTAMPTZ",
];

/// DuckDB's widest decimal.
const MAX_DECIMAL_DIGITS: i64 = 38;

/// How the file's pages are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParquetCompression {
    /// Smaller files; what DuckDB and most tools default to.
    Zstd,
    /// Faster, for tools that read only Snappy.
    Snappy,
}

impl ParquetCompression {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Snappy => "snappy",
        }
    }
}

/// A Parquet file being written: rows go in a page at a time, and [`ParquetWriter::finish`]
/// writes the file. Blocking; call it from the blocking pool.
pub struct ParquetWriter {
    connection: Connection,
    /// Each column's name and declared kind.
    columns: Vec<(String, ValueKind)>,
    /// Holds the work database and DuckDB's spill files; removed when the writer goes.
    _work: TempDir,
}

impl ParquetWriter {
    /// Starts the rows' table, in a temporary folder.
    ///
    /// # Errors
    ///
    /// If the folder or the database can't be created.
    pub fn create(columns: Vec<(String, ValueKind)>) -> io::Result<Self> {
        let work = tempfile::Builder::new()
            .prefix("dbrover-parquet")
            .tempdir()?;
        let connection =
            Connection::open(work.path().join("rows.duckdb")).map_err(io::Error::other)?;
        let spill = work.path().join("spill");
        let definitions: Vec<String> = (0..columns.len())
            .map(|index| format!("{} VARCHAR", work_column(index)))
            .collect();
        let setup = format!(
            "SET memory_limit = '{MEMORY_LIMIT}'; SET temp_directory = {}; \
             CREATE TABLE rows ({});",
            literal(&spill.to_string_lossy()),
            definitions.join(", ")
        );
        connection.execute_batch(&setup).map_err(io::Error::other)?;
        Ok(Self {
            connection,
            columns,
            _work: work,
        })
    }

    /// Appends rows, each with a cell per column.
    ///
    /// # Errors
    ///
    /// DuckDB's, as an I/O error.
    pub fn append(&mut self, rows: &[Vec<Cell>]) -> io::Result<()> {
        let mut appender = self.connection.appender("rows").map_err(io::Error::other)?;
        for row in rows {
            appender
                .append_row(appender_params_from_iter(row))
                .map_err(io::Error::other)?;
        }
        appender.flush().map_err(io::Error::other)
    }

    /// Types each column and writes the file at `path`.
    ///
    /// # Errors
    ///
    /// DuckDB's, as an I/O error, if the file can't be written.
    pub fn finish(self, path: &Path, compression: ParquetCompression) -> io::Result<()> {
        let names = unique_names(&self.columns);
        let mut selected = Vec::with_capacity(self.columns.len());
        for (index, ((_, kind), name)) in self.columns.iter().zip(&names).enumerate() {
            let column = work_column(index);
            let value = match self.column_type(&column, *kind)? {
                Some(column_type) => format!("CAST({column} AS {column_type})"),
                None => column,
            };
            selected.push(format!("{value} AS {}", identifier(name)));
        }
        // Rows are read back in the order they went in: an export keeps the result's order.
        let copy = format!(
            "SET preserve_insertion_order = true; \
             COPY (SELECT {} FROM rows) TO {} (FORMAT parquet, COMPRESSION {})",
            selected.join(", "),
            literal(&path.to_string_lossy()),
            compression.as_sql()
        );
        self.connection
            .execute_batch(&copy)
            .map_err(io::Error::other)
    }

    /// The column's type: the first candidate, the declared one first, that every value
    /// converts to and back unchanged; `None` to keep it text. A column without a value takes
    /// its declared type, which nothing contradicts.
    fn column_type(&self, column: &str, kind: ValueKind) -> io::Result<Option<String>> {
        let declared = declared_type(kind);
        if !self.has_values(column)? {
            return Ok(declared
                .filter(|&declared| declared != "DECIMAL")
                .map(str::to_owned));
        }
        let others = CANDIDATES
            .into_iter()
            .filter(|candidate| Some(*candidate) != declared);
        for candidate in declared.into_iter().chain(others) {
            // A decimal's shape is checked by `decimal_type`, and its values convert exactly,
            // though shown with the column's scale (`9.5` as `9.50`), so not as they were.
            if candidate == "DECIMAL" {
                match self.decimal_type(column)? {
                    Some(decimal) => return Ok(Some(decimal)),
                    None => continue,
                }
            }
            if self.fits(column, candidate)? {
                return Ok(Some(candidate.to_owned()));
            }
        }
        Ok(None)
    }

    fn has_values(&self, column: &str) -> io::Result<bool> {
        let sql = format!("SELECT count({column}) > 0 FROM rows");
        self.connection
            .query_row(&sql, [], |row| row.get(0))
            .map_err(io::Error::other)
    }

    /// Whether every value of `column` converts to `column_type` and back to the same text.
    fn fits(&self, column: &str, column_type: &str) -> io::Result<bool> {
        let sql = format!(
            "SELECT bool_and(CAST(TRY_CAST({column} AS {column_type}) AS VARCHAR) \
             IS NOT DISTINCT FROM {column}) FROM rows WHERE {column} IS NOT NULL"
        );
        self.connection
            .query_row(&sql, [], |row| row.get(0))
            .map_err(io::Error::other)
    }

    /// `DECIMAL(p, s)` wide enough for every value of `column`, when they are all plain
    /// decimals without a leading zero that fit DuckDB's 38 digits; `None` otherwise.
    fn decimal_type(&self, column: &str) -> io::Result<Option<String>> {
        let sql = format!(
            "SELECT bool_and(regexp_full_match({column}, '-?(0|[1-9][0-9]*)(\\.[0-9]+)?')), \
                    coalesce(max(length(regexp_extract({column}, '\\.([0-9]+)$', 1))), 0), \
                    max(length(regexp_extract({column}, '^-?([0-9]+)', 1))) \
             FROM rows WHERE {column} IS NOT NULL"
        );
        let shape: (Option<bool>, Option<i64>, Option<i64>) = self
            .connection
            .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(io::Error::other)?;
        let (Some(true), Some(scale), Some(whole)) = shape else {
            return Ok(None);
        };
        let precision = whole + scale;
        Ok((precision <= MAX_DECIMAL_DIGITS).then(|| format!("DECIMAL({precision}, {scale})")))
    }
}

/// The DuckDB type a declared kind is tried as first.
fn declared_type(kind: ValueKind) -> Option<&'static str> {
    match kind {
        ValueKind::Integer => Some("BIGINT"),
        ValueKind::Decimal => Some("DECIMAL"),
        ValueKind::Float => Some("DOUBLE"),
        ValueKind::Boolean => Some("BOOLEAN"),
        ValueKind::Date => Some("DATE"),
        ValueKind::Timestamp => Some("TIMESTAMP"),
        ValueKind::TimestampWithZone => Some("TIMESTAMPTZ"),
        ValueKind::Text | ValueKind::Undeclared => None,
    }
}

/// Column names as the file's: a repeated name (`SELECT a.id, b.id`) gets `_2`, `_3`, … as in
/// the other exports, since a Parquet file's columns need names of their own.
fn unique_names(columns: &[(String, ValueKind)]) -> Vec<String> {
    let mut used = std::collections::HashSet::new();
    columns
        .iter()
        .map(|(name, _)| {
            let unique = (1..)
                .map(|n| match n {
                    1 => name.clone(),
                    n => format!("{name}_{n}"),
                })
                .find(|candidate| !used.contains(candidate))
                .unwrap_or_else(|| name.clone());
            used.insert(unique.clone());
            unique
        })
        .collect()
}

/// The work table's columns are numbered, so any result's names fit, repeated ones included.
fn work_column(index: usize) -> String {
    format!("c{index}")
}

fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Cell {
        Some(value.to_owned())
    }

    /// The file's columns, as DuckDB reads its schema: each name and type.
    fn schema(path: &Path) -> Vec<(String, String)> {
        let connection = Connection::open_in_memory().unwrap();
        let sql = format!(
            "DESCRIBE SELECT * FROM {}",
            literal(&path.to_string_lossy())
        );
        let mut statement = connection.prepare(&sql).unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn values(path: &Path) -> Vec<Vec<Option<String>>> {
        let connection = Connection::open_in_memory().unwrap();
        let path = literal(&path.to_string_lossy());
        let columns: usize = connection
            .query_row(
                &format!("SELECT count(*) FROM (DESCRIBE SELECT * FROM {path})"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        let casts: Vec<String> = (1..=columns)
            .map(|n| format!("CAST(#{n} AS VARCHAR)"))
            .collect();
        let sql = format!("SELECT {} FROM {path}", casts.join(", "));
        let mut statement = connection.prepare(&sql).unwrap();
        statement
            .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn types_each_column_by_what_its_values_keep() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.parquet");
        let columns = [
            ("id", ValueKind::Undeclared),
            ("price", ValueKind::Undeclared),
            ("ratio", ValueKind::Float),
            ("zip", ValueKind::Undeclared),
            ("ok", ValueKind::Boolean),
            ("day", ValueKind::Undeclared),
            ("at", ValueKind::Undeclared),
            ("at_tz", ValueKind::Undeclared),
            ("note", ValueKind::Integer),
            ("nothing", ValueKind::Undeclared),
            ("id", ValueKind::Text),
        ]
        .map(|(name, kind)| (name.to_owned(), kind))
        .to_vec();
        let rows = vec![
            vec![
                text("1"),
                text("9.50"),
                text("0.5"),
                text("02134"),
                text("true"),
                text("2024-01-31"),
                text("2024-01-31 08:00:00"),
                text("2024-01-31 08:00:00+02"),
                text("12"),
                None,
                text("7"),
            ],
            vec![
                text("-20"),
                text("12345678901234567890.123"),
                text("1e+20"),
                text("10001"),
                None,
                None,
                text("2024-02-01 09:30:00.25"),
                None,
                text("twelve"),
                None,
                text("8"),
            ],
        ];
        let mut writer = ParquetWriter::create(columns).unwrap();
        for row in &rows {
            writer.append(std::slice::from_ref(row)).unwrap();
        }
        writer.finish(&path, ParquetCompression::Zstd).unwrap();

        let schema: Vec<(String, String)> = schema(&path);
        let expected = [
            ("id", "BIGINT"),
            ("price", "DECIMAL(23,3)"),
            ("ratio", "DOUBLE"),
            ("zip", "VARCHAR"),
            ("ok", "BOOLEAN"),
            ("day", "DATE"),
            ("at", "TIMESTAMP"),
            ("at_tz", "VARCHAR"),
            ("note", "VARCHAR"),
            ("nothing", "VARCHAR"),
            ("id_2", "BIGINT"),
        ]
        .map(|(name, column_type)| (name.to_owned(), column_type.to_owned()));
        assert_eq!(schema, expected);
        let values = values(&path);
        assert_eq!(values[0][3].as_deref(), Some("02134"));
        assert_eq!(values[1][1].as_deref(), Some("12345678901234567890.123"));
        assert_eq!(values[1][6].as_deref(), Some("2024-02-01 09:30:00.25"));
        assert_eq!(values[1][8].as_deref(), Some("twelve"));
    }

    #[test]
    fn writes_snappy_and_an_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.parquet");
        let writer = ParquetWriter::create(vec![("n".to_owned(), ValueKind::Integer)]).unwrap();

        writer.finish(&path, ParquetCompression::Snappy).unwrap();

        assert_eq!(schema(&path), [("n".to_owned(), "BIGINT".to_owned())]);
        assert!(values(&path).is_empty());
    }
}
