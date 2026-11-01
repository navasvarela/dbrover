//! What kind of value each database's type names hold, for the exports that write types.

use dbrover_core::ValueKind;

use super::Dialect;

impl Dialect {
    /// The kind of value a column declared as `type_name` holds, as its driver names the type
    /// (`ColumnInfo::type_name`). [`ValueKind::Undeclared`] without a name, and
    /// [`ValueKind::Text`] for a type no other kind describes.
    pub fn value_kind(self, type_name: Option<&str>) -> ValueKind {
        let Some(type_name) = type_name else {
            return ValueKind::Undeclared;
        };
        let name = type_name.trim().to_lowercase();
        match self {
            Self::Sqlite => sqlite_kind(&name),
            Self::ClickHouse => named_kind(unwrap_clickhouse(&name), self),
            Self::Cassandra => cql_kind(&name),
            Self::Postgres | Self::Mysql | Self::SqlServer | Self::DuckDb => {
                named_kind(&name, self)
            }
        }
    }
}

/// A type by its name without its parameters: `decimal(10, 2)` is a `decimal`.
fn named_kind(name: &str, dialect: Dialect) -> ValueKind {
    let base = name.split('(').next().unwrap_or(name).trim();
    match base {
        "smallint" | "integer" | "int" | "bigint" | "tinyint" | "mediumint" | "int2" | "int4"
        | "int8" | "serial" | "smallserial" | "bigserial" | "year" | "utinyint" | "usmallint"
        | "uinteger" | "ubigint" | "hugeint" | "uhugeint" | "int16" | "int32" | "int64"
        | "int128" | "int256" | "uint8" | "uint16" | "uint32" | "uint64" | "uint128"
        | "uint256" => ValueKind::Integer,
        "decimal" | "numeric" | "money" | "smallmoney" | "decimal32" | "decimal64"
        | "decimal128" | "decimal256" => ValueKind::Decimal,
        "real" | "float" | "double" | "double precision" | "float4" | "float8" | "float32"
        | "float64" => ValueKind::Float,
        "boolean" | "bool" => ValueKind::Boolean,
        // MySQL's `BIT(n)` is a string of bits; SQL Server's `bit` is its boolean.
        "bit" if dialect == Dialect::SqlServer => ValueKind::Boolean,
        "date" | "date32" => ValueKind::Date,
        "timestamp"
        | "timestamp without time zone"
        | "datetime"
        | "datetime2"
        | "smalldatetime"
        | "datetime64" => ValueKind::Timestamp,
        "timestamptz" | "timestamp with time zone" | "datetimeoffset" => {
            ValueKind::TimestampWithZone
        }
        _ => ValueKind::Text,
    }
}

/// CQL's types. A `timestamp` is an instant, which the driver writes in UTC with its offset;
/// `varint` and `counter` are integers, and collections, tuples and user-defined types are
/// JSON text.
fn cql_kind(name: &str) -> ValueKind {
    match name {
        "tinyint" | "smallint" | "int" | "bigint" | "varint" | "counter" => ValueKind::Integer,
        "decimal" => ValueKind::Decimal,
        "float" | "double" => ValueKind::Float,
        "boolean" => ValueKind::Boolean,
        "date" => ValueKind::Date,
        "timestamp" => ValueKind::TimestampWithZone,
        _ => ValueKind::Text,
    }
}

/// ClickHouse's type without the wrappers that don't change what its values are:
/// `Nullable(LowCardinality(String))` is a `string`.
fn unwrap_clickhouse(name: &str) -> &str {
    let mut name = name;
    for wrapper in ["nullable(", "lowcardinality("] {
        if let Some(inner) = name
            .strip_prefix(wrapper)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            name = unwrap_clickhouse(inner);
        }
    }
    name
}

