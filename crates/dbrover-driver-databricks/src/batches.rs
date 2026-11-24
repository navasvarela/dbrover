//! A statement's rows as Arrow batches (ADR 0007), from Databricks's `ARROW_STREAM` result
//! through its external links: C6's columnar path, and its handoff (`docs/cloud-data.md`, C6,
//! "Rows and columnar batches in C6").
//!
//! 1. The statement is submitted with `ARROW_STREAM` and polled to its end as any other.
//! 2. The batches' schema comes from the manifest's types, so it is known before the first
//!    chunk, even for a result with none: each column of a bulk type keeps it, every other
//!    (`ARRAY`, `MAP`, `STRUCT`, `BINARY`, intervals, `VARIANT`) is text.
//! 3. Chunks are fetched a few ahead, in parallel, each link's Arrow IPC stream read whole
//!    (the documented API sends it uncompressed), and each batch made to the schema: a column
//!    of a bulk type cast where Arrow's type differs only in its zone's name, any other written
//!    as Arrow formats it.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_ipc::reader::StreamReader;
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use async_trait::async_trait;
use dbrover_core::{
    BatchCursor, Cell, ColumnInfo, DriverError, JobId, Jobs, ResultCursor, Row, bulk_kind,
};
use tokio::task::JoinHandle;

use crate::api::ApiError;
use crate::errors;
use crate::results::{Chunk, Column, Manifest};
use crate::statements::{DatabricksJobs, Format};

/// How many chunks are fetched at once, ahead of the one being read: enough to keep DuckDB's
/// appender fed while the next downloads, few enough that a large result doesn't hold many
/// chunks in memory at once (a chunk is tens of megabytes at most).
const CHUNKS_AHEAD: usize = 3;

/// The zone Arrow timestamps of Databricks's `TIMESTAMP` are read in: an instant, which the
/// batch carries in UTC whatever the session's zone.
const UTC: &str = "UTC";

/// Runs `sql` for a caller that reads its rows as batches: see the module.
pub(crate) async fn execute(
    statements: &Arc<DatabricksJobs>,
    jobs: &Arc<Jobs>,
    sql: &str,
) -> Result<Box<dyn ResultCursor>, DriverError> {
    let job = statements.submit_as(sql, Format::ArrowStream).await?;
    jobs.wait_for(&job).await?;
    let (manifest, first) = statements.take_result(&job).ok_or_else(|| {
        errors::connection(ApiError::Unreadable(
            "a finished statement's result wasn't kept".to_owned(),
        ))
    })?;
    Ok(Box::new(ArrowCursor::new(
        Arc::clone(statements),
        job,
        &manifest,
        first,
    )))
}

/// The Arrow type a column of `column`'s type is carried as: its own where it is a bulk type,
/// else text.
fn arrow_type(column: &Column) -> DataType {
    let text = column
        .type_text
        .as_deref()
        .unwrap_or_default()
        .to_uppercase();
    let name = column
        .type_name
        .as_deref()
        .unwrap_or_default()
        .to_uppercase();
    let base = if text.is_empty() { name } else { text };
    let base = base.split('(').next().unwrap_or_default().trim().to_owned();
    match base.as_str() {
        "BOOLEAN" => DataType::Boolean,
        "TINYINT" | "BYTE" => DataType::Int8,
        "SMALLINT" | "SHORT" => DataType::Int16,
        "INT" | "INTEGER" => DataType::Int32,
        "BIGINT" | "LONG" => DataType::Int64,
        "FLOAT" | "REAL" => DataType::Float32,
        "DOUBLE" => DataType::Float64,
        "DATE" => DataType::Date32,
        "TIMESTAMP" | "TIMESTAMP_LTZ" => {
            DataType::Timestamp(TimeUnit::Microsecond, Some(UTC.into()))
        }
        "TIMESTAMP_NTZ" => DataType::Timestamp(TimeUnit::Microsecond, None),
        "DECIMAL" | "DEC" | "NUMERIC" => {
            let precision = column.type_precision.unwrap_or(10);
            let scale = column.type_scale.unwrap_or(0);
            let decimal = DataType::Decimal128(precision, scale);
            if bulk_kind(&decimal).is_some() {
                decimal
            } else {
                DataType::Utf8
            }
        }
        _ => DataType::Utf8,
    }
}

/// The batches' schema, from the manifest's columns.
fn schema_of(columns: &[Column]) -> SchemaRef {
    Arc::new(Schema::new(
        columns
            .iter()
            .map(|column| Field::new(&column.name, arrow_type(column), true))
            .collect::<Vec<_>>(),
    ))
}

