//! The type each column of a loaded source gets in DuckDB (`docs/reporting.md`, "Loading a
//! source"): the driver's, where it says more than the values can, else the narrowest type
//! that holds every value exactly, as the text it arrived as.
//!
//! The values decide because PostgreSQL's text protocol reports no types at all, and
//! `numeric`'s precision is per value. A column is typed after its last row is in, so one
//! decimal with more places than the rest widens the whole column rather than failing it.
//!
//! A column whose type is known before its first value, declared in the file or stated
//! exactly by its driver, is made of that type and loaded into it (RS3c); only the others are
//! typed by their values.

use dbrover_core::{DriverKind, ValueKind};
use dbrover_driver_duckdb::DuckDbType;
use dbrover_export::{ColumnType, ExportColumn};

use crate::SourceColumn;

/// The most digits a `BIGINT` holds for any value: 19 fit some of them, but not all.
const BIGINT_DIGITS: u8 = 18;
/// The most digits a `HUGEINT` holds for any value, and DuckDB's `DECIMAL` too.
const MAX_DIGITS: u8 = DuckDbType::MAX_DECIMAL_DIGITS;

/// What a column's values have been so far, from the narrowest to the widest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// No value but `NULL`.
    Nothing,
    Boolean,
    /// Whole numbers of at most this many digits.
    Integer {
        digits: u8,
    },
    /// Decimal numbers: the most digits seen before the point, and after it.
    Decimal {
        whole: u8,
        scale: u8,
    },
    /// A number with an exponent, or `NaN` or `Infinity`, which only a float holds.
    Float,
    Date,
    Timestamp,
    TimestampTz,
    /// A number of more digits than DuckDB holds, which can only be text.
    TooPrecise,
    Text,
}

impl Seen {
    fn of(value: &str) -> Self {
        number(value)
            .or_else(|| boolean(value))
            .or_else(|| time(value))
            .unwrap_or(Self::Text)
    }

    /// The narrowest of the kinds that holds both.
    fn widen(self, other: Self) -> Self {
        use Seen::{Date, Decimal, Float, Integer, Nothing, Text, Timestamp, TooPrecise};
        match (self, other) {
            (Nothing, seen) | (seen, Nothing) => seen,
            (TooPrecise, _) | (_, TooPrecise) => TooPrecise,
            (Integer { digits: a }, Integer { digits: b }) => Integer { digits: a.max(b) },
            (Integer { digits }, Decimal { whole, scale })
            | (Decimal { whole, scale }, Integer { digits }) => decimal(whole.max(digits), scale),
            (Decimal { whole: a, scale: s }, Decimal { whole: b, scale: t }) => {
                decimal(a.max(b), s.max(t))
            }
            (Integer { .. } | Decimal { .. } | Float, Integer { .. } | Decimal { .. } | Float) => {
                Float
            }
            (Date | Timestamp, Date | Timestamp) if self != other => Timestamp,
            (a, b) if a == b => a,
            _ => Text,
        }
    }

    fn duckdb_type(self) -> DuckDbType {
        match self {
            Self::Boolean => DuckDbType::Boolean,
            Self::Integer { digits } if digits <= BIGINT_DIGITS => DuckDbType::BigInt,
            Self::Integer { .. } => DuckDbType::HugeInt,
            Self::Decimal { whole, scale } => DuckDbType::Decimal {
                precision: whole + scale,
                scale,
            },
            Self::Float => DuckDbType::Double,
            Self::Date => DuckDbType::Date,
            Self::Timestamp => DuckDbType::Timestamp,
            Self::TimestampTz => DuckDbType::TimestampTz,
            Self::Nothing | Self::TooPrecise | Self::Text => DuckDbType::Varchar,
        }
    }
}

/// A decimal of `whole` and `scale` digits, unless that is more than DuckDB holds.
fn decimal(whole: u8, scale: u8) -> Seen {
    if u16::from(whole) + u16::from(scale) > u16::from(MAX_DIGITS) {
        Seen::TooPrecise
    } else {
        Seen::Decimal { whole, scale }
    }
}

