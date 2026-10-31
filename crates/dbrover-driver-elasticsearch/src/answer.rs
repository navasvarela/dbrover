//! An answer read whole, as the grid shows it: documents for JSON, a row a line for text.

use std::collections::VecDeque;

use async_trait::async_trait;
use dbrover_core::{ColumnInfo, DriverError, ResultCursor, ResultShape, Row};
use dbrover_document::Value;

use crate::http::Answer;
use crate::json::object;

/// The one column a result of documents reports (ADR 0006).
pub(crate) const DOCUMENT_COLUMN: &str = "document";

/// The column of an answer that isn't JSON, a line of its text a row.
const TEXT_COLUMN: &str = "answer";

pub(crate) fn columns(names: &[&str]) -> Vec<ColumnInfo> {
    names
        .iter()
        .map(|name| ColumnInfo {
            name: (*name).to_owned(),
            type_name: None,
        })
        .collect()
}

/// Rows already read, handed out a page at a time.
pub(crate) struct Table {
    columns: Vec<ColumnInfo>,
    rows: VecDeque<Row>,
    shape: ResultShape,
}

impl Table {
    /// What an answer shows: an array of objects as documents (a `_cat` answer, a line each),
    /// any other JSON as one document, and text a line a row.
    pub(crate) fn of(answer: &Answer) -> Self {
        let json = answer.is_json().then(|| answer.json().ok()).flatten();
        match json {
            Some(Value::Array(items))
                if items.iter().all(|item| matches!(item, Value::Object(_))) =>
            {
                Self::documents(items)
            }
            Some(document @ Value::Object(_)) => Self::documents(vec![document]),
            // A document is an object; any other answer is one field of one.
            Some(other) => Self::documents(vec![object([("answer", other)])]),
            None => Self::text(&answer.text()),
        }
    }

    pub(crate) fn documents(documents: Vec<Value>) -> Self {
        Self {
            columns: columns(&[DOCUMENT_COLUMN]),
            rows: documents
                .iter()
                .map(|document| vec![Some(document.to_json())])
                .collect(),
            shape: ResultShape::Documents,
        }
    }

    fn text(text: &str) -> Self {
        Self {
            columns: columns(&[TEXT_COLUMN]),
            rows: text
                .lines()
                .map(|line| vec![Some(line.to_owned())])
                .collect(),
            shape: ResultShape::Rows,
        }
    }
}

#[async_trait]
impl ResultCursor for Table {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn shape(&self) -> ResultShape {
        self.shape
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let count = max_rows.min(self.rows.len());
        Ok(self.rows.drain(..count).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.rows.is_empty()
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}
