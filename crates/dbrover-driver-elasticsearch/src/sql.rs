//! The SQL endpoints' answers as rows: Elasticsearch's `_sql`, in its JSON format, and
//! OpenSearch's SQL and PPL plugins, in theirs. Each answers with its columns' names and types
//! and a first page of rows, and a cursor for the next.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{ColumnInfo, DriverError, ResultCursor, Row};
use dbrover_document::Value;
use dbrover_language::search::{Body, Method, Request};

use crate::errors::{self, Refused};
use crate::http::{Answer, Call, Client};
use crate::json::{cell, field, object, set, string, text_of};
use crate::running::Running;

/// How many rows OpenSearch's SQL plugin answers a page, asked for when a statement doesn't
/// say: without it, the plugin answers at most its `plugins.query.size_limit` rows (200 by
/// default) and no cursor. The grid's page.
const OPENSEARCH_FETCH_SIZE: &str = "500";

/// Which SQL endpoint a request is for, which decides its answer's shape and its paging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// `_sql`, with `columns`, `rows` and `cursor`.
    Elasticsearch,
    /// `_plugins/_sql` (or `_opendistro/_sql`), with `schema`, `datarows` and `cursor`.
    OpenSearchSql,
    /// `_plugins/_ppl`, answered as the SQL plugin answers, without a cursor.
    OpenSearchPpl,
}

impl Endpoint {
    /// The endpoint `request` runs a statement on, in a format DbRover reads as rows; `None`
    /// for any other request, and for a statement asked for in another format (`txt`, `csv`),
    /// whose answer is shown as text.
    pub(crate) fn of(request: &Request) -> Option<Self> {
        if !matches!(request.method, Method::Get | Method::Post) {
            return None;
        }
        let endpoint = match request.segments().as_slice() {
            ["_sql"] => Self::Elasticsearch,
            ["_plugins" | "_opendistro", "_sql"] => Self::OpenSearchSql,
            ["_plugins" | "_opendistro", "_ppl"] => Self::OpenSearchPpl,
            _ => return None,
        };
        let readable = match (endpoint, request.parameter("format")) {
            (_, None) => true,
            (Self::Elasticsearch, Some(format)) => format == "json",
            (Self::OpenSearchSql | Self::OpenSearchPpl, Some(format)) => format == "jdbc",
        };
        readable.then_some(endpoint)
    }

    /// Where the statement goes, its answer in JSON.
    fn target(self, request: &Request) -> String {
        match (self, request.parameter("format")) {
            (Self::Elasticsearch, None) if request.query.is_empty() => {
                format!("{}?format=json", request.path)
            }
            (Self::Elasticsearch, None) => format!("{}&format=json", request.target()),
            _ => request.target(),
        }
    }

    /// The statement's body, with OpenSearch's SQL asked to page.
    fn body(self, request: &Request) -> Option<(String, &'static str)> {
        let body = request.body.as_ref()?;
        match (self, body) {
            (
                Self::OpenSearchSql,
                Body::Json {
                    value: Value::Object(fields),
                    ..
                },
            ) if !fields.iter().any(|(name, _)| name == "fetch_size") => {
                let mut fields = fields.clone();
                set(
                    &mut fields,
                    "fetch_size",
                    Value::Number(OPENSEARCH_FETCH_SIZE.to_owned()),
                );
                Some((Value::Object(fields).to_json(), body.content_type()))
            }
            _ => Some((body.text(), body.content_type())),
        }
    }

    /// The page after `cursor`'s, and where to let the cursor go.
    fn cursor_targets(self, request: &Request) -> (String, String) {
        match self {
            Self::Elasticsearch => ("/_sql?format=json".to_owned(), "/_sql/close".to_owned()),
            Self::OpenSearchSql | Self::OpenSearchPpl => {
                let path = request.path.trim_end_matches('/');
                (path.to_owned(), format!("{path}/close"))
            }
        }
    }