/// A whole or decimal number as a database writes one: `-12`, `0.5`, `1234.000`, but not
/// `007`, which keeps its zeros as text. With an exponent, a float.
fn number(value: &str) -> Option<Seen> {
    if matches!(value, "NaN" | "Infinity" | "-Infinity") {
        return Some(Seen::Float);
    }
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    if unsigned.contains(['e', 'E']) {
        let is_float =
            unsigned.parse::<f64>().is_ok() && unsigned.starts_with(|c: char| c.is_ascii_digit());
        return is_float.then_some(Seen::Float);
    }
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits_only = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits_only(whole) || !digits_only(fraction) {
        return None;
    }
    if whole.len() > 1 && whole.starts_with('0') {
        return None;
    }
    let whole_digits = u8::try_from(whole.trim_start_matches('0').len()).ok();
    let scale = u8::try_from(fraction.len()).ok();
    let (Some(whole_digits), Some(scale)) = (whole_digits, scale) else {
        return Some(Seen::TooPrecise);
    };
    Some(match (unsigned.contains('.'), whole_digits.max(1)) {
        (false, digits) if digits > MAX_DIGITS => Seen::TooPrecise,
        (false, digits) => Seen::Integer { digits },
        (true, _) => decimal(whole_digits, scale),
    })
}

/// `t` and `f`, as PostgreSQL writes booleans, or `true` and `false`.
fn boolean(value: &str) -> Option<Seen> {
    ["t", "f", "true", "false"]
        .iter()
        .any(|word| word.eq_ignore_ascii_case(value))
        .then_some(Seen::Boolean)
}

/// `2026-10-01`, `2026-10-01 12:00:00.5`, `2026-10-01T12:00:00+02`.
fn time(value: &str) -> Option<Seen> {
    let (date, rest) = value.split_at_checked(10)?;
    let date_shaped = date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !date_shaped || date.parse::<jiff::civil::Date>().is_err() {
        return None;
    }
    if rest.is_empty() {
        return Some(Seen::Date);
    }
    let time = rest.strip_prefix([' ', 'T'])?;
    let iso = format!("{date}T{time}");
    if iso.parse::<jiff::Timestamp>().is_ok() {
        Some(Seen::TimestampTz)
    } else if !has_offset(time) && iso.parse::<jiff::civil::DateTime>().is_ok() {
        Some(Seen::Timestamp)
    } else {
        None
    }
}

/// Whether a time of day ends with an offset or a zone, which a civil date and time would
/// drop rather than refuse.
fn has_offset(time: &str) -> bool {
    time.contains(['+', 'Z', 'z', '[']) || time.get(2..).is_some_and(|rest| rest.contains('-'))
}

/// How far a driver's column types hold before any value is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriverTypes {
    /// Every value is of its column's type, as in PostgreSQL or DuckDB.
    Strict,
    /// A column's type is a preference its values may ignore, as SQLite's affinities are: a
    /// `REAL` column can hold text.
    Affinities,
}

impl DriverTypes {
    pub(crate) fn of(driver: DriverKind) -> Self {
        match driver {
            DriverKind::Sqlite => Self::Affinities,
            _ => Self::Strict,
        }
    }
}

/// The types of a source's columns, worked out a page at a time as it loads.
pub(crate) struct ColumnTyper {
    columns: Vec<(ExportColumn, Seen)>,
    /// Each column's declared type, when the source declares its columns.
    declared: Option<Vec<ColumnType>>,
    /// Each column's type, when known before its first value; its values aren't looked at.
    known: Vec<Option<DuckDbType>>,
}

impl ColumnTyper {
    /// For `columns`, typed by `declared` when the source declares them, in the same order,
    /// else by what `drivers` says of the columns' types and by their values.
    pub(crate) fn new(
        columns: &[ExportColumn],
        declared: Option<&[SourceColumn]>,
        drivers: DriverTypes,
    ) -> Self {
        let declared: Option<Vec<ColumnType>> =
            declared.map(|declared| declared.iter().map(|column| column.column_type).collect());
        let known = match &declared {
            Some(types) => types
                .iter()
                .map(|declared| known_declared(*declared))
                .collect(),
            None => columns
                .iter()
                .map(|column| known_from_driver(column, drivers))
                .collect(),
        };
        Self {
            columns: columns
                .iter()
                .map(|column| (column.clone(), Seen::Nothing))
                .collect(),
            declared,
            known,
        }
    }

    /// Each column's type when it is known before its first value, which its table is made
    /// with; `None` for a column its values type.
    pub(crate) fn known(&self) -> &[Option<DuckDbType>] {
        &self.known
    }

