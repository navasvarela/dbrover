//! The few HTTPS requests a sign-in makes, to a provider's token endpoint, and those of the
//! small API clients built on it (a bucket's listing, BigQuery's REST API): hyper with rustls
//! on ring and the web's root certificates, as the Elasticsearch driver sends its requests,
//! rather than a provider's SDK (`docs/cloud-data.md`, Decisions).

use std::time::Duration;

use dbrover_core::{Password, TlsMode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::with_causes;

/// How long a request may take, connecting included. Token endpoints, a bucket's listing and
/// BigQuery's job calls answer in well under a second (DbRover never asks BigQuery to wait for
/// a job); a request that hangs would hold the connection up with it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("the address {0} isn't one DbRover sends credentials to: it must be https://")]
    Insecure(String),
    #[error("the address {0} can't be read")]
    BadAddress(String),
    #[error("TLS couldn't be set up: {0}")]
    Tls(#[from] dbrover_tls::TlsSetupError),
    #[error("{0}")]
    Request(String),
    #[error("no answer within {} seconds", REQUEST_TIMEOUT.as_secs())]
    TimedOut,
}

/// What an endpoint answered: its status, its headers and its whole body.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Answer {
    /// The header's value, when it is there and is text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }
}

/// A client for HTTPS APIs. Cheap to clone: clones share the connection pool.
#[derive(Clone)]
pub struct Https {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl Https {
    /// # Errors
    ///
    /// [`HttpError::Tls`] when the system's root certificates can't be loaded.
    pub fn new() -> Result<Self, HttpError> {
        let tls = dbrover_tls::client_config(&TlsMode::VerifyFull { ca_file: None })?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        })
    }

    /// Posts `form` as `application/x-www-form-urlencoded` to `address`.
    ///
    /// Credentials only ever go over HTTPS, except to this machine's own loopback address,
    /// where the tests' stand-in endpoints listen.
    pub(crate) async fn post_form(
        &self,
        address: &str,
        form: &[(&str, &str)],
    ) -> Result<Answer, HttpError> {
        let uri = sendable(address)?;
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let mut request = Request::new(Full::new(Bytes::from(body)));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = uri;
        request.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.send(request).await
    }

    /// Gets `address` with `headers`, which carry no secret of their own: a request signed
    /// with SigV4 shows only its signature, so it may go over plain HTTP to a store on the
    /// network, as DuckDB's own requests to it do.
    pub(crate) async fn get_signed(
        &self,
        address: &str,
        headers: &[(String, String)],
    ) -> Result<Answer, HttpError> {
        let uri: Uri = address
            .parse()
            .map_err(|_| HttpError::BadAddress(address.to_owned()))?;
        let mut request = Request::new(Full::new(Bytes::new()));
        *request.uri_mut() = uri;
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpError::BadAddress(address.to_owned()))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| HttpError::BadAddress(address.to_owned()))?;
            request.headers_mut().insert(name, value);
        }
        self.send(request).await
    }

    /// Gets `address` with `headers`, and `token` as a bearer token when there is one. A
    /// token is a password for its lifetime, so it goes only over HTTPS, or to this machine.
    pub(crate) async fn get_with_token(
        &self,
        address: &str,
        token: Option<&Password>,
        headers: &[(String, String)],
    ) -> Result<Answer, HttpError> {
        let mut headers = headers.to_vec();
        if let Some(token) = token {
            sendable(address)?;
            headers.push((
                AUTHORIZATION.to_string(),
                format!("Bearer {}", token.expose()),
            ));
        }
        self.get_signed(address, &headers).await
    }

    /// Sends `body` as JSON (a `GET` when there is none, a `POST` when there is) to `address`,
    /// with `token` as a bearer token when there is one, which goes only over HTTPS or to this
    /// machine, as [`Https::get_with_token`]'s.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the address can't take the token or can't be reached; an answer with
    /// an error status is an [`Answer`] all the same.
    pub async fn json_with_token(
        &self,
        address: &str,
        token: Option<&Password>,
        body: Option<String>,
    ) -> Result<Answer, HttpError> {
        let uri = match token {
            Some(_) => sendable(address)?,
            None => address
                .parse()
                .map_err(|_| HttpError::BadAddress(address.to_owned()))?,
        };
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let mut request = Request::new(Full::new(Bytes::from(body.unwrap_or_default())));
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        let headers = request.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                .map_err(|_| HttpError::BadAddress(address.to_owned()))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        self.send(request).await
    }

    async fn send(&self, request: Request<Full<Bytes>>) -> Result<Answer, HttpError> {
        let exchange = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|error| HttpError::Request(with_causes(&error)))?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| HttpError::Request(with_causes(&error)))?
                .to_bytes();
            Ok(Answer {
                status,
                headers,
                body,
            })
        };
        tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| HttpError::TimedOut)?
    }
}