/// `batch` made to `schema`: each column of a bulk type as it is, or cast where only the
/// zone's name differs; any other as the text Arrow formats it as.
fn conform(batch: &RecordBatch, schema: &SchemaRef) -> Result<RecordBatch, DriverError> {
    let unreadable = |what: String| errors::connection(ApiError::Unreadable(what));
    if batch.num_columns() != schema.fields().len() {
        return Err(unreadable(format!(
            "a chunk has {} columns, and the result {}",
            batch.num_columns(),
            schema.fields().len()
        )));
    }
    let columns = schema
        .fields()
        .iter()
        .zip(batch.columns())
        .map(|(field, array)| {
            let wanted = field.data_type();
            if array.data_type() == wanted {
                Ok(Arc::clone(array))
            } else if *wanted == DataType::Utf8 {
                as_text(array)
            } else {
                arrow_cast::cast(array, wanted).map_err(|error| {
                    unreadable(format!("column {} as {wanted}: {error}", field.name()))
                })
            }
        })
        .collect::<Result<Vec<ArrayRef>, DriverError>>()?;
    RecordBatch::try_new(Arc::clone(schema), columns)
        .map_err(|error| unreadable(format!("a chunk's batch: {error}")))
}

/// `array`'s values as the text Arrow formats them as, `NULL` kept.
fn as_text(array: &ArrayRef) -> Result<ArrayRef, DriverError> {
    let options = FormatOptions::new();
    let formatter = ArrayFormatter::try_new(array.as_ref(), &options)
        .map_err(|error| errors::connection(ApiError::Unreadable(error.to_string())))?;
    let values: StringArray = (0..array.len())
        .map(|row| (!array.is_null(row)).then(|| formatter.value(row).to_string()))
        .collect();
    Ok(Arc::new(values))
}

/// The batches of chunk `index`, made to `schema`.
async fn chunk_batches(
    statements: Arc<DatabricksJobs>,
    job: JobId,
    index: u64,
    known: Option<Chunk>,
    schema: SchemaRef,
) -> Result<Vec<RecordBatch>, DriverError> {
    let (downloads, _) = statements.chunk_bytes(&job, index, known).await?;
    let mut batches = Vec::new();
    for bytes in downloads {
        let reader = StreamReader::try_new(Cursor::new(bytes), None).map_err(|error| {
            errors::connection(ApiError::Unreadable(format!(
                "chunk {index}'s Arrow: {error}"
            )))
        })?;
        for batch in reader {
            let batch = batch.map_err(|error| {
                errors::connection(ApiError::Unreadable(format!(
                    "chunk {index}'s Arrow: {error}"
                )))
            })?;
            batches.push(conform(&batch, &schema)?);
        }
    }
    Ok(batches)
}

struct ArrowCursor {
    statements: Arc<DatabricksJobs>,
    job: JobId,
    columns: Vec<ColumnInfo>,
    schema: SchemaRef,
    /// Chunk 0's links, from the statement's answer, until its fetch starts.
    first: Option<Chunk>,
    total_chunks: u64,
    /// The next chunk whose fetch isn't started yet.
    next_chunk: u64,
    /// Chunks being fetched, in their order.
    fetching: VecDeque<JoinHandle<Result<Vec<RecordBatch>, DriverError>>>,
    /// Batches of a fetched chunk not handed out yet, and the next row of the first.
    pending: VecDeque<RecordBatch>,
    offset: usize,
}

impl ArrowCursor {
    fn new(statements: Arc<DatabricksJobs>, job: JobId, manifest: &Manifest, first: Chunk) -> Self {
        let mut cursor = Self {
            statements,
            job,
            columns: manifest.schema.columns.iter().map(Column::info).collect(),
            schema: schema_of(&manifest.schema.columns),
            first: Some(first),
            total_chunks: manifest.total_chunk_count,
            next_chunk: 0,
            fetching: VecDeque::new(),
            pending: VecDeque::new(),
            offset: 0,
        };
        cursor.fetch_ahead();
        cursor
    }

    /// Starts fetching chunks until [`CHUNKS_AHEAD`] are on their way.
    fn fetch_ahead(&mut self) {
        while self.fetching.len() < CHUNKS_AHEAD && self.next_chunk < self.total_chunks {
            let index = self.next_chunk;
            let known = if index == 0 { self.first.take() } else { None };
            self.fetching.push_back(tokio::spawn(chunk_batches(
                Arc::clone(&self.statements),
                self.job.clone(),
                index,
                known,
                Arc::clone(&self.schema),
            )));
            self.next_chunk += 1;
        }
    }

