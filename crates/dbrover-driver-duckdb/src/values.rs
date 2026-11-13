//! Result columns and values as text, formatted by Arrow, the form DuckDB hands its results
//! over in. Arrow formats every type DuckDB has (lists, structs, maps, intervals, huge
//! integers), so DbRover needs no formatter per type; dates and times are written as DuckDB's
//! own client shows them rather than in Arrow's ISO 8601.
//!
//! What DuckDB's Arrow export loses, it loses here too: a `HUGEINT` beyond 38 digits arrives
//! with its last digit cut (it becomes a 38-digit decimal), `TIME WITH TIME ZONE` without its
//! offset, and `BIT` as bytes. DuckDB's `arrow_lossless_conversion` setting would keep them,
//! but would also turn booleans, UUIDs and times into raw bytes. Casting such a column to
//! `VARCHAR` shows it whole.

use std::ops::Range;
use std::sync::Arc;

use dbrover_core::{Cell, ColumnInfo, Row, bulk_kind};
use duckdb::arrow::array::{Array, ArrayRef, AsArray, StringArray, StructArray};
use duckdb::arrow::compute::cast;
use duckdb::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use duckdb::arrow::error::ArrowError;
use duckdb::arrow::record_batch::RecordBatch;
use duckdb::arrow::util::display::{ArrayFormatter, FormatOptions};

/// Binary values longer than this are shown as their size, not their bytes: a result grid is
/// no place for a megabyte of hex.
const MAX_BINARY_HEX_BYTES: usize = 64;

/// `2026-10-24 07:28:23.93`, as DuckDB writes a timestamp, rather than Arrow's `T` between the
/// date and the time.
const FORMAT: FormatOptions<'static> = FormatOptions::new()
    .with_display_error(true)
    .with_timestamp_format(Some("%Y-%m-%d %H:%M:%S%.f"))
    .with_timestamp_tz_format(Some("%Y-%m-%d %H:%M:%S%.f%:z"))
    .with_datetime_format(Some("%Y-%m-%d %H:%M:%S%.f"));

pub(crate) fn column_info(schema: &Schema) -> Vec<ColumnInfo> {
    schema
        .fields()
        .iter()
        .map(|field| ColumnInfo {
            name: field.name().clone(),
            type_name: type_name(field.data_type()),
        })
        .collect()
}

/// The column's type as DuckDB names it, from the Arrow type it arrives as.
fn type_name(data_type: &DataType) -> Option<String> {
    Some(
        match data_type {
            DataType::Boolean => "boolean",
            DataType::Int8 => "tinyint",
            DataType::Int16 => "smallint",
            DataType::Int32 => "integer",
            DataType::Int64 => "bigint",
            DataType::UInt8 => "utinyint",
            DataType::UInt16 => "usmallint",
            DataType::UInt32 => "uinteger",
            DataType::UInt64 => "ubigint",
            DataType::Float32 => "float",
            DataType::Float64 => "double",
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "varchar",
            DataType::Binary | DataType::LargeBinary | DataType::BinaryView => "blob",
            DataType::Date32 | DataType::Date64 => "date",
            DataType::Time32(_) | DataType::Time64(_) => "time",
            DataType::Timestamp(_, None) => "timestamp",
            DataType::Timestamp(_, Some(_)) => "timestamp with time zone",
            DataType::Interval(_) | DataType::Duration(_) => "interval",
            DataType::Decimal128(precision, scale) | DataType::Decimal256(precision, scale) => {
                return Some(format!("decimal({precision}, {scale})"));
            }
            DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(..)
            | DataType::ListView(_) => "list",
            DataType::Struct(_) => "struct",
            DataType::Map(..) => "map",
            DataType::Union(..) => "union",
            DataType::Dictionary(..) => "enum",
            _ => return None,
        }
        .to_owned(),
    )
}

/// The offset Arrow can format a time zone as without a time zone database.
const UTC_OFFSET: &str = "+00:00";

/// Rows `range` of `chunk`, each column formatted as text.
pub(crate) fn rows(chunk: &StructArray, range: Range<usize>) -> Vec<Row> {
    let arrays: Vec<ArrayRef> = chunk.columns().iter().map(formattable).collect();
    let columns: Vec<Column<'_>> = arrays.iter().map(|array| Column::new(array)).collect();
    range
        .map(|row| columns.iter().map(|column| column.cell(row)).collect())
        .collect()
}

