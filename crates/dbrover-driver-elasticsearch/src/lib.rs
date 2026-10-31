//! Elasticsearch and OpenSearch driver for DbRover, over their REST APIs (`docs/nosql.md`,
//! N3).
//!
//! A console's requests are read by `dbrover_language::search` and sent as typed, each tagged
//! with its run's `X-Opaque-Id`. What an answer shows depends on the request: a search's hits
//! are documents, read a page at a time through a point in time ([`hits`]); the SQL
//! endpoints' answers are rows ([`sql`]); any other answer is one document, or a line of text a
//! row. HTTP is stateless and neither server has transactions, so a connection is a client and
//! never inside one. The two servers differ, for what DbRover sends, only in the
//! point-in-time endpoints, so a connection keeps which one it reached ([`Flavor`]).

mod answer;
mod catalog;
mod errors;
mod hits;
mod http;
mod json;
mod running;
mod sql;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, Connection, DriverError, ElasticsearchConfig, Password,
    Rejection, RelationName, ResultCursor, Route, ServerInfo, TableConstraints, TableDescription,
    TableName, TransactionStatus,
};
use dbrover_document::Value;
use dbrover_language::search::{self, Method, Request};

use crate::answer::Table;
use crate::errors::Refused;
use crate::http::{Call, Client};
use crate::json::{at, object, text_of};
use crate::running::{Running, SearchCancel};

/// Which of the two servers a connection reached, from its answer to `GET /`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Elasticsearch,
    OpenSearch,
}

impl Flavor {
    fn product(self) -> &'static str {
        match self {
            Self::Elasticsearch => "Elasticsearch",
            Self::OpenSearch => "OpenSearch",
        }
    }
}

/// Connects to the node in `config`, through `route`, and asks it what it is.
///
/// # Errors
///
/// [`DriverError::Connect`] if the TLS settings can't be used, the node can't be reached, the
/// TLS handshake or certificate check fails, or the node refuses the credentials.
pub async fn connect(
    config: &ElasticsearchConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<SearchConnection, DriverError> {
    let address = match route {
        Route::Direct => format!("{}:{}", config.server.host, config.server.port_number()?),
        Route::Tunnel(local) => local.to_string(),
    };
    let client = Client::new(config, password, &address).map_err(errors::connect)?;
    let answer = client.send(Call::get("/")).await.map_err(errors::connect)?;
    if !answer.status.is_success() {
        return Err(errors::connect(Refused::of(&answer)));
    }
    let root = answer.json().map_err(errors::connect)?;
    let flavor = match at(&root, &["version", "distribution"]) {
        Some(Value::String(distribution)) if distribution == "opensearch" => Flavor::OpenSearch,
        _ => Flavor::Elasticsearch,
    };
    let version = at(&root, &["version"])
        .and_then(|version| text_of(version, "number"))
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(SearchConnection {
        client,
        flavor,
        server_info: ServerInfo {
            product: flavor.product().to_owned(),
            version,
        },
        running: Arc::new(Running::default()),
    })
}

/// A console's or the explorer's connection to an Elasticsearch or OpenSearch cluster.
pub struct SearchConnection {
    client: Client,
    flavor: Flavor,
    server_info: ServerInfo,
    running: Arc<Running>,
}

impl SearchConnection {
    /// Sends `request` as typed and shows its whole answer.
    async fn send_as_typed(
        &self,
        request: &Request,
        opaque_id: &str,
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        let target = with_json_format(request);
        let call = Call {
            method: http_method(request.method),
            target: &target,
            body: request
                .body
                .as_ref()
                .map(|body| (body.text(), body.content_type())),
            opaque_id: Some(opaque_id),
        };
        let answer = self.running.send(&self.client, call).await?;
        if request.method == Method::Head {
            // A HEAD answer is its status alone: 404 says the index or document isn't there.
            let status = Value::Number(answer.status.as_u16().to_string());
            return Ok(Box::new(Table::documents(vec![object([(
                "status", status,
            )])])));
        }
        if !answer.status.is_success() {
            let refused = Refused::of(&answer);
            let position = refused.position_in(request);
            return Err(errors::statement(refused, position));
        }
        Ok(Box::new(Table::of(&answer)))
    }
}

/// The request's target, with `format=json` added to a `_cat` request that doesn't name a
/// format, so its answer is documents rather than a text table.
fn with_json_format(request: &Request) -> String {
    let is_cat = request.segments().first() == Some(&"_cat");
    match (is_cat, request.parameter("format")) {
        (true, None) if request.query.is_empty() => format!("{}?format=json", request.path),
        (true, None) => format!("{}&format=json", request.target()),
        _ => request.target(),
    }
}

fn http_method(method: Method) -> hyper::Method {
    match method {
        Method::Get => hyper::Method::GET,
        Method::Post => hyper::Method::POST,
        Method::Put => hyper::Method::PUT,
        Method::Delete => hyper::Method::DELETE,
        Method::Head => hyper::Method::HEAD,
    }
}

#[async_trait]
impl Connection for SearchConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        catalog::get(&self.client, "/").await.map(drop)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&self.client, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        Err(DriverError::InvalidPath(path.clone()))
    }

    async fn execute(&mut self, text: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let request = search::parse(text).map_err(errors::parse)?;
        let opaque_id = self.running.start();
        if let Some(paging) = hits::Paging::of(&request) {
            let cursor = hits::HitCursor::start(
                self.client.clone(),
                Arc::clone(&self.running),
                opaque_id,
                self.flavor,
                paging,
                &request,
            )
            .await?;
            return Ok(Box::new(cursor));
        }
        if let Some(endpoint) = sql::Endpoint::of(&request) {
            let cursor = sql::SqlCursor::start(
                self.client.clone(),
                Arc::clone(&self.running),
                opaque_id,
                endpoint,
                &request,
            )
            .await?;
            return Ok(Box::new(cursor));
        }
        self.send_as_typed(&request, &opaque_id).await
    }

    async fn describe_table(
        &mut self,
        _table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        Ok(None)
    }

    async fn describe_constraints(
        &mut self,
        _table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        Ok(TableConstraints::default())
    }

    /// Reads the request only: asking the server comes with N3·5.
    async fn validate(&mut self, text: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(search::parse(text).err().map(|error| Rejection {
            position: Some(error.range.start),
            message: error.message,
            kind: dbrover_core::RejectionKind::Syntax,
        }))
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(SearchCancel {
            client: self.client.clone(),
            running: Arc::clone(&self.running),
        })
    }

    /// Nothing to close: HTTP keeps no session, and the pool's connections close with it.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}
