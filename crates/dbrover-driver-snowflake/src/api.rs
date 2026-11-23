//! Snowflake's SQL API (`/api/v2/statements`): JSON over HTTPS with C5c's bearer token, through
//! `dbrover-cloud`'s small HTTPS client.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use dbrover_cloud::snowflake::{SnowflakeError, SnowflakeSignIn};
use dbrover_cloud::{Answer, HttpError, Https};
use serde_json::Value;

/// The header that names the kind of bearer token.
const TOKEN_TYPE: &str = "X-Snowflake-Authorization-Token-Type";

/// The API's path below the account's address.
pub(crate) const STATEMENTS: &str = "/api/v2/statements";

/// The SQL API asks every client to name itself, as `name/version`.
const USER_AGENT: &str = concat!("DbRover/", env!("CARGO_PKG_VERSION"));

/// How many times a request Snowflake answers with 429, 503 or 504 is sent again, and the pause
/// before the first retry, doubling after it. Snowflake's guidance is a growing pause; three
/// retries over about two seconds ride out a moment's throttling without holding a console up
/// for long. No jitter: one console sends one request at a time.
const RETRIES: u32 = 3;
const FIRST_RETRY_PAUSE: Duration = Duration::from_millis(250);

/// Why a request got no answer DbRover could read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    #[error(transparent)]
    SignIn(#[from] SnowflakeError),
    #[error("Couldn't reach Snowflake: {0}")]
    Http(#[from] HttpError),
    /// Snowflake answered with an error: its HTTP status, its own code and its message.
    #[error("{message}")]
    Refused {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("Snowflake's answer can't be read: {0}")]
    Unreadable(String),
}

/// An answer's status and its JSON body, ungzipped.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    /// The reply as an error, with Snowflake's own code and message when its body has them.
    pub(crate) fn refusal(&self) -> ApiError {
        let message = self.body["message"].as_str().map_or_else(
            || format!("HTTP {}: {}", self.status, self.body),
            str::to_owned,
        );
        ApiError::Refused {
            status: self.status,
            code: self.body["code"].as_str().map(str::to_owned),
            message,
        }
    }

    /// Whether Snowflake asks for the request again later: too many requests, or a timeout on
    /// its side.
    fn is_throttled(&self) -> bool {
        matches!(self.status, 429 | 503 | 504)
    }
}

/// A client for one account's address and sign-in. Cheap to clone: clones share the
/// connection pool and the sign-in's last JWT.
#[derive(Clone)]
pub(crate) struct Api {
    https: Https,
    /// `https://<account>.snowflakecomputing.com`, or a private endpoint's or a test server's.
    base: String,
    sign_in: Arc<SnowflakeSignIn>,
}

impl Api {
    /// A client for `account` at `endpoint` (empty for Snowflake's own host), signed in as
    /// `sign_in` says.
    ///
    /// # Errors
    ///
    /// When the system's root certificates can't be loaded.
    pub(crate) fn new(
        account: &str,
        endpoint: &str,
        sign_in: SnowflakeSignIn,
    ) -> Result<Self, ApiError> {
        let base = match endpoint.trim().trim_end_matches('/') {
            "" => format!("https://{}.snowflakecomputing.com", account.trim()),
            endpoint => endpoint.to_owned(),
        };
        Ok(Self {
            https: Https::new()?,
            base,
            sign_in: Arc::new(sign_in),
        })
    }

    /// `GET`s `path_and_query`, whatever its status.
    pub(crate) async fn get(&self, path_and_query: &str) -> Result<Reply, ApiError> {
        self.send(path_and_query, None).await
    }

    /// `POST`s `body` to `path_and_query`, whatever its status.
    pub(crate) async fn post(&self, path_and_query: &str, body: &Value) -> Result<Reply, ApiError> {
        self.send(path_and_query, Some(body.to_string())).await
    }