/// `address` as a URI credentials may be sent to.
fn sendable(address: &str) -> Result<Uri, HttpError> {
    let uri: Uri = address
        .parse()
        .map_err(|_| HttpError::BadAddress(address.to_owned()))?;
    let loopback = matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"));
    match uri.scheme_str() {
        Some("https") => Ok(uri),
        Some("http") if loopback => Ok(uri),
        _ => Err(HttpError::Insecure(address.to_owned())),
    }
}

#[cfg(any(test, feature = "stand-in"))]
pub mod stand_in {
    //! A token endpoint on this machine's loopback address that answers every request with
    //! the same canned body and hands back what it was sent, for tests that sign in without a
    //! cloud account.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;

    /// What the stand-in received: the request line and the form, decoded.
    #[derive(Debug)]
    pub struct Received {
        pub request_line: String,
        pub form: Vec<(String, String)>,
    }

    impl Received {
        pub fn field(&self, name: &str) -> Option<&str> {
            self.form
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    /// A running stand-in: where it listens, and each request it has answered.
    pub struct TokenEndpoint {
        /// `http://127.0.0.1:<port>/token`.
        pub address: String,
        pub received: mpsc::UnboundedReceiver<Received>,
    }

    /// Starts the endpoint, which answers with `status` and the JSON `body` until the test's
    /// runtime ends.
    ///
    /// # Errors
    ///
    /// When the loopback address can't be listened on.
    pub async fn token_endpoint(status: u16, body: &str) -> std::io::Result<TokenEndpoint> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("http://{}/token", listener.local_addr()?);
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{body}",
            body.len()
        );
        let (sender, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let request = read_request(&mut stream).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = sender.send(request);
            }
        });
        Ok(TokenEndpoint { address, received })
    }

    async fn read_request(stream: &mut TcpStream) -> Received {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap_or(0);
            bytes.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                if read == 0 {
                    return Received {
                        request_line: text,
                        form: Vec::new(),
                    };
                }
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length || read == 0 {
                return Received {
                    request_line: head.lines().next().unwrap_or_default().to_owned(),
                    form: form_urlencoded::parse(body.as_bytes())
                        .into_owned()
                        .collect(),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_go_only_over_https_or_to_this_machine() {
        for (address, sendable_to) in [
            ("https://oauth2.googleapis.com/token", true),
            ("http://127.0.0.1:4000/token", true),
            ("http://localhost:4000/token", true),
            ("http://oauth2.googleapis.com/token", false),
            ("ftp://example.com/token", false),
            ("not an address", false),
        ] {
            assert_eq!(sendable(address).is_ok(), sendable_to, "{address}");
        }
    }

    #[tokio::test]
    async fn a_form_is_posted_and_the_answer_read() {
        let mut endpoint = stand_in::token_endpoint(200, "{\"ok\":true}")
            .await
            .unwrap();
        let answer = Https::new()
            .unwrap()
            .post_form(
                &endpoint.address,
                &[("grant_type", "refresh_token"), ("a b", "c&d")],
            )
            .await
            .unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(&answer.body[..], b"{\"ok\":true}");
        let received = endpoint.received.recv().await.unwrap();
        assert_eq!(received.request_line, "POST /token HTTP/1.1");
        assert_eq!(received.field("grant_type"), Some("refresh_token"));
        assert_eq!(received.field("a b"), Some("c&d"));
    }
}