    fn rows_field(self) -> &'static str {
        match self {
            Self::Elasticsearch => "rows",
            Self::OpenSearchSql | Self::OpenSearchPpl => "datarows",
        }
    }

    fn columns_field(self) -> &'static str {
        match self {
            Self::Elasticsearch => "columns",
            Self::OpenSearchSql | Self::OpenSearchPpl => "schema",
        }
    }
}

/// The rows of a statement, read a page at a time with the answer's cursor.
pub(crate) struct SqlCursor {
    columns: Vec<ColumnInfo>,
    client: Client,
    running: Arc<Running>,
    opaque_id: String,
    endpoint: Endpoint,
    next: String,
    close: String,
    rows: VecDeque<Row>,
    cursor: Option<String>,
}

impl SqlCursor {
    /// Runs the statement in `request` and reads its first page.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] when the server refuses the statement.
    pub(crate) async fn start(
        client: Client,
        running: Arc<Running>,
        opaque_id: String,
        endpoint: Endpoint,
        request: &Request,
    ) -> Result<Self, DriverError> {
        let target = endpoint.target(request);
        let call = Call {
            method: hyper::Method::POST,
            target: &target,
            body: endpoint.body(request),
            opaque_id: Some(&opaque_id),
        };
        let answer = running.send(&client, call).await?;
        let page = page(&answer).map_err(|refused| {
            let position = refused.position_in(request);
            errors::statement(refused, position)
        })?;
        let columns = match field(&page, endpoint.columns_field()) {
            Some(Value::Array(columns)) => columns.iter().map(column).collect(),
            _ => Vec::new(),
        };
        let (next, close) = endpoint.cursor_targets(request);
        let mut cursor = Self {
            columns,
            client,
            running,
            opaque_id,
            endpoint,
            next,
            close,
            rows: VecDeque::new(),
            cursor: None,
        };
        cursor.take(&page);
        Ok(cursor)
    }

    fn take(&mut self, page: &Value) {
        if let Some(Value::Array(rows)) = field(page, self.endpoint.rows_field()) {
            self.rows.extend(rows.iter().map(|row| match row {
                Value::Array(values) => values.iter().map(cell).collect(),
                other => vec![cell(other)],
            }));
        }
        self.cursor = text_of(page, "cursor").filter(|cursor| !cursor.is_empty());
    }

    async fn fetch(&mut self) -> Result<(), DriverError> {
        let Some(cursor) = self.cursor.take() else {
            return Ok(());
        };
        let body = object([("cursor", string(cursor))]);
        let call = Call::json(hyper::Method::POST, &self.next, &body).tagged(&self.opaque_id);
        let answer = self.running.send(&self.client, call).await?;
        let page = page(&answer).map_err(|refused| errors::statement(refused, None))?;
        self.take(&page);
        Ok(())
    }
}

/// The page `answer` holds, or the server's refusal.
fn page(answer: &Answer) -> Result<Value, Refused> {
    if !answer.status.is_success() {
        return Err(Refused::of(answer));
    }
    answer.json().map_err(|error| Refused {
        message: format!("the answer isn't JSON: {error}"),
        status: answer.status.as_u16(),
        kind: None,
        line_column: None,
    })
}

/// A column of the answer, its name and its type as the endpoint names them.
fn column(column: &Value) -> ColumnInfo {
    ColumnInfo {
        name: text_of(column, "alias")
            .or_else(|| text_of(column, "name"))
            .unwrap_or_default(),
        type_name: text_of(column, "type"),
    }
}

#[async_trait]
impl ResultCursor for SqlCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        while self.rows.len() < max_rows && self.cursor.is_some() {
            self.fetch().await?;
        }
        let count = max_rows.min(self.rows.len());
        Ok(self.rows.drain(..count).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.rows.is_empty() && self.cursor.is_none()
    }

    /// Lets the server go of a cursor not read to its end. Best effort: it expires anyway.
    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        if let Some(cursor) = &self.cursor {
            let body = object([("cursor", string(cursor))]);
            let call = Call::json(hyper::Method::POST, &self.close, &body);
            if let Err(error) = self.client.send(call).await {
                tracing::debug!("could not close a SQL cursor: {error}");
            }
        }
        Ok(None)
    }
}
