//! Databricks's REST APIs (the Statement Execution API, and later Unity Catalog's and the SQL
//! warehouses'): JSON over HTTPS with C6c's bearer token, through `dbrover-cloud`'s small HTTPS
//! client.

use std::sync::Arc;
use std::time::Duration;

use dbrover_cloud::databricks::{DatabricksError, DatabricksSignIn};
use dbrover_cloud::{Answer, HttpError, Https};
use serde_json::Value;

/// Every request names its client, as Databricks's partner guide asks: `name/version`.
const USER_AGENT: &str = concat!("DbRover/", env!("CARGO_PKG_VERSION"));

/// How many times a request Databricks answers with 429 or 503 is sent again. Its SDKs wait
/// for `Retry-After` (a second without one) and keep trying for five minutes; three retries
/// ride out a moment's throttling without holding a console up for long.
const RETRIES: u32 = 3;

/// The pause before a retry when the answer says none, as Databricks's SDKs default it.
const DEFAULT_RETRY_PAUSE: Duration = Duration::from_secs(1);

/// The longest pause a `Retry-After` is followed for: a longer one is a refusal for now.
const LONGEST_RETRY_PAUSE: Duration = Duration::from_secs(10);

/// Why a request got no answer DbRover could read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    #[error(transparent)]
    SignIn(#[from] DatabricksError),
    #[error("Couldn't reach Databricks: {0}")]
    Http(#[from] HttpError),
    /// Databricks refused the request: its HTTP status, its `error_code` and its message.
    #[error("{message}")]
    Refused {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("Databricks's answer can't be read: {0}")]
    Unreadable(String),
}

/// An answer's status, its `Retry-After` in seconds, and its JSON body.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Value,
}

impl Reply {
    /// The reply as an error, with Databricks's `error_code` and message when its body has
    /// them.
    pub(crate) fn refusal(&self) -> ApiError {
        let message = self.body["message"].as_str().map_or_else(
            || format!("HTTP {}: {}", self.status, self.body),
            str::to_owned,
        );
        ApiError::Refused {
            status: self.status,
            code: self.body["error_code"].as_str().map(str::to_owned),
            message,
        }
    }

    /// Whether Databricks asks for the request again later, as its SDKs read 429 and 503:
    /// the request wasn't taken, so sending it again can't run a statement twice.
    fn is_throttled(&self) -> bool {
        matches!(self.status, 429 | 503)
    }

    fn ok(self) -> Result<Value, ApiError> {
        match self.status {
            200 => Ok(self.body),
            _ => Err(self.refusal()),
        }
    }
}

/// A client for one workspace and its sign-in. Cheap to clone: clones share the connection
/// pool and the sign-in's cached token.
#[derive(Clone)]
pub(crate) struct Api {
    https: Https,
    /// `https://<workspace host>`, or a test server's address.
    base: String,
    sign_in: Arc<DatabricksSignIn>,
}

impl Api {
    /// A client for the workspace at `address`, signed in as `sign_in` says.
    ///
    /// # Errors
    ///
    /// When the system's root certificates can't be loaded.
    pub(crate) fn new(address: &str, sign_in: DatabricksSignIn) -> Result<Self, ApiError> {
        Ok(Self {
            https: Https::new()?,
            base: address.trim_end_matches('/').to_owned(),
            sign_in: Arc::new(sign_in),
        })
    }

    /// `GET`s `path_and_query` and reads a 200's body, sending it again while Databricks
    /// answers that it is throttled.
    pub(crate) async fn get(&self, path_and_query: &str) -> Result<Value, ApiError> {
        self.retrying(path_and_query, None).await?.ok()
    }

    /// `POST`s `body` to `path_and_query`, whatever its status, sending it again while
    /// Databricks answers that it is throttled, which means it didn't take it. One whose answer
    /// was lost isn't sent again: the API has no request id, and the statement may have run.
    pub(crate) async fn post(&self, path_and_query: &str, body: &Value) -> Result<Reply, ApiError> {
        self.retrying(path_and_query, Some(body.to_string())).await
    }

    /// Downloads an external link: no token of the workspace's, only the link's own headers.
    pub(crate) async fn download(
        &self,
        link: &str,
        headers: &[(String, String)],
    ) -> Result<Answer, ApiError> {
        Ok(self.https.get_presigned(link, headers).await?)
    }

    async fn retrying(
        &self,
        path_and_query: &str,
        body: Option<String>,
    ) -> Result<Reply, ApiError> {
        let mut attempt = 0;
        loop {
            let reply = self.send(path_and_query, body.clone()).await?;
            match retry_pause(&reply, attempt) {
                Some(pause) => tokio::time::sleep(pause).await,
                None => return Ok(reply),
            }
            attempt += 1;
        }
    }

    async fn send(&self, path_and_query: &str, body: Option<String>) -> Result<Reply, ApiError> {
        let token = self.sign_in.bearer().await?;
        let address = format!("{}{path_and_query}", self.base);
        let headers = [("Accept", "application/json"), ("User-Agent", USER_AGENT)];
        let answer = self
            .https
            .json_with_headers(&address, Some(&token), &headers, body)
            .await?;
        Ok(read(&answer))
    }
}

/// The pause before sending `reply`'s request again for the `attempt`th time (from 0), or
/// `None` when it isn't to be sent again.
fn retry_pause(reply: &Reply, attempt: u32) -> Option<Duration> {
    if !reply.is_throttled() || attempt >= RETRIES {
        return None;
    }
    let pause = reply.retry_after.unwrap_or(DEFAULT_RETRY_PAUSE);
    (pause <= LONGEST_RETRY_PAUSE).then_some(pause)
}

/// The answer's body as JSON; an empty or non-JSON body becomes its text, for the error that
/// names it.
fn read(answer: &Answer) -> Reply {
    let body = serde_json::from_slice(&answer.body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answer.body).trim().to_owned()));
    Reply {
        status: answer.status,
        retry_after: answer
            .header("retry-after")
            .and_then(|seconds| seconds.trim().parse().ok())
            .map(Duration::from_secs),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, retry_after: Option<u64>) -> Reply {
        Reply {
            status,
            retry_after: retry_after.map(Duration::from_secs),
            body: Value::Null,
        }
    }

    #[test]
    fn throttled_requests_wait_as_long_as_databricks_asks_three_times() {
        assert_eq!(
            retry_pause(&reply(429, Some(2)), 0),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            retry_pause(&reply(503, None), 2),
            Some(Duration::from_secs(1))
        );
        assert_eq!(retry_pause(&reply(429, None), 3), None);
        assert_eq!(retry_pause(&reply(429, Some(60)), 0), None);
        assert_eq!(retry_pause(&reply(400, None), 0), None);
        assert_eq!(retry_pause(&reply(500, None), 0), None);
    }

    #[test]
    fn databricks_errors_carry_their_code_and_message() {
        let error = Reply {
            status: 400,
            retry_after: None,
            body: serde_json::json!({"error_code": "INVALID_PARAMETER_VALUE",
                "message": "Warehouse 1234 does not exist"}),
        }
        .refusal();
        assert!(
            matches!(&error, ApiError::Refused { status: 400, code: Some(code), .. }
                if code == "INVALID_PARAMETER_VALUE"),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "Warehouse 1234 does not exist");
        let plain = Reply {
            status: 502,
            retry_after: None,
            body: Value::String("Bad Gateway".to_owned()),
        };
        assert_eq!(plain.refusal().to_string(), "HTTP 502: \"Bad Gateway\"");
    }
}