/// SQLite takes any name as a column's type and gives it an affinity by the rules of its
/// documentation's section 3.1, "Determination Of Column Affinity"
/// (<https://www.sqlite.org/datatype3.html#determination_of_column_affinity>), checked in that
/// order. Names with numeric affinity are then told apart by what people declare them as,
/// since SQLite has no date or boolean type of its own.
fn sqlite_kind(name: &str) -> ValueKind {
    if name.contains("int") {
        return ValueKind::Integer;
    }
    if ["char", "clob", "text", "blob"]
        .iter()
        .any(|text| name.contains(text))
    {
        return ValueKind::Text;
    }
    if ["real", "floa", "doub"]
        .iter()
        .any(|float| name.contains(float))
    {
        return ValueKind::Float;
    }
    match name.split('(').next().unwrap_or(name).trim() {
        "bool" | "boolean" => ValueKind::Boolean,
        "date" => ValueKind::Date,
        "datetime" | "timestamp" => ValueKind::Timestamp,
        "decimal" | "numeric" => ValueKind::Decimal,
        _ => ValueKind::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type names as each driver reports them, with the kind each must map to.
    #[test]
    fn maps_each_drivers_type_names() {
        use ValueKind::*;
        let cases: &[(Dialect, &[(&str, ValueKind)])] = &[
            (
                Dialect::Postgres,
                &[
                    ("int4", Integer),
                    ("numeric(10,2)", Decimal),
                    ("double precision", Float),
                    ("bool", Boolean),
                    ("date", Date),
                    ("timestamp", Timestamp),
                    ("timestamptz", TimestampWithZone),
                    ("uuid", Text),
                    ("jsonb", Text),
                ],
            ),
            (
                Dialect::Sqlite,
                &[
                    ("integer", Integer),
                    ("bigint", Integer),
                    ("varchar(20)", Text),
                    ("text", Text),
                    ("blob", Text),
                    ("real", Float),
                    ("double", Float),
                    ("numeric", Decimal),
                    ("decimal(10, 2)", Decimal),
                    ("boolean", Boolean),
                    ("date", Date),
                    ("datetime", Timestamp),
                    ("whatever", Text),
                ],
            ),
            (
                Dialect::Mysql,
                &[
                    ("tinyint", Integer),
                    ("bigint", Integer),
                    ("year", Integer),
                    ("decimal", Decimal),
                    ("double", Float),
                    ("date", Date),
                    ("datetime", Timestamp),
                    ("timestamp", Timestamp),
                    ("bit", Text),
                    ("json", Text),
                    ("varchar", Text),
                ],
            ),
            (
                Dialect::SqlServer,
                &[
                    ("int", Integer),
                    ("money", Decimal),
                    ("numeric", Decimal),
                    ("float", Float),
                    ("real", Float),
                    ("bit", Boolean),
                    ("date", Date),
                    ("datetime2", Timestamp),
                    ("smalldatetime", Timestamp),
                    ("datetimeoffset", TimestampWithZone),
                    ("uniqueidentifier", Text),
                    ("nvarchar", Text),
                ],
            ),
            (
                Dialect::DuckDb,
                &[
                    ("ubigint", Integer),
                    ("decimal(18, 3)", Decimal),
                    ("float", Float),
                    ("boolean", Boolean),
                    ("date", Date),
                    ("timestamp", Timestamp),
                    ("timestamp with time zone", TimestampWithZone),
                    ("list", Text),
                    ("interval", Text),
                ],
            ),
            (
                Dialect::ClickHouse,
                &[
                    ("UInt64", Integer),
                    ("Nullable(Int32)", Integer),
                    ("Decimal(10, 2)", Decimal),
                    ("Float64", Float),
                    ("Bool", Boolean),
                    ("Date32", Date),
                    ("DateTime", Timestamp),
                    ("DateTime64(3, 'UTC')", Timestamp),
                    ("Nullable(LowCardinality(String))", Text),
                    ("UUID", Text),
                ],
            ),
        ];
        for (dialect, types) in cases {
            for (type_name, kind) in *types {
                assert_eq!(
                    dialect.value_kind(Some(type_name)),
                    *kind,
                    "{dialect:?} {type_name}"
                );
            }
            assert_eq!(dialect.value_kind(None), Undeclared, "{dialect:?}");
        }
    }
}
