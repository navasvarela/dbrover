//! The few HTTPS requests a sign-in makes, to a provider's token endpoint: hyper with rustls on
//! ring and the web's root certificates, as the Elasticsearch driver sends its requests, rather
//! than a provider's SDK (`docs/cloud-data.md`, Decisions).

use std::time::Duration;

use dbrover_core::TlsMode;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::with_causes;

/// How long a token request may take, connecting included. Token endpoints answer in well
/// under a second; a sign-in that hangs would hold the data source's connection up with it.
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

/// What an endpoint answered: its status and its whole body.
#[derive(Debug)]
pub(crate) struct Answer {
    pub status: u16,
    pub body: Bytes,
}

pub(crate) struct Https {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl Https {
    pub(crate) fn new() -> Result<Self, HttpError> {
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
        let exchange = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|error| HttpError::Request(with_causes(&error)))?;
            let status = response.status().as_u16();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| HttpError::Request(with_causes(&error)))?
                .to_bytes();
            Ok(Answer { status, body })
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

#[cfg(test)]
pub(crate) mod stand_in {
    //! A token endpoint on this machine's loopback address that answers one request with a
    //! canned body, and hands back what it was sent.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// What the stand-in received: the request line and the form, decoded.
    pub(crate) struct Received {
        pub request_line: String,
        pub form: Vec<(String, String)>,
    }

    impl Received {
        pub(crate) fn field(&self, name: &str) -> Option<&str> {
            self.form
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    /// Starts the endpoint; returns its address and what it will have received.
    pub(crate) async fn token_endpoint(
        status: u16,
        body: &str,
    ) -> (String, oneshot::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/token", listener.local_addr().unwrap());
        let body = body.to_owned();
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = sender.send(request);
        });
        (address, receiver)
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Received {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
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
        let (address, received) = stand_in::token_endpoint(200, "{\"ok\":true}").await;
        let answer = Https::new()
            .unwrap()
            .post_form(&address, &[("grant_type", "refresh_token"), ("a b", "c&d")])
            .await
            .unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(&answer.body[..], b"{\"ok\":true}");
        let received = received.await.unwrap();
        assert_eq!(received.request_line, "POST /token HTTP/1.1");
        assert_eq!(received.field("grant_type"), Some("refresh_token"));
        assert_eq!(received.field("a b"), Some("c&d"));
    }
}
