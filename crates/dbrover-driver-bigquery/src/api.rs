//! BigQuery's REST API (`bigquery/v2`): JSON over HTTPS with C0b's access token as a bearer
//! token, through `dbrover-cloud`'s small HTTPS client.

use std::fmt::Write as _;
use std::path::PathBuf;

use dbrover_cloud::google::{GoogleError, GoogleSignIn};
use dbrover_cloud::{Answer, HttpError, Https};
use dbrover_core::{GoogleCloudSignIn, Password, Variables};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Google's own address for the API.
const GOOGLE_ENDPOINT: &str = "https://bigquery.googleapis.com";

/// The API's path below its address.
const API_PATH: &str = "/bigquery/v2";

/// Why a request got no answer DbRover could read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    #[error(transparent)]
    SignIn(#[from] GoogleError),
    #[error("Couldn't reach BigQuery: {0}")]
    Http(#[from] HttpError),
    /// BigQuery answered with an error: its HTTP status, its reason (`notFound`,
    /// `accessDenied`, `invalidQuery`, …) and its message.
    #[error("{message}")]
    Refused {
        status: u16,
        reason: Option<String>,
        message: String,
    },
    #[error("BigQuery's answer can't be read: {0}")]
    Unreadable(String),
}

/// Who requests are sent as.
#[derive(Debug, Clone)]
enum Bearer {
    Google(GoogleSignIn),
    /// No token at all, for the emulator.
    Nobody,
}

/// A client for one data source's address and sign-in. Cheap to clone: clones share the
/// connection pool and, through C0b's cache, the token.
#[derive(Clone)]
pub(crate) struct Api {
    https: Https,
    /// `https://bigquery.googleapis.com/bigquery/v2`, or the emulator's or a test server's.
    base: String,
    bearer: Bearer,
}

/// A query string's parameters, in order.
pub(crate) type Query<'a> = &'a [(&'a str, &'a str)];

impl Api {
    /// A client for `endpoint` (empty for Google's), signed in as `sign_in` says.
    ///
    /// # Errors
    ///
    /// When the system's root certificates can't be loaded.
    pub(crate) fn new(endpoint: &str, sign_in: &GoogleCloudSignIn) -> Result<Self, ApiError> {
        let address = match endpoint.trim_end_matches('/') {
            "" => GOOGLE_ENDPOINT,
            endpoint => endpoint,
        };
        let bearer = match sign_in {
            GoogleCloudSignIn::Google => Bearer::Google(GoogleSignIn::default()),
            GoogleCloudSignIn::CredentialsFile { credentials_file } => {
                Bearer::Google(GoogleSignIn {
                    credentials_file: Some(credentials_file.clone()),
                })
            }
            GoogleCloudSignIn::Anonymous => Bearer::Nobody,
        };
        Ok(Self {
            https: Https::new()?,
            base: format!("{address}{API_PATH}"),
            bearer,
        })
    }

    /// `GET`s `path` (below the API's, its segments already encoded with [`segment`]) with
    /// `query`, and reads the answer as `T`.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Query<'_>,
    ) -> Result<T, ApiError> {
        self.send(path, query, None).await
    }

    /// `POST`s `body` to `path` with `query`, and reads the answer as `T`.
    pub(crate) async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Query<'_>,
        body: &serde_json::Value,
    ) -> Result<T, ApiError> {
        self.send(path, query, Some(body.to_string())).await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Query<'_>,
        body: Option<String>,
    ) -> Result<T, ApiError> {
        let token = self.token().await?;
        let address = self.address(path, query);
        let answer = self
            .https
            .json_with_token(&address, token.as_ref(), body)
            .await?;
        read(&answer)
    }

    async fn token(&self) -> Result<Option<Password>, ApiError> {
        match &self.bearer {
            Bearer::Google(google) => {
                Ok(Some(google.cached_token(&ProcessVariables).await?.secret))
            }
            Bearer::Nobody => Ok(None),
        }
    }

    fn address(&self, path: &str, query: Query<'_>) -> String {
        let mut address = format!("{}{path}", self.base);
        for (index, (name, value)) in query.iter().enumerate() {
            let separator = if index == 0 { '?' } else { '&' };
            let _ = write!(address, "{separator}{name}={}", segment(value));
        }
        address
    }
}

/// The answer's body as `T`, or BigQuery's error.
fn read<T: DeserializeOwned>(answer: &Answer) -> Result<T, ApiError> {
    if (200..300).contains(&answer.status) {
        return serde_json::from_slice(&answer.body)
            .map_err(|error| ApiError::Unreadable(error.to_string()));
    }
    Err(refusal(answer))
}

/// Google's error body: `{"error": {"code", "message", "errors": [{"reason", …}]}}`.
#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
    #[serde(default)]
    errors: Vec<ErrorReason>,
}

#[derive(Deserialize)]
struct ErrorReason {
    reason: Option<String>,
}

fn refusal(answer: &Answer) -> ApiError {
    match serde_json::from_slice::<ErrorBody>(&answer.body) {
        Ok(body) => ApiError::Refused {
            status: answer.status,
            reason: body
                .error
                .errors
                .into_iter()
                .find_map(|reason| reason.reason),
            message: body.error.message,
        },
        Err(_) => ApiError::Refused {
            status: answer.status,
            reason: None,
            message: format!(
                "HTTP {}: {}",
                answer.status,
                String::from_utf8_lossy(&answer.body).trim()
            ),
        },
    }
}

/// `text` as one segment of a path or a query's value: everything but RFC 3986's unreserved
/// characters percent-encoded. A table's name may hold spaces and any letter.
pub(crate) fn segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The process's environment, where Application Default Credentials are looked for.
struct ProcessVariables;

impl Variables for ProcessVariables {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::home_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_keep_only_unreserved_characters() {
        for (text, encoded) in [
            ("my-project", "my-project"),
            ("google.com:analytics", "google.com%3Aanalytics"),
            ("daily sales", "daily%20sales"),
            ("ventes_été", "ventes_%C3%A9t%C3%A9"),
            ("a/b?c=d&e", "a%2Fb%3Fc%3Dd%26e"),
        ] {
            assert_eq!(segment(text), encoded);
        }
    }

    #[test]
    fn google_errors_carry_their_reason() {
        let answer = Answer {
            status: 404,
            headers: Default::default(),
            body: r#"{"error": {"code": 404, "message": "Not found: Dataset p:nope",
                "errors": [{"message": "Not found: Dataset p:nope", "domain": "global",
                "reason": "notFound"}], "status": "NOT_FOUND"}}"#
                .into(),
        };
        let error = read::<serde_json::Value>(&answer).unwrap_err();
        assert!(
            matches!(&error, ApiError::Refused { status: 404, reason: Some(reason), .. } if reason == "notFound"),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "Not found: Dataset p:nope");
        let plain = Answer {
            status: 502,
            headers: Default::default(),
            body: "Bad Gateway".into(),
        };
        assert_eq!(
            read::<serde_json::Value>(&plain).unwrap_err().to_string(),
            "HTTP 502: Bad Gateway"
        );
    }
}
