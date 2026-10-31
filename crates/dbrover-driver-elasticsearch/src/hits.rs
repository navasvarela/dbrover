//! A search's hits as documents, read a page at a time through a point in time and
//! `search_after`, which both servers have (`docs/nosql.md`, N3's decisions).
//!
//! A search DbRover pages is sent again for each page with the point in time, the page's
//! `size`, the sort with `_shard_doc` after it as the tiebreaker, and the last hit's sort
//! values as `search_after`; the rest of its body is sent as typed.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{ColumnInfo, DriverError, ResultCursor, ResultShape, Row};
use dbrover_document::Value;
use dbrover_language::search::{self, Body, Request};

use crate::Flavor;
use crate::answer::{DOCUMENT_COLUMN, columns};
use crate::errors::{self, Refused};
use crate::http::{Answer, Call, Client};
use crate::json::{at, field, object, set, string, text_of};
use crate::running::Running;

/// How long the server keeps a point in time between two pages. A grid may sit for longer
/// than this before its next page; the point in time is then opened again (see
/// [`HitCursor::fetch`]).
const KEEP_ALIVE: &str = "5m";

/// How many hits the first request reads: the grid's first page, and one more, so the grid
/// knows whether there are others.
const FIRST_FETCH: usize = 501;

/// Fields of a hit left out of its document: its source becomes the document's own fields,
/// and its sort values are DbRover's paging.
const HIDDEN_HIT_FIELDS: &[&str] = &["_source", "sort"];

/// A search DbRover reads every hit of, a page at a time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Paging {
    /// The indices the point in time is opened on: the path's, or every index.
    indices: String,
    /// The query string's parameters each page keeps.
    parameters: String,
    /// The body's fields, without `size` and `sort`.
    body: Vec<(String, Value)>,
    sort: Vec<Value>,
    /// The search's own `size`: at most this many hits.
    limit: Option<u64>,
}

impl Paging {
    /// How `request` is paged, when it is a search DbRover pages: `GET` or `POST` to
    /// `_search`, with no parameter or body key that asks for something else.
    pub(crate) fn of(request: &Request) -> Option<Self> {
        if !search::pages_hits(request) {
            return None;
        }
        let indices = match request.segments().as_slice() {
            [indices, "_search"] => (*indices).to_owned(),
            _ => "*".to_owned(),
        };
        let mut limit = None;
        let mut kept = Vec::new();
        for (name, value) in request.parameters() {
            match (name, value) {
                ("size", Some(size)) => limit = size.parse::<u64>().ok(),
                (name, Some(value)) => kept.push(format!("{name}={value}")),
                (name, None) => kept.push(name.to_owned()),
            }
        }
        let mut body = match &request.body {
            Some(Body::Json {
                value: Value::Object(fields),
                ..
            }) => fields.clone(),
            _ => Vec::new(),
        };
        if let Some(Value::Number(size)) = take(&mut body, "size") {
            limit = size.parse::<u64>().ok();
        }
        let sort = sort_with_tiebreaker(take(&mut body, "sort"));
        if field(&Value::Object(body.clone()), "track_total_hits").is_none() {
            // Counting every match is what makes a large search slow, and the grid doesn't
            // show the count.
            body.push(("track_total_hits".to_owned(), Value::Bool(false)));
        }
        Some(Self {
            indices,
            parameters: kept.join("&"),
            body,
            sort,
            limit,
        })
    }

    /// The body of the page after `search_after`, `size` hits long, through `pit`.
    fn page(&self, pit: &str, size: usize, search_after: Option<&Value>) -> Value {
        let mut body = self.body.clone();
        set(
            &mut body,
            "pit",
            object([("id", string(pit)), ("keep_alive", string(KEEP_ALIVE))]),
        );
        set(&mut body, "size", Value::Number(size.to_string()));
        set(&mut body, "sort", Value::Array(self.sort.clone()));
        if let Some(after) = search_after {
            set(&mut body, "search_after", after.clone());
        }
        Value::Object(body)
    }

    fn target(&self) -> String {
        if self.parameters.is_empty() {
            "/_search".to_owned()
        } else {
            format!("/_search?{}", self.parameters)
        }
    }
}

/// Removes the field `name` from `fields`, returning its value.
fn take(fields: &mut Vec<(String, Value)>, name: &str) -> Option<Value> {
    let index = fields.iter().position(|(field, _)| field == name)?;
    Some(fields.remove(index).1)
}