    /// As [`Api::get`], sending the request again while Snowflake answers that it is
    /// throttled, as its guidance asks. Only for requests that change nothing: a statement's
    /// submission is retried by its caller, which must say `retry=true`.
    pub(crate) async fn get_retrying(&self, path_and_query: &str) -> Result<Reply, ApiError> {
        let mut pause = FIRST_RETRY_PAUSE;
        let mut reply = self.get(path_and_query).await?;
        for _ in 0..RETRIES {
            if !reply.is_throttled() {
                break;
            }
            tokio::time::sleep(pause).await;
            pause *= 2;
            reply = self.get(path_and_query).await?;
        }
        Ok(reply)
    }

    /// Whether `reply` asks for its request again, and the pause before the `attempt`th time
    /// (from 0); `None` once [`RETRIES`] are spent.
    pub(crate) fn retry_pause(reply: &Reply, attempt: u32) -> Option<Duration> {
        (reply.is_throttled() && attempt < RETRIES).then(|| FIRST_RETRY_PAUSE * 2_u32.pow(attempt))
    }

    async fn send(&self, path_and_query: &str, body: Option<String>) -> Result<Reply, ApiError> {
        let (token, kind) = self.sign_in.bearer().await?;
        let address = format!("{}{path_and_query}", self.base);
        let headers = [
            ("Accept", "application/json"),
            ("User-Agent", USER_AGENT),
            (TOKEN_TYPE, kind.header_value()),
        ];
        let answer = self
            .https
            .json_with_headers(&address, Some(&token), &headers, body)
            .await?;
        read(&answer)
    }
}

/// The answer's body as JSON, ungzipped where Snowflake compressed it (a result's later
/// partitions). An empty or non-JSON body becomes its text, for the error that names it.
fn read(answer: &Answer) -> Result<Reply, ApiError> {
    let gzipped = answer
        .header("content-encoding")
        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("gzip"));
    let bytes = if gzipped {
        let mut unzipped = Vec::new();
        flate2::read::GzDecoder::new(answer.body.as_ref())
            .read_to_end(&mut unzipped)
            .map_err(|error| ApiError::Unreadable(format!("its gzip: {error}")))?;
        unzipped
    } else {
        answer.body.to_vec()
    };
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).trim().to_owned()));
    Ok(Reply {
        status: answer.status,
        body,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    fn answer(status: u16, headers: &[(&'static str, &str)], body: Vec<u8>) -> Answer {
        let mut answer = Answer {
            status,
            headers: Default::default(),
            body: body.into(),
        };
        for (name, value) in headers {
            answer
                .headers
                .insert(*name, value.parse().expect("a header value"));
        }
        answer
    }

    #[test]
    fn a_gzipped_partition_reads_as_json() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(br#"{"data": [["1", "a"]]}"#)
            .expect("gzip");
        let gzipped = encoder.finish().expect("gzip");
        let reply = read(&answer(200, &[("content-encoding", "gzip")], gzipped)).expect("read");
        assert_eq!(reply.body["data"][0][1], "a");
    }

    #[test]
    fn snowflakes_errors_carry_their_code_and_message() {
        let reply = read(&answer(
            422,
            &[],
            br#"{"code": "000904", "message": "SQL compilation error: error line 1 at position 7\ninvalid identifier 'AFAF'", "sqlState": "42000"}"#.to_vec(),
        ))
        .expect("read");
        let error = reply.refusal();
        assert!(
            matches!(&error, ApiError::Refused { status: 422, code: Some(code), .. } if code == "000904"),
            "{error:?}"
        );
        let plain = read(&answer(502, &[], b"Bad Gateway".to_vec())).expect("read");
        assert_eq!(plain.refusal().to_string(), "HTTP 502: \"Bad Gateway\"");
    }

    #[test]
    fn throttled_requests_are_retried_three_times_with_a_growing_pause() {
        let throttled = Reply {
            status: 429,
            body: Value::Null,
        };
        let pauses: Vec<Option<Duration>> = (0..4)
            .map(|attempt| Api::retry_pause(&throttled, attempt))
            .collect();
        assert_eq!(
            pauses,
            [
                Some(Duration::from_millis(250)),
                Some(Duration::from_millis(500)),
                Some(Duration::from_millis(1000)),
                None
            ]
        );
        let refused = Reply {
            status: 422,
            body: Value::Null,
        };
        assert_eq!(Api::retry_pause(&refused, 0), None);
    }
}
