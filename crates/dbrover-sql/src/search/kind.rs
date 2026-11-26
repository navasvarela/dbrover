//! What a column's type can hold, as far as a search is concerned.

use dbrover_core::ValueKind;

use crate::Dialect;
use crate::dialect::unwrap_clickhouse;

/// How a search compares a column with the searched value, from the column's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    /// Text the engine compares with `LIKE` as it is.
    Text,
    /// An enumerated type, compared as its label's text, cast where the engine needs it.
    Enum,
    /// A document (JSON, XML, Snowflake's `VARIANT`), searched as its text, and only for text
    /// it contains: a document is never one value.
    Document,
    /// A whole number between `min` and `max`, compared with `=`.
    Integer {
        min: i128,
        max: i128,
    },
    /// An exact number; `digits` is its precision and scale where the type says them, and
    /// `None` for a number of any size (PostgreSQL's `numeric`, `money`).
    Decimal {
        digits: Option<(u32, u32)>,
    },
    /// A floating-point number, compared with `=` as the engine compares it.
    Float,
    Uuid,
    Date,
    /// A date and time with no zone.
    Timestamp,
    /// An instant, or a date and time with a zone or offset.
    TimestampWithZone,
    /// Never searched: binary data, booleans (`true` would match every other row), arrays,
    /// maps, geometry, intervals, and types the driver can't name.
    Never,
}

impl SearchKind {
    fn integer_bits(bits: u32, signed: bool) -> Self {
        let bits = bits.min(127);
        if signed {
            Self::Integer {
                min: -(1i128 << (bits - 1)),
                max: (1i128 << (bits - 1)) - 1,
            }
        } else {
            Self::Integer {
                min: 0,
                max: (1i128 << bits) - 1,
            }
        }
    }

    /// A `DECIMAL(precision, 0)`, which holds whole numbers of up to `precision` digits.
    fn integer_digits(precision: u32) -> Self {
        let max = 10i128
            .checked_pow(precision)
            .map_or(i128::MAX, |bound| bound - 1);
        Self::Integer { min: -max, max }
    }

    /// Whether a text search reads the column.
    pub fn holds_text(self) -> bool {
        matches!(self, Self::Text | Self::Enum | Self::Document)
    }
}

impl Dialect {
    /// How a search compares a column declared as `data_type`, as the driver names the type
    /// (`dbrover_core::SearchColumn::data_type`). Text types are recognised by name, and any
    /// other type a search can't compare is [`SearchKind::Never`], so a type DbRover doesn't
    /// know is skipped rather than sent a `LIKE` the engine refuses.
    pub fn search_kind(self, data_type: &str) -> SearchKind {
        let lowered = data_type.trim().to_lowercase();
        if self == Self::Sqlite {
            return sqlite_search_kind(&lowered);
        }
        let name = match self {
            Self::ClickHouse => unwrap_clickhouse(&lowered),
            _ => &lowered,
        };
        let unsigned = self == Self::Mysql && name.contains("unsigned");
        let name = name.replace(" unsigned", "").replace(" zerofill", "");
        let name = name.trim();
        if is_container(self, name) {
            return SearchKind::Never;
        }
        if let Some(kind) = named_search_kind(self, name) {
            return kind;
        }
        match self.value_kind(Some(name)) {
            ValueKind::Integer => integer_kind(self, name, unsigned),
            ValueKind::Decimal => decimal_kind(self, name),
            ValueKind::Float => SearchKind::Float,
            ValueKind::Date => SearchKind::Date,
            ValueKind::Timestamp => SearchKind::Timestamp,
            ValueKind::TimestampWithZone => SearchKind::TimestampWithZone,
            ValueKind::Boolean | ValueKind::Text | ValueKind::Undeclared => SearchKind::Never,
        }
    }
}

/// The type's name before its parameters: `varchar(20)` is a `varchar`.
fn base_name(name: &str) -> &str {
    name.split('(').next().unwrap_or(name).trim()
}

