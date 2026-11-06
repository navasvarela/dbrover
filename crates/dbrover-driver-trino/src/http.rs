//! Requests to a coordinator, over HTTP/1.1 with hyper, as the Elasticsearch driver sends
//! them: plain, or TLS with `dbrover-tls`'s configuration. The protocol's own headers are
//! spelt `X-Trino-…`, or `X-Presto-…` for Presto ([`Headers`]).

use std::time::Duration;

use base64::Engine;
use dbrover_core::{BoxError, Password, TlsMode, TrinoConfig, TrinoProtocol};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::ServerName;

/// How long a new TCP connection may take. Requests themselves have no limit: the coordinator
/// answers each within a second or so while a query runs.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What DbRover tells the coordinator it is, which Trino's web UI shows beside each query.
const SOURCE: &str = "dbrover";

/// The protocol's header names, in the server's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Headers {
    prefix: &'static str,
}

impl Headers {
    pub(crate) fn of(protocol: TrinoProtocol) -> Self {
        Self {
            prefix: match protocol {
                TrinoProtocol::Trino => "x-trino-",
                TrinoProtocol::Presto => "x-presto-",
            },
        }
    }

    /// The header `name` (`User`, `Set-Catalog`, …) in this spelling, lowercase as HTTP/1.1
    /// compares names.
    pub(crate) fn name(self, name: &str) -> String {
        format!("{}{}", self.prefix, name.to_ascii_lowercase())
    }
}

/// A client for one coordinator, with the data source's user and credentials. Cheap to clone:
/// clones share the connection pool.
#[derive(Clone)]
pub(crate) struct Client {
    http: HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>,
    /// `http://host:port` or `https://host:port`, the coordinator's or an SSH tunnel's.
    base: String,
    user: String,
    authorization: Option<HeaderValue>,
    pub(crate) headers: Headers,
}

/// What the coordinator answered: its status, headers and whole body.
pub(crate) struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Answer {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Why the client couldn't be made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientError {
    #[error(
        "Trino takes a password only over HTTPS; set TLS to require or verify-full, or leave \
         the password empty"
    )]
    PasswordOverHttp,
}

impl Client {
    /// A client for the coordinator in `config`, reached at `address` (`host:port`, the
    /// coordinator's own or an SSH tunnel's).
    ///
    /// HTTPS is a port of its own, not an upgrade of the HTTP one, so `prefer` can't try TLS
    /// and fall back: it means plain HTTP, as `disable` does.
    ///
    /// # Errors
    ///
    /// When the TLS settings can't be used, a password would go over plain HTTP, or the
    /// credentials can't be a header.
    pub(crate) fn new(
        config: &TrinoConfig,
        password: Option<&Password>,
        address: &str,
    ) -> Result<Self, BoxError> {
        let server = &config.server;
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        // The address may be a tunnel's local end; the certificate must name the coordinator.
        let server_name = ServerName::try_from(server.host.clone())?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(dbrover_tls::client_config(&server.tls)?)
            .https_or_http()
            .with_server_name_resolver(FixedServerNameResolver::new(server_name))
            .enable_http1()
            .wrap_connector(http);
        let tls = matches!(server.tls, TlsMode::Require | TlsMode::VerifyFull { .. });
        let secret = password.map_or("", Password::expose);
        if !secret.is_empty() && !tls {
            return Err(ClientError::PasswordOverHttp.into());
        }
        let authorization = (!secret.is_empty())
            .then(|| {
                let pair = format!("{}:{secret}", server.user);
                let value = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(pair)
                );
                HeaderValue::try_from(value).map(|mut header| {
                    header.set_sensitive(true);
                    header
                })
            })
            .transpose()?;
        let scheme = if tls { "https" } else { "http" };
        Ok(Self {
            http: HyperClient::builder(TokioExecutor::new()).build(connector),
            base: format!("{scheme}://{address}"),
            user: server.user.clone(),
            authorization,
            headers: Headers::of(config.protocol),
        })
    }

    /// The address to send a request for `uri` to: a path on the coordinator, or one of the
    /// coordinator's own `nextUri`s with its scheme and host replaced by this client's.
    ///
    /// A coordinator names itself as it sees itself (`http://trino-0.internal:8080/…`), which a
    /// client behind an SSH tunnel or a load balancer can't reach; the path is the part that
    /// matters, as the request still goes to the same coordinator.
    fn address(&self, uri: &str) -> String {
        let path = match uri.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
            None => uri,
        };
        format!("{}{path}", self.base)
    }

    /// Sends a request with the user, the credentials and `headers`, and reads the whole
    /// answer.
    ///
    /// # Errors
    ///
    /// When the coordinator can't be reached or the answer can't be read; an answer with an
    /// error status is an [`Answer`] all the same.
    pub(crate) async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<&str>,
        headers: &[(String, String)],
    ) -> Result<Answer, BoxError> {
        let mut request = Request::builder()
            .method(method)
            .uri(self.address(uri))
            .header(self.headers.name("User"), self.user.as_str())
            .header(self.headers.name("Source"), SOURCE);
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        for (name, value) in headers {
            request = request.header(HeaderName::try_from(name.as_str())?, value.as_str());
        }
        let body = match body {
            Some(text) => {
                request = request.header(CONTENT_TYPE, "text/plain; charset=utf-8");
                Full::new(Bytes::from(text.to_owned()))
            }
            None => Full::new(Bytes::new()),
        };
        let response = self.http.request(request.body(body)?).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await?.to_bytes();
        Ok(Answer {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{Port, ServerConfig};

    use super::*;

    fn config(tls: TlsMode) -> TrinoConfig {
        TrinoConfig {
            server: ServerConfig {
                host: "localhost".to_owned(),
                port: Port::Number(8080),
                database: String::new(),
                user: "ada".to_owned(),
                tls,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
            schema: String::new(),
            protocol: TrinoProtocol::Trino,
        }
    }

    #[test]
    fn next_uris_go_to_this_clients_address() {
        let client = Client::new(&config(TlsMode::Disable), None, "127.0.0.1:4100").unwrap();
        for (uri, expected) in [
            (
                "http://trino-0.internal:8080/v1/statement/queued/q1/x/1",
                "http://127.0.0.1:4100/v1/statement/queued/q1/x/1",
            ),
            ("/v1/statement", "http://127.0.0.1:4100/v1/statement"),
            ("https://lb.example.com", "http://127.0.0.1:4100/"),
        ] {
            assert_eq!(client.address(uri), expected);
        }
    }

    #[test]
    fn a_password_goes_only_over_https() {
        let password = Password::new("hunter2");
        let error = Client::new(&config(TlsMode::Prefer), Some(&password), "localhost:8080")
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|error| error.contains("only over HTTPS")));
        assert!(Client::new(&config(TlsMode::Require), Some(&password), "localhost:443").is_ok());
        assert!(Client::new(&config(TlsMode::Disable), None, "localhost:8080").is_ok());
    }

    #[test]
    fn presto_spells_the_headers_its_own_way() {
        assert_eq!(
            Headers::of(TrinoProtocol::Trino).name("Set-Catalog"),
            "x-trino-set-catalog"
        );
        assert_eq!(
            Headers::of(TrinoProtocol::Presto).name("User"),
            "x-presto-user"
        );
    }
}
