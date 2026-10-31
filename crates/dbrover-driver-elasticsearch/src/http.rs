//! Requests to a node's REST API, over HTTP/1.1 with hyper, as the ClickHouse driver's client
//! sends them: plain, or TLS with `dbrover-tls`'s configuration.

use std::time::Duration;

use base64::Engine;
use dbrover_core::{BoxError, ElasticsearchAuth, ElasticsearchConfig, Password, TlsMode};
use dbrover_document::Value;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::ServerName;

/// How long a new TCP connection may take. Requests themselves have no limit: a search runs
/// as long as it needs, and ⌘. stops it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The header a request is tagged with, which the tasks API reports for the tasks it started.
pub(crate) const OPAQUE_ID: &str = "X-Opaque-Id";

/// A client for one node, with the data source's credentials. Cheap to clone: clones share
/// the connection pool.
#[derive(Clone)]
pub(crate) struct Client {
    http: HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>,
    /// `http://host:port` or `https://host:port`, the node's or an SSH tunnel's.
    base: String,
    authorization: Option<HeaderValue>,
}

/// One request: its method, its path with the query string, and its body with its type.
pub(crate) struct Call<'a> {
    pub method: Method,
    pub target: &'a str,
    pub body: Option<(String, &'static str)>,
    /// The run the request belongs to, for the tasks API.
    pub opaque_id: Option<&'a str>,
}

impl<'a> Call<'a> {
    pub(crate) fn get(target: &'a str) -> Self {
        Self {
            method: Method::GET,
            target,
            body: None,
            opaque_id: None,
        }
    }

    /// A request with `body` as its JSON.
    pub(crate) fn json(method: Method, target: &'a str, body: &Value) -> Self {
        Self {
            method,
            target,
            body: Some((body.to_json(), "application/json")),
            opaque_id: None,
        }
    }

    pub(crate) fn tagged(self, opaque_id: &'a str) -> Self {
        Self {
            opaque_id: Some(opaque_id),
            ..self
        }
    }
}

/// What the server answered: its status and its body, whole.
pub(crate) struct Answer {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Bytes,
}

impl Answer {
    pub(crate) fn is_json(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|content_type| content_type.starts_with("application/json"))
    }

    /// The body as JSON.
    ///
    /// # Errors
    ///
    /// When the body isn't UTF-8 JSON.
    pub(crate) fn json(&self) -> Result<Value, BoxError> {
        let text = std::str::from_utf8(&self.body)?;
        Ok(Value::parse(text)?)
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Client {
    /// A client for the node in `config`, reached at `address` (`host:port`, the node's own or
    /// an SSH tunnel's).
    ///
    /// HTTPS is configured on the node's one port in place of HTTP, not offered beside it, so
    /// `prefer` can't try TLS and fall back: it means plain HTTP, as `disable` does.
    ///
    /// # Errors
    ///
    /// When the TLS settings can't be used, or the credentials can't be a header.
    pub(crate) fn new(
        config: &ElasticsearchConfig,
        password: Option<&Password>,
        address: &str,
    ) -> Result<Self, BoxError> {
        let server = &config.server;
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        // The address may be a tunnel's local end; the certificate must name the node.
        let server_name = ServerName::try_from(server.host.clone())?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(dbrover_tls::client_config(&server.tls)?)
            .https_or_http()
            .with_server_name_resolver(FixedServerNameResolver::new(server_name))
            .enable_http1()
            .wrap_connector(http);
        let scheme = match server.tls {
            TlsMode::Disable | TlsMode::Prefer => "http",
            TlsMode::Require | TlsMode::VerifyFull { .. } => "https",
        };
        Ok(Self {
            http: HyperClient::builder(TokioExecutor::new()).build(connector),
            base: format!("{scheme}://{address}"),
            authorization: authorization(config, password)?,
        })
    }

    /// Sends `call` and reads the whole answer.
    ///
    /// # Errors
    ///
    /// When the node can't be reached or the answer can't be read; an answer with an error
    /// status is an [`Answer`] all the same.
    pub(crate) async fn send(&self, call: Call<'_>) -> Result<Answer, BoxError> {
        let mut request = Request::builder()
            .method(call.method)
            .uri(format!("{}{}", self.base, call.target));
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        if let Some(opaque_id) = call.opaque_id {
            request = request.header(OPAQUE_ID, opaque_id);
        }
        let body = match call.body {
            Some((text, content_type)) => {
                request = request.header(CONTENT_TYPE, content_type);
                Full::new(Bytes::from(text))
            }
            None => Full::new(Bytes::new()),
        };
        let response = self.http.request(request.body(body)?).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.into_body().collect().await?.to_bytes();
        Ok(Answer {
            status,
            content_type,
            body,
        })
    }
}

/// The `Authorization` header for the data source's credentials: an API key, a user and
/// password, or none for a node without security.
fn authorization(
    config: &ElasticsearchConfig,
    password: Option<&Password>,
) -> Result<Option<HeaderValue>, BoxError> {
    let secret = password.map_or("", Password::expose);
    let value = match config.auth {
        ElasticsearchAuth::ApiKey if secret.is_empty() => {
            return Err("an API key is chosen, but none is saved".into());
        }
        ElasticsearchAuth::ApiKey => format!("ApiKey {secret}"),
        ElasticsearchAuth::Password if config.server.user.is_empty() && secret.is_empty() => {
            return Ok(None);
        }
        ElasticsearchAuth::Password => {
            let pair = format!("{}:{secret}", config.server.user);
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(pair)
            )
        }
    };
    let mut header = HeaderValue::try_from(value)?;
    header.set_sensitive(true);
    Ok(Some(header))
}