    /// The columns' names, in their order.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().map(|(column, _)| column.name.as_str())
    }

    /// Whether the file declares the columns' types.
    pub(crate) fn is_declared(&self) -> bool {
        self.declared.is_some()
    }

    pub(crate) fn see(&mut self, page: &[dbrover_core::Row]) {
        for row in page {
            let columns = self.columns.iter_mut().zip(&self.known);
            for (((_, seen), known), value) in columns.zip(row) {
                if let (None, Some(value)) = (known, value) {
                    *seen = seen.widen(Seen::of(value));
                }
            }
        }
    }

    /// Each column's type, and a sentence for each column that could only be text because
    /// its numbers have more digits than DuckDB holds.
    pub(crate) fn types(&self) -> (Vec<DuckDbType>, Vec<String>) {
        let mut warnings = Vec::new();
        let types = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, (column, seen))| {
                if let Some(Some(known)) = self.known.get(index) {
                    return *known;
                }
                let declared = self.declared.as_ref().and_then(|types| types.get(index));
                let chosen = match declared {
                    Some(declared) => declared_type(*declared, *seen),
                    None => column_type(column, *seen),
                };
                if chosen == DuckDbType::Varchar && *seen == Seen::TooPrecise {
                    warnings.push(format!(
                        "column {:?} has numbers of more than {MAX_DIGITS} digits, more than \
                         DuckDB's DECIMAL holds, so it was loaded as text; cast it in \
                         [combine] to the number it needs to be",
                        column.name
                    ));
                }
                chosen
            })
            .collect();
        (types, warnings)
    }
}

/// The driver's type where it says more than the values: text stays text, a float stays a
/// float, and a decimal keeps its declared precision. Otherwise the values'.
///
/// Text whose every value is a date or a time is the exception: SQLite has no type for
/// either and keeps them as text, while digits in a text column are codes (`0042`, `1000`),
/// which stay text.
fn column_type(column: &ExportColumn, seen: Seen) -> DuckDbType {
    match column.kind {
        ValueKind::Text => match seen {
            Seen::Date | Seen::Timestamp | Seen::TimestampTz => seen.duckdb_type(),
            _ => DuckDbType::Varchar,
        },
        ValueKind::Float => match seen {
            Seen::Nothing | Seen::Integer { .. } | Seen::Decimal { .. } | Seen::Float => {
                DuckDbType::Double
            }
            _ => DuckDbType::Varchar,
        },
        ValueKind::Decimal => {
            declared_decimal(column.type_name.as_deref()).unwrap_or_else(|| seen.duckdb_type())
        }
        ValueKind::Integer
        | ValueKind::Boolean
        | ValueKind::Date
        | ValueKind::Timestamp
        | ValueKind::TimestampWithZone
        | ValueKind::Undeclared => seen.duckdb_type(),
    }
}

/// A declared column's type before any value is seen; `None` for a `decimal` without its
/// precision, which its values give.
fn known_declared(declared: ColumnType) -> Option<DuckDbType> {
    match declared {
        ColumnType::Decimal(None) => None,
        other => Some(declared_type(other, Seen::Nothing)),
    }
}

/// The type a driver states exactly for `column`: a decimal with its precision, or a float,
/// which only a float holds. Everything else, its values decide, as [`column_type`] does.
fn known_from_driver(column: &ExportColumn, drivers: DriverTypes) -> Option<DuckDbType> {
    if drivers == DriverTypes::Affinities {
        return None;
    }
    match column.kind {
        ValueKind::Decimal => declared_decimal(column.type_name.as_deref()),
        ValueKind::Float => Some(DuckDbType::Double),
        _ => None,
    }
}

/// The DuckDB type of a declared column before any value is seen, as an empty table of it is
/// made to explain a report (RS2d).
pub(crate) fn declared_column_type(declared: ColumnType) -> DuckDbType {
    declared_type(declared, Seen::Nothing)
}