/// The search's sort, as a list, with `_shard_doc` last unless it is there already: the
/// tiebreaker that makes each hit's sort values unique, so `search_after` neither skips nor
/// repeats hits that sort alike. A search without a sort sorts by score, as it would have.
fn sort_with_tiebreaker(sort: Option<Value>) -> Vec<Value> {
    let mut sort = match sort {
        None => vec![object([("_score", string("desc"))])],
        Some(Value::Array(items)) => items,
        Some(single) => vec![single],
    };
    let has_tiebreaker = sort.iter().any(|item| match item {
        Value::String(name) => name == "_shard_doc",
        Value::Object(fields) => fields.iter().any(|(name, _)| name == "_shard_doc"),
        _ => false,
    });
    if !has_tiebreaker {
        sort.push(object([("_shard_doc", string("asc"))]));
    }
    sort
}

/// A hit as a document: `_index`, `_id` and `_score`, then its source's fields, then whatever
/// else the hit holds (`fields`, `highlight`, …). Should the source have a field of the same
/// name as one of those, it stays whole under `_source`, so nothing is hidden.
pub(crate) fn hit_document(hit: &Value) -> Value {
    let Value::Object(fields) = hit else {
        return hit.clone();
    };
    let mut document: Vec<(String, Value)> = search::HIT_KEYS
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), field(hit, name)?.clone())))
        .collect();
    let rest: Vec<(String, Value)> = fields
        .iter()
        .filter(|(name, _)| {
            !search::HIT_KEYS.contains(&name.as_str())
                && !HIDDEN_HIT_FIELDS.contains(&name.as_str())
        })
        .cloned()
        .collect();
    match field(hit, "_source") {
        Some(Value::Object(source))
            if !source.iter().any(|(name, _)| {
                search::HIT_KEYS.contains(&name.as_str())
                    || rest.iter().any(|(other, _)| other == name)
            }) =>
        {
            document.extend(source.iter().cloned());
        }
        Some(source) => document.push(("_source".to_owned(), source.clone())),
        None => {}
    }
    document.extend(rest);
    Value::Object(document)
}

/// The hits of a search, read a page at a time.
pub(crate) struct HitCursor {
    columns: Vec<ColumnInfo>,
    client: Client,
    running: Arc<Running>,
    opaque_id: String,
    flavor: Flavor,
    paging: Paging,
    pit: String,
    /// The last hit's sort values, where the next page starts.
    search_after: Option<Value>,
    buffer: VecDeque<Row>,
    /// How many hits have been read, for the search's own `size`.
    read: u64,
    /// Whether the server has no more hits, or the search's `size` is reached.
    done: bool,
}

impl HitCursor {
    /// Opens a point in time on the search's indices and reads its first page.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] when the server refuses the search, pointing into its body
    /// where the server says.
    pub(crate) async fn start(
        client: Client,
        running: Arc<Running>,
        opaque_id: String,
        flavor: Flavor,
        paging: Paging,
        request: &Request,
    ) -> Result<Self, DriverError> {
        let pit = open_pit(&client, &running, &opaque_id, flavor, &paging.indices)
            .await
            .map_err(|refused| refused.into_error(request))?;
        let mut cursor = Self {
            columns: columns(&[DOCUMENT_COLUMN]),
            client,
            running,
            opaque_id,
            flavor,
            paging,
            pit,
            search_after: None,
            buffer: VecDeque::new(),
            read: 0,
            done: false,
        };
        if let Err(refused) = cursor.fetch(FIRST_FETCH).await {
            cursor.close().await;
            return Err(refused.into_error(request));
        }
        Ok(cursor)
    }

    /// Reads up to `wanted` more hits into the buffer. A point in time the server has let go,
    /// after a grid sat longer than [`KEEP_ALIVE`], is opened again and the page asked for
    /// again: it continues after the same sort values, on the indices as they are now.
    async fn fetch(&mut self, wanted: usize) -> Result<(), PageError> {
        let size = match self.paging.limit {
            Some(limit) => wanted.min(usize::try_from(limit - self.read).unwrap_or(usize::MAX)),
            None => wanted,
        };
        if size == 0 {
            self.done = true;
            return Ok(());
        }
        let answer = match self.search(size).await {
            Err(PageError::Refused(refused)) if refused.is_lost_context() => {
                self.pit = open_pit(
                    &self.client,
                    &self.running,
                    &self.opaque_id,
                    self.flavor,
                    &self.paging.indices,
                )
                .await?;
                self.search(size).await?
            }
            other => other?,
        };
        if let Some(pit) = text_of(&answer, "pit_id") {
            self.pit = pit;
        }
        let hits = match at(&answer, &["hits", "hits"]) {
            Some(Value::Array(hits)) => hits.as_slice(),
            _ => &[],
        };
        if let Some(last) = hits.last() {
            self.search_after = field(last, "sort").cloned();
        }
        self.buffer.extend(
            hits.iter()
                .map(|hit| vec![Some(hit_document(hit).to_json())]),
        );
        self.read += hits.len() as u64;
        let reached_limit = self.paging.limit.is_some_and(|limit| self.read >= limit);
        self.done = hits.len() < size || reached_limit || self.search_after.is_none();
        Ok(())
    }

