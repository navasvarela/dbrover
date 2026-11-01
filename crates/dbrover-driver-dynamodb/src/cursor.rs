//! A statement's items, read through `ExecuteStatement` a page at a time with its
//! `NextToken`, as the grid asks for them.
//!
//! What a page reads is bounded whatever the statement (`docs/nosql.md`, N5): see
//! [`PageBound`]. A filtered Scan shows a short page with **Load more**, rather than reading on
//! unseen.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use dbrover_core::{ColumnInfo, DriverError, ResultCursor, ResultShape, Row};

use crate::items;
use crate::running::Running;

/// The most one page of a result may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageBound {
    /// The `Limit` of each request: the items DynamoDB evaluates before answering, matched or
    /// not.
    pub(crate) items_per_request: i32,
    /// The requests one page may make, even when a filter kept fewer items than asked for.
    pub(crate) requests_per_page: usize,
}

impl PageBound {
    /// 500 items a request, the engine's page size, so a page of a statement without a filter
    /// is one request; and ten requests a page, at most 5,000 items or 10 MB read before the
    /// page is shown: enough for a selective filter to find something, little enough to cost
    /// cents on the largest table.
    pub(crate) const DEFAULT: Self = Self {
        items_per_request: 500,
        requests_per_page: 10,
    };
}

/// The one column a result of documents reports (ADR 0006).
const DOCUMENT_COLUMN: &str = "item";

/// The items of one statement.
pub(crate) struct ItemCursor {
    client: Client,
    running: Arc<Running>,
    bound: PageBound,
    statement: String,
    /// The attributes each document starts with: the keys of the table read.
    keys: Vec<String>,
    columns: Vec<ColumnInfo>,
    read: VecDeque<Row>,
    /// Where the next request starts, until the last one has been answered.
    next_token: Option<String>,
    /// The requests made for the page being read, the statement's first one included.
    page_requests: usize,
}

impl ItemCursor {
    /// Sends `statement`'s first request.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] with DynamoDB's message when it refuses the statement, or
    /// when the run is cancelled.
    pub(crate) async fn start(
        client: Client,
        running: Arc<Running>,
        bound: PageBound,
        statement: &str,
        keys: Vec<String>,
        returns_items: bool,
    ) -> Result<Self, DriverError> {
        let mut cursor = Self {
            client,
            running,
            bound,
            statement: statement.to_owned(),
            keys,
            columns: Vec::new(),
            read: VecDeque::new(),
            next_token: None,
            page_requests: 0,
        };
        let generation = cursor.running.generation();
        cursor.request(generation, None).await?;
        // A write answers with items only when it says `RETURNING`; without them it is a
        // statement with no result, as an `UPDATE` is on any engine.
        if returns_items || !cursor.read.is_empty() {
            cursor.columns = vec![ColumnInfo {
                name: DOCUMENT_COLUMN.to_owned(),
                type_name: None,
            }];
        }
        Ok(cursor)
    }

    async fn request(&mut self, generation: u64, token: Option<String>) -> Result<(), DriverError> {
        let request = self
            .client
            .execute_statement()
            .statement(&self.statement)
            .limit(self.bound.items_per_request)
            .set_next_token(token)
            .send();
        let answer = self.running.send(generation, request).await?;
        self.read.extend(
            answer
                .items()
                .iter()
                .map(|item| vec![Some(items::document(item, &self.keys).to_json())]),
        );
        self.next_token = answer.next_token().map(str::to_owned);
        self.page_requests += 1;
        Ok(())
    }
}

#[async_trait]
impl ResultCursor for ItemCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn shape(&self) -> ResultShape {
        ResultShape::Documents
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let generation = self.running.generation();
        while self.read.len() < max_rows && self.page_requests < self.bound.requests_per_page {
            let Some(token) = self.next_token.take() else {
                break;
            };
            self.request(generation, Some(token)).await?;
        }
        self.page_requests = 0;
        let count = max_rows.min(self.read.len());
        Ok(self.read.drain(..count).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.read.is_empty() && self.next_token.is_none()
    }

    /// Nothing to close: DynamoDB keeps no cursor, and a token not followed costs nothing.
    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}
