//! What a part of a source's rows weighs, a page of text or an Arrow batch (ADR 0007), and the
//! DuckDB type a batch's column is loaded as.

use arrow_array::cast::AsArray;
use arrow_array::{Array, GenericStringArray, OffsetSizeTrait};
use arrow_schema::DataType;
use dbrover_core::{Row, ValueKind, bulk_kind};
use dbrover_driver_duckdb::DuckDbType;

use crate::SourcePart;

impl SourcePart {
    pub(crate) fn is_empty(&self) -> bool {
        self.rows() == 0
    }

    pub(crate) fn rows(&self) -> u64 {
        match self {
            Self::Rows(page) => page.len() as u64,
            Self::Batch(batch) => batch.num_rows() as u64,
        }
    }

    /// The bytes of its values: as the text they arrive as for a page, and in their type for a
    /// batch, a text value its bytes and any other its width. `NULL` weighs nothing.
    pub(crate) fn bytes(&self) -> u64 {
        match self {
            Self::Rows(page) => page
                .iter()
                .flat_map(|row| row.iter().flatten())
                .map(|value| value.len() as u64)
                .sum(),
            Self::Batch(batch) => batch
                .columns()
                .iter()
                .map(|array| array_bytes(array.as_ref()))
                .sum(),
        }
    }

    /// The bytes of each of its `columns` columns' values, weighed as [`SourcePart::bytes`]
    /// weighs them.
    pub(crate) fn column_bytes(&self, columns: usize) -> Vec<u64> {
        match self {
            Self::Rows(page) => page_column_bytes(page, columns),
            Self::Batch(batch) => batch
                .columns()
                .iter()
                .map(|array| array_bytes(array.as_ref()))
                .collect(),
        }
    }
}

fn page_column_bytes(page: &[Row], columns: usize) -> Vec<u64> {
    let mut bytes = vec![0; columns];
    for row in page {
        for (total, value) in bytes.iter_mut().zip(row) {
            *total += value.as_ref().map_or(0, |value| value.len() as u64);
        }
    }
    bytes
}

/// The bytes of `array`'s values in their type.
fn array_bytes(array: &dyn Array) -> u64 {
    let present = (array.len() - array.null_count()) as u64;
    match array.data_type() {
        DataType::Utf8 => string_bytes(array.as_string::<i32>()),
        DataType::LargeUtf8 => string_bytes(array.as_string::<i64>()),
        DataType::Boolean => present,
        data_type => match data_type.primitive_width() {
            Some(width) => present * width as u64,
            None => array.get_buffer_memory_size() as u64,
        },
    }
}

/// The bytes between a string array's first offset and its last: its values', a `NULL`
/// taking none.
fn string_bytes<O: OffsetSizeTrait>(array: &GenericStringArray<O>) -> u64 {
    let offsets = array.value_offsets();
    let first = offsets.first().map_or(0, |offset| offset.as_usize());
    let last = offsets.last().map_or(0, |offset| offset.as_usize());
    last.saturating_sub(first) as u64
}

/// The type a batch's column of `data_type` is loaded as, before its first row: its own, in
/// DuckDB's terms, for a bulk type; `None` for text, which its values type as a page's do.
/// A column of another type, which a driver shouldn't send, is loaded as text, which DuckDB
/// casts it to.
pub(crate) fn batch_column_type(data_type: &DataType) -> Option<DuckDbType> {
    let Some(kind) = bulk_kind(data_type) else {
        return Some(DuckDbType::Varchar);
    };
    match (kind, data_type) {
        (ValueKind::Text, _) => None,
        // An unsigned 64-bit value may need 20 digits, past what a `BIGINT` holds.
        (ValueKind::Integer, DataType::UInt64) => Some(DuckDbType::HugeInt),
        (ValueKind::Integer, _) => Some(DuckDbType::BigInt),
        (ValueKind::Float, _) => Some(DuckDbType::Double),
        (ValueKind::Decimal, DataType::Decimal128(precision, scale)) => {
            match u8::try_from(*scale) {
                Ok(scale) => Some(DuckDbType::Decimal {
                    precision: *precision,
                    scale,
                }),
                Err(_) => Some(DuckDbType::Varchar),
            }
        }
        (ValueKind::Boolean, _) => Some(DuckDbType::Boolean),
        (ValueKind::Date, _) => Some(DuckDbType::Date),
        (ValueKind::Timestamp, _) => Some(DuckDbType::Timestamp),
        (ValueKind::TimestampWithZone, _) => Some(DuckDbType::TimestampTz),
        (ValueKind::Decimal | ValueKind::Undeclared, _) => Some(DuckDbType::Varchar),
    }
}

