//! A result's rows as Arrow record batches, for paths that move many rows into DuckDB without
//! formatting them as text (ADR 0007).

use arrow_array::RecordBatch;
use arrow_schema::{DataType, SchemaRef};
use async_trait::async_trait;

use crate::{DriverError, ValueKind};

/// The most digits of a decimal a batch carries as a decimal: DuckDB's `DECIMAL` and Arrow's
/// `Decimal128` both hold 38.
const MAX_BULK_DECIMAL_DIGITS: u8 = 38;

/// The rows of a running statement as Arrow batches, which a [`ResultCursor`] hands out
/// through [`ResultCursor::batches`] where its engine answers in Arrow.
///
/// Every column of [`BatchCursor::schema`] is of a bulk type ([`bulk_kind`] says which): a
/// driver sends a column of any other type as text, the text its `next_page` gives for it.
///
/// [`ResultCursor`]: crate::ResultCursor
/// [`ResultCursor::batches`]: crate::ResultCursor::batches
#[async_trait]
pub trait BatchCursor: Send {
    /// The batches' columns, known before the first batch.
    fn schema(&self) -> SchemaRef;

    /// The next batch, of at least one and at most `max_rows` rows, or `None` once every row
    /// has been read.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] if the statement fails while producing rows.
    async fn next_batch(&mut self, max_rows: usize) -> Result<Option<RecordBatch>, DriverError>;
}

/// What kind of value a column of `data_type` holds, if it is a bulk type: one a batch may
/// carry as it is, which DuckDB holds exactly. `None` for every other type, which a batch
/// carries as text.
///
/// [`ValueKind::Text`] is text as the engine formats it: a reader types it by its values, as
/// it would a page of text.
pub fn bulk_kind(data_type: &DataType) -> Option<ValueKind> {
    match data_type {
        DataType::Boolean => Some(ValueKind::Boolean),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => Some(ValueKind::Integer),
        DataType::Float32 | DataType::Float64 => Some(ValueKind::Float),
        DataType::Decimal128(precision, scale)
            if *precision <= MAX_BULK_DECIMAL_DIGITS && *scale >= 0 =>
        {
            Some(ValueKind::Decimal)
        }
        DataType::Date32 => Some(ValueKind::Date),
        DataType::Timestamp(_, None) => Some(ValueKind::Timestamp),
        DataType::Timestamp(_, Some(_)) => Some(ValueKind::TimestampWithZone),
        DataType::Utf8 | DataType::LargeUtf8 => Some(ValueKind::Text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use arrow_schema::{Field, Fields, IntervalUnit, TimeUnit};

    use super::*;

    #[test]
    fn says_which_types_a_batch_carries_as_they_are() {
        let list = DataType::List(Field::new("item", DataType::Int64, true).into());
        let cases = [
            (DataType::Boolean, Some(ValueKind::Boolean)),
            (DataType::Int8, Some(ValueKind::Integer)),
            (DataType::Int64, Some(ValueKind::Integer)),
            (DataType::UInt64, Some(ValueKind::Integer)),
            (DataType::Float32, Some(ValueKind::Float)),
            (DataType::Float64, Some(ValueKind::Float)),
            (DataType::Decimal128(18, 2), Some(ValueKind::Decimal)),
            (DataType::Decimal128(38, 0), Some(ValueKind::Decimal)),
            (DataType::Decimal128(10, -2), None),
            (DataType::Decimal256(50, 2), None),
            (DataType::Date32, Some(ValueKind::Date)),
            (DataType::Date64, None),
            (
                DataType::Timestamp(TimeUnit::Microsecond, None),
                Some(ValueKind::Timestamp),
            ),
            (
                DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                Some(ValueKind::TimestampWithZone),
            ),
            (DataType::Utf8, Some(ValueKind::Text)),
            (DataType::LargeUtf8, Some(ValueKind::Text)),
            (DataType::Binary, None),
            (DataType::FixedSizeBinary(16), None),
            (DataType::Time64(TimeUnit::Microsecond), None),
            (DataType::Interval(IntervalUnit::MonthDayNano), None),
            (list, None),
            (DataType::Struct(Fields::empty()), None),
            (
                DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
                None,
            ),
        ];
        for (data_type, kind) in cases {
            assert_eq!(bulk_kind(&data_type), kind, "{data_type}");
        }
    }
}