/// The schema of the batches a result of `schema` is read in (ADR 0007): each column of a
/// bulk type as it is, every other as text.
pub(crate) fn batch_schema(schema: &Schema) -> SchemaRef {
    let fields: Vec<Field> = schema
        .fields()
        .iter()
        .map(|field| match bulk_kind(field.data_type()) {
            Some(_) => field.as_ref().clone(),
            None => Field::new(field.name(), DataType::Utf8, true),
        })
        .collect();
    Arc::new(Schema::new(fields))
}

/// Rows `range` of `chunk` as a batch of `schema`, from [`batch_schema`]: a column of a bulk
/// type as DuckDB handed it over, every other as the text [`rows`] gives it.
pub(crate) fn batch(
    chunk: &StructArray,
    schema: &SchemaRef,
    range: Range<usize>,
) -> Result<RecordBatch, ArrowError> {
    let rows = chunk.slice(range.start, range.len());
    let columns: Vec<ArrayRef> = rows
        .columns()
        .iter()
        .zip(schema.fields())
        .map(|(array, field)| {
            if array.data_type() == field.data_type() {
                Arc::clone(array)
            } else {
                text_column(array)
            }
        })
        .collect();
    RecordBatch::try_new(Arc::clone(schema), columns)
}

/// Each value of `array` as the text [`rows`] gives it.
fn text_column(array: &ArrayRef) -> ArrayRef {
    let array = formattable(array);
    let column = Column::new(array.as_ref());
    let values: StringArray = (0..array.len()).map(|row| column.cell(row)).collect();
    Arc::new(values)
}

/// `array`, with a `TIMESTAMP WITH TIME ZONE` labelled `+00:00` rather than `UTC`, the same
/// instant: Arrow formats only offsets without its `chrono-tz` feature, and DuckDB keeps these
/// values in UTC unless its ICU extension (not compiled in) gives it time zones.
fn formattable(array: &ArrayRef) -> ArrayRef {
    match array.data_type() {
        DataType::Timestamp(unit, Some(zone)) if zone.as_ref() != UTC_OFFSET => {
            let relabelled = DataType::Timestamp(*unit, Some(UTC_OFFSET.into()));
            cast(array, &relabelled).unwrap_or_else(|_| array.clone())
        }
        _ => array.clone(),
    }
}

/// One column of a chunk, ready to format.
struct Column<'a> {
    array: &'a dyn Array,
    formatter: Option<ArrayFormatter<'a>>,
}

impl<'a> Column<'a> {
    fn new(array: &'a dyn Array) -> Self {
        Self {
            array,
            formatter: ArrayFormatter::try_new(array, &FORMAT).ok(),
        }
    }

    fn cell(&self, row: usize) -> Cell {
        if self.array.is_null(row) {
            return None;
        }
        if let Some(bytes) = binary(self.array, row) {
            return Some(hex(bytes));
        }
        Some(match &self.formatter {
            Some(formatter) => formatter.value(row).to_string(),
            None => format!("<{} value>", self.array.data_type()),
        })
    }
}

fn binary(array: &dyn Array, row: usize) -> Option<&[u8]> {
    match array.data_type() {
        DataType::Binary => Some(array.as_binary::<i32>().value(row)),
        DataType::LargeBinary => Some(array.as_binary::<i64>().value(row)),
        DataType::BinaryView => Some(array.as_binary_view().value(row)),
        _ => None,
    }
}

/// `\xDE\xAD`, as DuckDB writes a blob, or its size if it is long.
fn hex(bytes: &[u8]) -> String {
    if bytes.len() > MAX_BINARY_HEX_BYTES {
        return format!("<blob, {} bytes>", bytes.len());
    }
    bytes.iter().map(|b| format!("\\x{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_short_blobs_as_duckdb_does_and_long_ones_as_their_size() {
        assert_eq!(hex(&[0xde, 0xad]), "\\xDE\\xAD");
        assert_eq!(hex(&[0; 65]), "<blob, 65 bytes>");
    }
}
