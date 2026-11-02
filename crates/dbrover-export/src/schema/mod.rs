//! Output schemas: the columns a report's output declares, each with its name, the result
//! column it comes from, a type and Excel's number format, mapped onto a result's columns
//! before any format sees them (`docs/reporting.md`, RP1). Every format then writes the
//! declared columns as it writes any export's.
//!
//! Values stay text (ADR 0004). A declared type checks each value and rewrites it in the
//! type's plain form (`12.50`, `2026-10-01 12:00:00`, `true`), which CSV writes as it is and
//! the Excel writer types; decimals are rounded on their digits, never through `f64`.

mod convert;
mod decimal;

use std::fmt;
use std::str::FromStr;

use dbrover_core::{Cell, ValueKind};

use crate::ExportColumn;

pub use self::convert::CellTarget;

/// What a declared column's values are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Integer,
    /// A decimal number, kept with the digits it came with, or rounded to a scale.
    Decimal(Option<DecimalSpec>),
    Boolean,
    Date,
    /// A date and time with no time zone.
    Timestamp,
}

impl ColumnType {
    /// The kind of value the formats that write types make of the column.
    pub fn kind(self) -> ValueKind {
        match self {
            Self::Text => ValueKind::Text,
            Self::Integer => ValueKind::Integer,
            Self::Decimal(_) => ValueKind::Decimal,
            Self::Boolean => ValueKind::Boolean,
            Self::Date => ValueKind::Date,
            Self::Timestamp => ValueKind::Timestamp,
        }
    }

    /// `text` in the type's plain form, as a CSV file writes it (`12.50`, `2026-10-01`), or
    /// why it isn't a value of the type, as the end of a sentence that starts with the value.
    ///
    /// # Errors
    ///
    /// Why `text` isn't a value of the type: "isn't a date, as 2026-10-01".
    pub fn plain_form(self, text: &str) -> Result<String, String> {
        convert::value(self, None, CellTarget::Text, text)
    }

    fn is_number(self) -> bool {
        matches!(self, Self::Integer | Self::Decimal(_))
    }
}

/// `decimal(p,s)`: rounded to `s` places, with at most `p` digits in all, as SQL's `DECIMAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalSpec {
    precision: u8,
    scale: u8,
}

impl DecimalSpec {
    /// SQL's largest common precision: PostgreSQL's `numeric` takes more, but DuckDB, SQL
    /// Server and MySQL stop at 38 and 65, and 76 is DuckDB's `HUGEINT` decimal limit.
    pub const MAX_PRECISION: u8 = 76;

    /// `None` unless `1 <= precision <= 76` and `scale <= precision`.
    pub fn new(precision: u8, scale: u8) -> Option<Self> {
        ((1..=Self::MAX_PRECISION).contains(&precision) && scale <= precision)
            .then_some(Self { precision, scale })
    }

    pub fn precision(self) -> u8 {
        self.precision
    }

    pub fn scale(self) -> u8 {
        self.scale
    }
}

/// A type name a report file can't take.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{0:?} is not a type; a column's type is text, integer, decimal, decimal(p,s), boolean, \
     date or timestamp"
)]
pub struct UnknownType(String);

impl FromStr for ColumnType {
    type Err = UnknownType;

    /// As a report file writes them: `decimal(18,2)`, with spaces allowed inside the brackets.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownType(name.to_owned());
        let lower = name.trim().to_ascii_lowercase();
        Ok(match lower.as_str() {
            "text" => Self::Text,
            "integer" => Self::Integer,
            "decimal" => Self::Decimal(None),
            "boolean" => Self::Boolean,
            "date" => Self::Date,
            "timestamp" => Self::Timestamp,
            other => {
                let arguments = other
                    .strip_prefix("decimal")
                    .map(str::trim_start)
                    .and_then(|rest| rest.strip_prefix('('))
                    .and_then(|rest| rest.strip_suffix(')'))
                    .ok_or_else(unknown)?;
                let (precision, scale) = arguments.split_once(',').ok_or_else(unknown)?;
                let number = |text: &str| text.trim().parse::<u8>().map_err(|_| unknown());
                let spec =
                    DecimalSpec::new(number(precision)?, number(scale)?).ok_or_else(|| {
                        UnknownType(format!(
                            "{name} (a decimal's precision is 1 to {}, and its scale at most its \
                         precision)",
                            DecimalSpec::MAX_PRECISION
                        ))
                    })?;
                Self::Decimal(Some(spec))
            }
        })
    }
}