    /// Up to `max_rows` rows of the batch being handed out, or of the next one.
    async fn next_rows(&mut self, max_rows: usize) -> Result<Option<RecordBatch>, DriverError> {
        loop {
            if let Some(batch) = self.pending.front() {
                if self.offset < batch.num_rows() {
                    let rows = max_rows.max(1).min(batch.num_rows() - self.offset);
                    let slice = batch.slice(self.offset, rows);
                    self.offset += rows;
                    return Ok(Some(slice));
                }
                self.pending.pop_front();
                self.offset = 0;
                continue;
            }
            let Some(fetch) = self.fetching.pop_front() else {
                return Ok(None);
            };
            let batches = fetch.await.map_err(|error| {
                errors::connection(ApiError::Unreadable(format!("a chunk's fetch: {error}")))
            })??;
            self.pending.extend(batches);
            self.fetch_ahead();
        }
    }
}

impl Drop for ArrowCursor {
    /// Chunks still on their way aren't wanted once the cursor is gone.
    fn drop(&mut self) {
        for fetch in &self.fetching {
            fetch.abort();
        }
    }
}

#[async_trait]
impl ResultCursor for ArrowCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    /// Rows as text, from the batches, each value as Arrow formats it: a caller wanting
    /// Databricks's own text starts its statement with `execute` instead.
    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let mut rows = Vec::new();
        while rows.len() < max_rows {
            let Some(batch) = self.next_rows(max_rows - rows.len()).await? else {
                break;
            };
            rows.extend(text_rows(&batch)?);
        }
        Ok(rows)
    }

    fn is_exhausted(&self) -> bool {
        self.fetching.is_empty()
            && self.next_chunk >= self.total_chunks
            && match self.pending.len() {
                0 => true,
                1 => self
                    .pending
                    .front()
                    .is_some_and(|batch| self.offset >= batch.num_rows()),
                _ => false,
            }
    }

    fn batches(&mut self) -> Option<&mut dyn BatchCursor> {
        Some(self)
    }

    /// Nothing to stop: the statement had finished before its first chunk was read, and its
    /// result is Databricks's to drop.
    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}

#[async_trait]
impl BatchCursor for ArrowCursor {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    async fn next_batch(&mut self, max_rows: usize) -> Result<Option<RecordBatch>, DriverError> {
        self.next_rows(max_rows).await
    }
}

/// `batch`'s rows, each value as Arrow formats it.
fn text_rows(batch: &RecordBatch) -> Result<Vec<Row>, DriverError> {
    let options = FormatOptions::new();
    let formatters = batch
        .columns()
        .iter()
        .map(|array| ArrayFormatter::try_new(array.as_ref(), &options))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| errors::connection(ApiError::Unreadable(error.to_string())))?;
    Ok((0..batch.num_rows())
        .map(|row| {
            batch
                .columns()
                .iter()
                .zip(&formatters)
                .map(|(array, formatter)| -> Cell {
                    (!array.is_null(row)).then(|| formatter.value(row).to_string())
                })
                .collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(type_text: &str, precision: Option<u8>, scale: Option<i8>) -> Column {
        serde_json::from_value(serde_json::json!({
            "name": "c",
            "type_text": type_text,
            "type_precision": precision,
            "type_scale": scale,
        }))
        .expect("a column")
    }

    #[test]
    fn bulk_types_keep_their_own_and_the_rest_are_text() {
        let utc = DataType::Timestamp(TimeUnit::Microsecond, Some(UTC.into()));
        let cases = [
            (column("BIGINT", None, None), DataType::Int64),
            (column("int", None, None), DataType::Int32),
            (column("BOOLEAN", None, None), DataType::Boolean),
            (column("DOUBLE", None, None), DataType::Float64),
            (column("DATE", None, None), DataType::Date32),
            (column("TIMESTAMP", None, None), utc),
            (
                column("TIMESTAMP_NTZ", None, None),
                DataType::Timestamp(TimeUnit::Microsecond, None),
            ),
            (
                column("DECIMAL(38,2)", Some(38), Some(2)),
                DataType::Decimal128(38, 2),
            ),
            (column("STRING", None, None), DataType::Utf8),
            (column("ARRAY<STRING>", None, None), DataType::Utf8),
            (column("MAP<STRING,INT>", None, None), DataType::Utf8),
            (column("BINARY", None, None), DataType::Utf8),
            (column("VARIANT", None, None), DataType::Utf8),
            (column("INTERVAL DAY", None, None), DataType::Utf8),
        ];
        for (column, wanted) in cases {
            assert_eq!(arrow_type(&column), wanted, "{:?}", column.type_text);
        }
    }
}