/// Each text value of `array`, a column of text; nothing for a column of another type.
pub(crate) fn text_values(array: &dyn Array) -> Box<dyn Iterator<Item = Option<&str>> + '_> {
    match array.data_type() {
        DataType::Utf8 => Box::new(array.as_string::<i32>().iter()),
        DataType::LargeUtf8 => Box::new(array.as_string::<i64>().iter()),
        _ => Box::new(std::iter::empty()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{
        ArrayRef, BooleanArray, Decimal128Array, Int32Array, LargeStringArray, RecordBatch,
        StringArray,
    };
    use arrow_schema::{Field, TimeUnit};

    use super::*;

    #[test]
    fn loads_each_bulk_type_as_its_duckdb_type() {
        let list = DataType::List(Field::new("item", DataType::Int64, true).into());
        let cases = [
            (DataType::Int8, Some(DuckDbType::BigInt)),
            (DataType::Int64, Some(DuckDbType::BigInt)),
            (DataType::UInt32, Some(DuckDbType::BigInt)),
            (DataType::UInt64, Some(DuckDbType::HugeInt)),
            (DataType::Float32, Some(DuckDbType::Double)),
            (
                DataType::Decimal128(18, 2),
                Some(DuckDbType::Decimal {
                    precision: 18,
                    scale: 2,
                }),
            ),
            (DataType::Boolean, Some(DuckDbType::Boolean)),
            (DataType::Date32, Some(DuckDbType::Date)),
            (
                DataType::Timestamp(TimeUnit::Nanosecond, None),
                Some(DuckDbType::Timestamp),
            ),
            (
                DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                Some(DuckDbType::TimestampTz),
            ),
            (DataType::Utf8, None),
            (DataType::LargeUtf8, None),
            (list, Some(DuckDbType::Varchar)),
            (DataType::Binary, Some(DuckDbType::Varchar)),
        ];
        for (data_type, expected) in cases {
            assert_eq!(batch_column_type(&data_type), expected, "{data_type}");
        }
    }

    #[test]
    fn weighs_a_batch_by_its_values_in_their_types() {
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![Some(1), None, Some(3)])),
            Arc::new(StringArray::from(vec![Some("ab"), None, Some("cde")])),
            Arc::new(LargeStringArray::from(vec![Some("x"), Some(""), None])),
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
            Arc::new(
                Decimal128Array::from(vec![Some(1), Some(2), Some(3)])
                    .with_precision_and_scale(18, 2)
                    .unwrap(),
            ),
        ];
        let batch =
            RecordBatch::try_from_iter(["i", "s", "l", "b", "d"].into_iter().zip(arrays)).unwrap();
        assert_eq!(
            SourcePart::Batch(batch.slice(1, 2)).column_bytes(5),
            vec![4, 3, 0, 1, 32]
        );
        assert_eq!(
            SourcePart::Batch(batch).column_bytes(5),
            vec![8, 5, 1, 2, 48]
        );
    }

    #[test]
    fn weighs_a_page_by_its_text() {
        let page = vec![
            vec![Some("12".to_owned()), None],
            vec![Some("3".to_owned()), Some("abc".to_owned())],
        ];
        assert_eq!(SourcePart::Rows(page).column_bytes(2), vec![3, 3]);
    }
}