impl fmt::Display for ColumnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => f.write_str("text"),
            Self::Integer => f.write_str("integer"),
            Self::Decimal(None) => f.write_str("decimal"),
            Self::Decimal(Some(spec)) => write!(f, "decimal({},{})", spec.precision, spec.scale),
            Self::Boolean => f.write_str("boolean"),
            Self::Date => f.write_str("date"),
            Self::Timestamp => f.write_str("timestamp"),
        }
    }
}

/// What to do with a value that doesn't fit its column's type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IfInvalid {
    /// Fail the whole output, naming the value.
    #[default]
    Fail,
    /// Write an empty cell.
    Null,
    /// Write the value as it came.
    Text,
}

impl FromStr for IfInvalid {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "fail" => Ok(Self::Fail),
            "null" => Ok(Self::Null),
            "text" => Ok(Self::Text),
            other => Err(format!(
                "{other:?} is not what to do with a value that doesn't fit; it is \"fail\", \
                 \"null\" or \"text\""
            )),
        }
    }
}

/// A declared column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaColumn {
    /// The header.
    pub name: String,
    /// The result column its values come from.
    pub from: String,
    /// `None` writes the values as they come, typed as an export of the result would be.
    pub column_type: Option<ColumnType>,
    /// Excel's number format, such as `#,##0.00` or `yyyy-mm-dd hh:mm`. Other formats
    /// ignore it.
    pub excel_format: Option<String>,
    /// The zone a timestamp with an offset is converted to; without one, such a value
    /// doesn't fit.
    pub time_zone: Option<jiff::tz::TimeZone>,
    pub if_invalid: IfInvalid,
}

impl SchemaColumn {
    /// The column `from`, under its own name, as it comes.
    pub fn same(from: impl Into<String>) -> Self {
        let from = from.into();
        Self {
            name: from.clone(),
            from,
            column_type: None,
            excel_format: None,
            time_zone: None,
            if_invalid: IfInvalid::Fail,
        }
    }
}

/// A declared column that names no column of the result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("column {name:?} comes from {from:?}, which the result doesn't have; it has {}", .available.join(", "))]
pub struct MissingColumn {
    pub name: String,
    pub from: String,
    pub available: Vec<String>,
}

/// A value that doesn't fit its column, for the run's error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("row {row}, column {column:?}: {value:?} {reason}")]
pub struct InvalidValue {
    pub column: String,
    /// Counted from 1, the header not counted.
    pub row: u64,
    /// Cut to a hundred characters.
    pub value: String,
    pub reason: String,
}

/// Values longer than this are cut in [`InvalidValue`], so an error stays a line.
const SHOWN_VALUE_CHARS: usize = 100;

/// An output's columns, mapped onto a result's.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSchema {
    columns: Vec<Mapped>,
}

#[derive(Debug, Clone, PartialEq)]
struct Mapped {
    /// The result column's position.
    index: usize,
    source: ExportColumn,
    declared: SchemaColumn,
}