/// The parameters in a type's parentheses: `decimal(10, 2)` has `10` and `2`.
fn parameters(name: &str) -> Vec<u32> {
    name.split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inside, _)| {
            inside
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Arrays, maps, structs and the like, whose element types would otherwise read as a kind:
/// `integer[]`, `Array(String)`, `ARRAY<STRING>`, `map<string,int>`.
fn is_container(dialect: Dialect, name: &str) -> bool {
    let base = name.split(['(', '<']).next().unwrap_or(name).trim();
    let snowflake_array = dialect == Dialect::Snowflake && base == "array";
    name.ends_with(']')
        || snowflake_array
        || matches!(
            base,
            "array" | "map" | "struct" | "tuple" | "nested" | "row" | "record"
        )
}

/// The kinds told by the type's name alone: text, enums, documents, UUIDs, and the types other
/// kinds' names would take for something they aren't.
fn named_search_kind(dialect: Dialect, name: &str) -> Option<SearchKind> {
    let base = base_name(name);
    let kind = match base {
        "char"
        | "character"
        | "varchar"
        | "character varying"
        | "nchar"
        | "nvarchar"
        | "national char"
        | "national character"
        | "national character varying"
        | "national varchar"
        | "nchar varying"
        | "text"
        | "tinytext"
        | "mediumtext"
        | "longtext"
        | "ntext"
        | "string"
        | "fixedstring"
        | "citext"
        | "name"
        | "bpchar"
        | "clob"
        | "sysname" => SearchKind::Text,
        // MySQL's `LIKE` reads an `ENUM` or a `SET` as its text; the others need a cast.
        "enum" | "set" if dialect == Dialect::Mysql => SearchKind::Text,
        "enum" | "enum8" | "enum16" => SearchKind::Enum,
        "json" | "jsonb" | "xml" | "variant" => SearchKind::Document,
        "object" if matches!(dialect, Dialect::Snowflake | Dialect::ClickHouse) => {
            SearchKind::Document
        }
        "uuid" | "uniqueidentifier" => SearchKind::Uuid,
        // SQL Server's `timestamp` is `rowversion`, eight bytes that change with each update.
        "timestamp" | "rowversion" if dialect == Dialect::SqlServer => SearchKind::Never,
        "year" if dialect == Dialect::Mysql => SearchKind::Integer { min: 0, max: 2155 },
        _ => return None,
    };
    Some(kind)
}

/// The range of an integer type, by its name and the dialect's widths: `int8` is eight bytes
/// on PostgreSQL and DuckDB but eight bits on ClickHouse, and `tinyint` signed everywhere but
/// SQL Server.
fn integer_kind(dialect: Dialect, name: &str, unsigned: bool) -> SearchKind {
    let base = base_name(name);
    match dialect {
        Dialect::BigQuery => return SearchKind::integer_bits(64, true),
        Dialect::Snowflake => {
            let precision = parameters(name).first().copied().unwrap_or(38);
            return SearchKind::integer_digits(precision);
        }
        _ => {}
    }
    if matches!(base, "decimal" | "numeric" | "number" | "dec") {
        let precision = parameters(name).first().copied().unwrap_or(10);
        return SearchKind::integer_digits(precision);
    }
    if dialect == Dialect::ClickHouse {
        let (signed, digits) = match base.strip_prefix('u') {
            Some(rest) => (false, rest),
            None => (true, base),
        };
        let bits = digits
            .strip_prefix("int")
            .and_then(|bits| bits.parse().ok());
        return SearchKind::integer_bits(bits.unwrap_or(64), signed);
    }
    let (bits, signed) = match base {
        "tinyint" if dialect == Dialect::SqlServer => (8, false),
        "tinyint" | "int1" | "byte" | "byteint" => (8, true),
        "utinyint" => (8, false),
        "smallint" | "int2" | "short" | "smallserial" => (16, true),
        "usmallint" => (16, false),
        "mediumint" => (24, true),
        "integer" | "int" | "int4" | "serial" => (32, true),
        "uinteger" => (32, false),
        "hugeint" | "int128" => (128, true),
        "uhugeint" => (128, false),
        "ubigint" => (64, false),
        _ => (64, true),
    };
    SearchKind::integer_bits(bits, signed && !unsigned)
}

/// An exact number's precision and scale, where its type says them. ClickHouse's
/// `Decimal32(S)` to `Decimal256(S)` name the scale alone, their precision fixed by their size.
fn decimal_kind(dialect: Dialect, name: &str) -> SearchKind {
    let base = base_name(name);
    let parameters = parameters(name);
    if dialect == Dialect::ClickHouse {
        let precision = match base {
            "decimal32" => Some(9),
            "decimal64" => Some(18),
            "decimal128" => Some(38),
            "decimal256" => Some(76),
            _ => None,
        };
        if let (Some(precision), Some(&scale)) = (precision, parameters.first()) {
            return SearchKind::Decimal {
                digits: Some((precision, scale)),
            };
        }
    }
    let digits = match parameters.as_slice() {
        [precision, scale] => Some((*precision, *scale)),
        [precision] => Some((*precision, 0)),
        _ => None,
    };
    SearchKind::Decimal { digits }
}

/// SQLite's columns hold any value whatever they declare, and its `LIKE` reads any value as
/// text. Its own affinity rules (<https://www.sqlite.org/datatype3.html>, section 3.1, as
/// `value_kind` reads them) say what a column mostly holds: integers and reals are compared as
/// numbers, and a column declared as a decimal too, since `42.50` is kept there as the real
/// `42.5`, which no text search for `42.50` finds. Booleans and `BLOB` columns are never
/// searched. Any other column is searched as text, even one SQLite gives numeric affinity
/// (`date`, `uuid`, `json`, no type at all), which people use to keep text.
fn sqlite_search_kind(name: &str) -> SearchKind {
    if name.contains("int") {
        return SearchKind::integer_bits(64, true);
    }
    if ["char", "clob", "text"]
        .iter()
        .any(|text| name.contains(text))
    {
        return SearchKind::Text;
    }
    if name.contains("blob") {
        return SearchKind::Never;
    }
    if ["real", "floa", "doub"]
        .iter()
        .any(|float| name.contains(float))
    {
        return SearchKind::Float;
    }
    if name.contains("bool") {
        return SearchKind::Never;
    }
    if ["numeric", "decimal", "number", "money"]
        .iter()
        .any(|number| name.contains(number))
    {
        return SearchKind::Decimal { digits: None };
    }
    SearchKind::Text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integer(min: i128, max: i128) -> SearchKind {
        SearchKind::Integer { min, max }
    }

    #[test]
    fn reads_each_dialects_types() {
        let i32_range = integer(i32::MIN.into(), i32::MAX.into());
        let i64_range = integer(i64::MIN.into(), i64::MAX.into());
        let cases = [
            (Dialect::Postgres, "character varying(20)", SearchKind::Text),
            (Dialect::Postgres, "citext", SearchKind::Text),
            (Dialect::Postgres, "enum", SearchKind::Enum),
            (Dialect::Postgres, "jsonb", SearchKind::Document),
            (Dialect::Postgres, "uuid", SearchKind::Uuid),
            (Dialect::Postgres, "integer", i32_range),
            (Dialect::Postgres, "int8", i64_range),
            (
                Dialect::Postgres,
                "numeric",
                SearchKind::Decimal { digits: None },
            ),
            (
                Dialect::Postgres,
                "numeric(10,2)",
                SearchKind::Decimal {
                    digits: Some((10, 2)),
                },
            ),
            (Dialect::Postgres, "double precision", SearchKind::Float),
            (Dialect::Postgres, "date", SearchKind::Date),
            (
                Dialect::Postgres,
                "timestamp without time zone",
                SearchKind::Timestamp,
            ),
            (
                Dialect::Postgres,
                "timestamp with time zone",
                SearchKind::TimestampWithZone,
            ),
            (Dialect::Postgres, "integer[]", SearchKind::Never),
            (Dialect::Postgres, "bytea", SearchKind::Never),
            (Dialect::Postgres, "boolean", SearchKind::Never),
            (Dialect::Postgres, "inet", SearchKind::Never),
            (
                Dialect::Postgres,
                "time without time zone",
                SearchKind::Never,
            ),
            (Dialect::Mysql, "enum('a','b')", SearchKind::Text),
            (
                Dialect::Mysql,
                "int(10) unsigned",
                integer(0, u32::MAX.into()),
            ),
            (Dialect::Mysql, "tinyint(4)", integer(-128, 127)),
            (
                Dialect::Mysql,
                "bigint unsigned",
                integer(0, u64::MAX.into()),
            ),
            (
                Dialect::Mysql,
                "mediumint",
                integer(-(1 << 23), (1 << 23) - 1),
            ),
            (Dialect::Mysql, "json", SearchKind::Document),
            (Dialect::Mysql, "bit(1)", SearchKind::Never),
            (Dialect::Mysql, "varbinary(16)", SearchKind::Never),
            (Dialect::SqlServer, "tinyint", integer(0, 255)),
            (Dialect::SqlServer, "uniqueidentifier", SearchKind::Uuid),
            (Dialect::SqlServer, "timestamp", SearchKind::Never),
            (Dialect::SqlServer, "xml", SearchKind::Document),
            (Dialect::SqlServer, "sysname", SearchKind::Text),
            (Dialect::SqlServer, "bit", SearchKind::Never),
            (
                Dialect::ClickHouse,
                "LowCardinality(Nullable(String))",
                SearchKind::Text,
            ),
            (Dialect::ClickHouse, "Nullable(String)", SearchKind::Text),
            (Dialect::ClickHouse, "UInt8", integer(0, 255)),
            (Dialect::ClickHouse, "Int8", integer(-128, 127)),
            (Dialect::ClickHouse, "Enum8('a' = 1)", SearchKind::Enum),
            (
                Dialect::ClickHouse,
                "Decimal64(2)",
                SearchKind::Decimal {
                    digits: Some((18, 2)),
                },
            ),
            (Dialect::ClickHouse, "Array(String)", SearchKind::Never),
            (Dialect::ClickHouse, "UUID", SearchKind::Uuid),
            (Dialect::DuckDb, "INTEGER", i32_range),
            (Dialect::DuckDb, "UTINYINT", integer(0, 255)),
            (Dialect::DuckDb, "ENUM('a', 'b')", SearchKind::Enum),
            (Dialect::DuckDb, "VARCHAR[]", SearchKind::Never),
            (Dialect::DuckDb, "STRUCT(a INTEGER)", SearchKind::Never),
            (
                Dialect::Snowflake,
                "NUMBER(38,0)",
                SearchKind::integer_digits(38),
            ),
            (Dialect::Snowflake, "VARIANT", SearchKind::Document),
            (Dialect::Snowflake, "ARRAY", SearchKind::Never),
            (Dialect::BigQuery, "INT64", i64_range),
            (Dialect::BigQuery, "ARRAY<STRING>", SearchKind::Never),
            (Dialect::BigQuery, "STRING", SearchKind::Text),
            (
                Dialect::Databricks,
                "decimal(10,0)",
                SearchKind::integer_digits(10),
            ),
            (Dialect::Databricks, "map<string,int>", SearchKind::Never),
            (Dialect::Trino, "varchar", SearchKind::Text),
            (
                Dialect::Trino,
                "timestamp(3) with time zone",
                SearchKind::TimestampWithZone,
            ),
        ];
        for (dialect, name, kind) in cases {
            assert_eq!(dialect.search_kind(name), kind, "{dialect:?} {name}");
        }
    }

    #[test]
    fn sqlite_reads_its_affinities() {
        let cases = [
            ("", SearchKind::Text),
            ("TEXT", SearchKind::Text),
            ("datetime", SearchKind::Text),
            ("DATE", SearchKind::Text),
            ("NUMERIC", SearchKind::Decimal { digits: None }),
            ("decimal(10, 2)", SearchKind::Decimal { digits: None }),
            ("boolean", SearchKind::Never),
            ("uuid", SearchKind::Text),
            ("INTEGER", SearchKind::integer_bits(64, true)),
            ("REAL", SearchKind::Float),
            ("BLOB", SearchKind::Never),
        ];
        for (name, kind) in cases {
            assert_eq!(Dialect::Sqlite.search_kind(name), kind, "{name:?}");
        }
    }
}