/// The DuckDB type of a declared column (`docs/reporting.md`, "Declared columns"). A
/// `decimal` without its precision is typed by its values, which were checked to be numbers.
fn declared_type(declared: ColumnType, seen: Seen) -> DuckDbType {
    match declared {
        ColumnType::Text => DuckDbType::Varchar,
        ColumnType::Integer => DuckDbType::BigInt,
        ColumnType::Decimal(Some(spec)) => DuckDbType::Decimal {
            precision: spec.precision(),
            scale: spec.scale(),
        },
        ColumnType::Decimal(None) => match seen {
            Seen::Integer { digits } => DuckDbType::Decimal {
                precision: digits,
                scale: 0,
            },
            Seen::Nothing => DuckDbType::Decimal {
                precision: MAX_DIGITS,
                scale: 0,
            },
            other => other.duckdb_type(),
        },
        ColumnType::Boolean => DuckDbType::Boolean,
        ColumnType::Date => DuckDbType::Date,
        ColumnType::Timestamp => DuckDbType::Timestamp,
    }
}

/// `decimal(18, 2)` or `numeric(38,18)` as a `DECIMAL` DuckDB holds; `None` for a decimal
/// without its precision, or with more digits than DuckDB holds.
fn declared_decimal(type_name: Option<&str>) -> Option<DuckDbType> {
    let (_, arguments) = type_name?.split_once('(')?;
    let (precision, scale) = arguments.strip_suffix(')')?.split_once(',')?;
    let precision: u8 = precision.trim().parse().ok()?;
    let scale: u8 = scale.trim().parse().ok()?;
    ((1..=MAX_DIGITS).contains(&precision) && scale <= precision)
        .then_some(DuckDbType::Decimal { precision, scale })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(kind: ValueKind, type_name: Option<&str>, values: &[Option<&str>]) -> DuckDbType {
        let column = ExportColumn {
            name: "c".to_owned(),
            type_name: type_name.map(str::to_owned),
            kind,
            excel_format: None,
        };
        let mut typer =
            ColumnTyper::new(std::slice::from_ref(&column), None, DriverTypes::Affinities);
        let rows: Vec<dbrover_core::Row> = values
            .iter()
            .map(|value| vec![value.map(str::to_owned)])
            .collect();
        typer.see(&rows);
        typer.types().0[0]
    }

    fn undeclared(values: &[&str]) -> DuckDbType {
        let values: Vec<Option<&str>> = values.iter().copied().map(Some).collect();
        typed(ValueKind::Undeclared, None, &values)
    }

    #[test]
    fn gives_each_column_the_narrowest_type_that_holds_its_values() {
        let decimal = |precision, scale| DuckDbType::Decimal { precision, scale };
        let cases: [(&[&str], DuckDbType); 22] = [
            (&["1", "-42", "0"], DuckDbType::BigInt),
            (&["123456789012345678"], DuckDbType::BigInt),
            (&["1234567890123456789"], DuckDbType::HugeInt),
            (&["12.50", "-3.1"], decimal(4, 2)),
            (&["7", "0.000000000000000001"], decimal(19, 18)),
            (&["0.5"], decimal(1, 1)),
            (
                &["12345678901234567890.123456789012345678"],
                decimal(38, 18),
            ),
            (&["1.5e+20", "3"], DuckDbType::Double),
            (&["NaN", "0.5"], DuckDbType::Double),
            (&["t", "f", "TRUE"], DuckDbType::Boolean),
            (&["2026-10-01", "2026-02-28"], DuckDbType::Date),
            (
                &["2026-10-01 12:00:00.5", "2026-10-01T08:00"],
                DuckDbType::Timestamp,
            ),
            (
                &["2026-10-01", "2026-10-01 12:00:00"],
                DuckDbType::Timestamp,
            ),
            (
                &["2026-10-01 12:00:00+02", "2026-10-01T10:00:00Z"],
                DuckDbType::TimestampTz,
            ),
            (&["2026-10-01 12:00:00-05:30"], DuckDbType::TimestampTz),
            (&["0042", "1000"], DuckDbType::Varchar),
            (&["1", "t"], DuckDbType::Varchar),
            (&["2026-10-01", "12"], DuckDbType::Varchar),
            (&["2026-02-30"], DuckDbType::Varchar),
            (
                &["2026-10-01 12:00:00", "2026-10-01 12:00:00+02"],
                DuckDbType::Varchar,
            ),
            (&["", "1"], DuckDbType::Varchar),
            (&["1e5x", ".5", "1."], DuckDbType::Varchar),
        ];
        for (values, expected) in cases {
            assert_eq!(undeclared(values), expected, "{values:?}");
        }
        assert_eq!(
            typed(ValueKind::Undeclared, None, &[None, None]),
            DuckDbType::Varchar
        );
        assert_eq!(
            typed(ValueKind::Undeclared, None, &[None, Some("3")]),
            DuckDbType::BigInt
        );
    }

    #[test]
    fn keeps_what_the_driver_declares_where_it_says_more() {
        let decimal = |precision, scale| DuckDbType::Decimal { precision, scale };
        assert_eq!(
            typed(ValueKind::Text, Some("text"), &[Some("1000")]),
            DuckDbType::Varchar
        );
        assert_eq!(
            typed(
                ValueKind::Text,
                Some("text"),
                &[Some("2026-10-01 10:15:00")]
            ),
            DuckDbType::Timestamp
        );
        assert_eq!(
            typed(ValueKind::Float, Some("double"), &[Some("1.5")]),
            DuckDbType::Double
        );
        assert_eq!(
            typed(ValueKind::Decimal, Some("decimal(18, 2)"), &[Some("1.5")]),
            decimal(18, 2)
        );
        assert_eq!(
            typed(
                ValueKind::Decimal,
                Some("numeric"),
                &[Some("1.50"), Some("10")]
            ),
            decimal(4, 2)
        );
        assert_eq!(
            typed(ValueKind::Decimal, Some("decimal(76, 2)"), &[Some("1.5")]),
            decimal(2, 1)
        );
        assert_eq!(
            typed(ValueKind::Integer, Some("integer"), &[Some("7")]),
            DuckDbType::BigInt
        );
    }

    #[test]
    fn knows_a_columns_type_before_its_values_where_its_driver_or_the_file_says_it() {
        let column = |kind, type_name: &str| ExportColumn {
            name: "c".to_owned(),
            type_name: Some(type_name.to_owned()),
            kind,
            excel_format: None,
        };
        let columns = [
            column(ValueKind::Decimal, "numeric(18,2)"),
            column(ValueKind::Decimal, "numeric"),
            column(ValueKind::Float, "double precision"),
            column(ValueKind::Integer, "bigint"),
            column(ValueKind::Text, "text"),
        ];
        let decimal = DuckDbType::Decimal {
            precision: 18,
            scale: 2,
        };
        let strict = ColumnTyper::new(&columns, None, DriverTypes::Strict);
        assert_eq!(
            strict.known(),
            [Some(decimal), None, Some(DuckDbType::Double), None, None]
        );
        let sqlite = ColumnTyper::new(&columns, None, DriverTypes::Affinities);
        assert_eq!(
            sqlite.known(),
            [None; 5],
            "SQLite's types are only affinities"
        );

        let declared = [
            ("amount", ColumnType::Decimal(None)),
            ("count", ColumnType::Integer),
            ("day", ColumnType::Date),
        ]
        .map(|(name, column_type)| SourceColumn {
            name: name.to_owned(),
            from: name.to_owned(),
            column_type,
            default: None,
        });
        let typer = ColumnTyper::new(&columns[..3], Some(&declared), DriverTypes::Affinities);
        assert_eq!(
            typer.known(),
            [None, Some(DuckDbType::BigInt), Some(DuckDbType::Date)]
        );
    }

    #[test]
    fn keeps_a_known_type_whatever_the_values_look_like() {
        let column = ExportColumn {
            name: "ratio".to_owned(),
            type_name: Some("Float64".to_owned()),
            kind: ValueKind::Float,
            excel_format: None,
        };
        let mut typer = ColumnTyper::new(&[column], None, DriverTypes::Strict);
        // ClickHouse's infinity, which DuckDB casts and the values' reading doesn't know.
        typer.see(&[vec![Some("inf".to_owned())]]);
        assert_eq!(typer.types().0, [DuckDbType::Double]);
    }

    #[test]
    fn warns_about_numbers_too_precise_for_duckdb() {
        let column = ExportColumn::text("amount");
        let undeclared = ExportColumn {
            kind: ValueKind::Undeclared,
            ..column
        };
        let mut typer = ColumnTyper::new(&[undeclared], None, DriverTypes::Strict);
        typer.see(&[
            vec![Some("123456789012345678901.123456789012345678".to_owned())],
            vec![Some("1".to_owned())],
        ]);
        let (types, warnings) = typer.types();
        assert_eq!(types, [DuckDbType::Varchar]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("column \"amount\" has numbers of more than 38 digits"));
    }
}