impl OutputSchema {
    /// `declared`, each column found among `result`'s by its `from` (the first of that name).
    ///
    /// # Errors
    ///
    /// [`MissingColumn`] for the first declared column the result doesn't have.
    pub fn new(declared: &[SchemaColumn], result: &[ExportColumn]) -> Result<Self, MissingColumn> {
        let columns = declared
            .iter()
            .map(|column| {
                let index = result
                    .iter()
                    .position(|source| source.name == column.from)
                    .ok_or_else(|| MissingColumn {
                        name: column.name.clone(),
                        from: column.from.clone(),
                        available: result.iter().map(|source| source.name.clone()).collect(),
                    })?;
                Ok(Mapped {
                    index,
                    source: result[index].clone(),
                    declared: column.clone(),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { columns })
    }

    /// Every column of `result`, as it comes.
    pub fn all(result: &[ExportColumn]) -> Self {
        let columns = result
            .iter()
            .enumerate()
            .map(|(index, source)| Mapped {
                index,
                source: source.clone(),
                declared: SchemaColumn::same(&source.name),
            })
            .collect();
        Self { columns }
    }

    /// The columns the formats are given.
    pub fn export_columns(&self) -> Vec<ExportColumn> {
        self.columns
            .iter()
            .map(|mapped| {
                let declared = &mapped.declared;
                let (type_name, kind) = match declared.column_type {
                    Some(column_type) => (Some(column_type.to_string()), column_type.kind()),
                    None => (mapped.source.type_name.clone(), mapped.source.kind),
                };
                ExportColumn {
                    name: declared.name.clone(),
                    type_name,
                    kind,
                    excel_format: declared.excel_format.clone(),
                }
            })
            .collect()
    }
}

/// Converts a result's rows into an output's, counting as it goes what the run's result
/// reports.
pub struct RowConverter {
    schema: OutputSchema,
    target: CellTarget,
    /// Rows converted so far.
    rows: u64,
    /// Per column, cells the Excel writer will write as text because they have more digits
    /// than its numbers hold.
    beyond_excel: Vec<u64>,
}

impl RowConverter {
    pub fn new(schema: OutputSchema, target: CellTarget) -> Self {
        let beyond_excel = vec![0; schema.columns.len()];
        Self {
            schema,
            target,
            rows: 0,
            beyond_excel,
        }
    }

    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// The output's row for the result's `row`.
    ///
    /// # Errors
    ///
    /// [`InvalidValue`] for the first value that doesn't fit a column that fails on one.
    pub fn convert(&mut self, row: &[Cell]) -> Result<Vec<Cell>, InvalidValue> {
        self.rows += 1;
        let mut converted = Vec::with_capacity(self.schema.columns.len());
        for (position, mapped) in self.schema.columns.iter().enumerate() {
            let value = row.get(mapped.index).cloned().flatten();
            let declared = &mapped.declared;
            let (Some(text), Some(column_type)) = (value.as_deref(), declared.column_type) else {
                converted.push(value);
                continue;
            };
            let cell =
                match convert::value(column_type, declared.time_zone.as_ref(), self.target, text) {
                    Ok(written) => {
                        if self.target == CellTarget::Excel
                            && column_type.is_number()
                            && convert::beyond_excel(&written)
                        {
                            self.beyond_excel[position] += 1;
                        }
                        Some(written)
                    }
                    Err(reason) => match declared.if_invalid {
                        IfInvalid::Fail => {
                            return Err(InvalidValue {
                                column: declared.name.clone(),
                                row: self.rows,
                                value: text.chars().take(SHOWN_VALUE_CHARS).collect(),
                                reason,
                            });
                        }
                        IfInvalid::Null => None,
                        IfInvalid::Text => Some(text.to_owned()),
                    },
                };
            converted.push(cell);
        }
        Ok(converted)
    }

    /// One sentence per column some of whose cells Excel holds only as text.
    pub fn warnings(&self) -> Vec<String> {
        self.schema
            .columns
            .iter()
            .zip(&self.beyond_excel)
            .filter(|(_, count)| **count > 0)
            .map(|(mapped, count)| {
                let (cells, were) = if *count == 1 {
                    ("cell", "was")
                } else {
                    ("cells", "were")
                };
                format!(
                    "column {:?}: {count} {cells} had more digits than the 15 Excel's numbers \
                     hold, so {were} written as text; a type that rounds, such as \
                     decimal(38,2), keeps them numbers",
                    mapped.declared.name
                )
            })
            .collect()
    }
}