    async fn search(&self, size: usize) -> Result<Value, PageError> {
        let body = self
            .paging
            .page(&self.pit, size, self.search_after.as_ref());
        let target = self.paging.target();
        let call = Call::json(hyper::Method::POST, &target, &body).tagged(&self.opaque_id);
        let answer = self.running.send(&self.client, call).await?;
        json_answer(&answer)
    }

    /// Lets the server go of the point in time. Best effort: it expires by itself anyway.
    async fn close(&self) {
        let (target, body) = match self.flavor {
            Flavor::Elasticsearch => ("/_pit", object([("id", string(&self.pit))])),
            Flavor::OpenSearch => (
                "/_search/point_in_time",
                object([("pit_id", Value::Array(vec![string(&self.pit)]))]),
            ),
        };
        let call = Call::json(hyper::Method::DELETE, target, &body);
        if let Err(error) = self.client.send(call).await {
            tracing::debug!("could not close a point in time: {error}");
        }
    }
}

/// Why a page couldn't be read: the server refused it, or the request failed.
pub(crate) enum PageError {
    Refused(Refused),
    Failed(DriverError),
}

impl From<DriverError> for PageError {
    fn from(error: DriverError) -> Self {
        Self::Failed(error)
    }
}

impl PageError {
    /// The error a run shows, pointing into `request`'s body where the server says.
    fn into_error(self, request: &Request) -> DriverError {
        match self {
            Self::Refused(refused) => {
                let position = refused.position_in(request);
                errors::statement(refused, position)
            }
            Self::Failed(error) => error,
        }
    }
}

impl Refused {
    /// Whether the server no longer has the point in time a page asked for.
    fn is_lost_context(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind.contains("search_context_missing"))
            || self.message.contains("search_context_missing")
    }
}

fn json_answer(answer: &Answer) -> Result<Value, PageError> {
    if !answer.status.is_success() {
        return Err(PageError::Refused(Refused::of(answer)));
    }
    answer
        .json()
        .map_err(|error| PageError::Failed(errors::statement(error, None)))
}

async fn open_pit(
    client: &Client,
    running: &Running,
    opaque_id: &str,
    flavor: Flavor,
    indices: &str,
) -> Result<String, PageError> {
    let (target, id_field) = match flavor {
        Flavor::Elasticsearch => (format!("/{indices}/_pit?keep_alive={KEEP_ALIVE}"), "id"),
        Flavor::OpenSearch => (
            format!("/{indices}/_search/point_in_time?keep_alive={KEEP_ALIVE}"),
            "pit_id",
        ),
    };
    let call = Call {
        method: hyper::Method::POST,
        target: &target,
        body: None,
        opaque_id: Some(opaque_id),
    };
    let answer = json_answer(&running.send(client, call).await?)?;
    text_of(&answer, id_field).ok_or_else(|| {
        PageError::Failed(errors::statement(
            "the server opened no point in time to page the search through",
            None,
        ))
    })
}

#[async_trait]
impl ResultCursor for HitCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn shape(&self) -> ResultShape {
        ResultShape::Documents
    }

    /// Hands out up to `max_rows` hits, reading as many more as needed and one past them, so
    /// [`Self::is_exhausted`] is exact.
    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        while self.buffer.len() <= max_rows && !self.done {
            let wanted = max_rows + 1 - self.buffer.len();
            self.fetch(wanted).await.map_err(|error| match error {
                PageError::Refused(refused) => errors::statement(refused, None),
                PageError::Failed(error) => error,
            })?;
        }
        let count = max_rows.min(self.buffer.len());
        Ok(self.buffer.drain(..count).collect())
    }

    fn is_exhausted(&self) -> bool {
        self.done && self.buffer.is_empty()
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        self.close().await;
        Ok(None)
    }
}
