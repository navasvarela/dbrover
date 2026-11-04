//! Google Cloud Storage's JSON API: a bucket's objects a level at a time, with an access token
//! from C0's Google sign-in.

use dbrover_core::Password;
use serde::Deserialize;

use super::{Level, StorageError, StoredFile, encode};
use crate::http::Https;

/// Google's own address for the JSON API.
const GOOGLE_ENDPOINT: &str = "https://storage.googleapis.com";

/// A bucket, where it is, and the token that lists it.
#[derive(Debug, Clone)]
pub struct GcsLocation {
    pub bucket: String,
    /// A stand-in's address (`http://localhost:4443`); `None` for Google's.
    pub endpoint: Option<String>,
    /// `None` for a public bucket.
    pub token: Option<Password>,
}

/// Lists one bucket.
pub struct GcsLister {
    https: Https,
    location: GcsLocation,
}

impl GcsLister {
    /// # Errors
    ///
    /// [`StorageError::Http`] when TLS can't be set up.
    pub fn new(location: GcsLocation) -> Result<Self, StorageError> {
        Ok(Self {
            https: Https::new()?,
            location,
        })
    }

    /// The folders and files directly under `prefix`, at most `max`, from where `after`, a
    /// previous level's [`Level::next`], left off.
    ///
    /// # Errors
    ///
    /// [`StorageError`] when the request fails or Google refuses it, with its reason.
    pub async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        max: usize,
    ) -> Result<Level, StorageError> {
        let url = self.location.list_url(prefix, after, max);
        let answer = self
            .https
            .get_with_token(&url, self.location.token.as_ref(), &[])
            .await?;
        if answer.status != 200 {
            return Err(refusal(answer.status, &answer.body));
        }
        level(&answer.body)
    }
}

impl GcsLocation {
    fn list_url(&self, prefix: &str, after: Option<&str>, max: usize) -> String {
        let base = self
            .endpoint
            .as_deref()
            .unwrap_or(GOOGLE_ENDPOINT)
            .trim_end_matches('/');
        let mut url = format!(
            "{base}/storage/v1/b/{}/o?delimiter=%2F&maxResults={max}&prefix={}",
            encode(&self.bucket),
            encode(prefix)
        );
        if let Some(after) = after {
            url.push_str("&pageToken=");
            url.push_str(&encode(after));
        }
        url
    }
}

/// `objects.list`'s answer, as Google documents it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Objects {
    #[serde(default)]
    prefixes: Vec<String>,
    #[serde(default)]
    items: Vec<Object>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct Object {
    name: String,
    /// A number written as a string, as the JSON API writes 64-bit integers.
    #[serde(default)]
    size: Option<String>,
}

fn level(body: &[u8]) -> Result<Level, StorageError> {
    let objects: Objects = serde_json::from_slice(body)
        .map_err(|error| StorageError::Unreadable(error.to_string()))?;
    Ok(Level {
        folders: objects.prefixes,
        files: objects
            .items
            .into_iter()
            .map(|object| StoredFile {
                key: object.name,
                size: object
                    .size
                    .and_then(|size| size.parse().ok())
                    .unwrap_or_default(),
            })
            .collect(),
        next: objects.next_page_token,
    })
}

/// Google's error document: `{"error": {"code": 403, "message": …, "errors": [{"reason": …}]}}`.
#[derive(Deserialize)]
struct ErrorAnswer {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
    #[serde(default)]
    errors: Vec<ErrorReason>,
}

#[derive(Deserialize)]
struct ErrorReason {
    reason: String,
}

fn refusal(status: u16, body: &[u8]) -> StorageError {
    let answer = serde_json::from_slice::<ErrorAnswer>(body).ok();
    StorageError::Refused {
        status,
        code: answer
            .as_ref()
            .and_then(|answer| answer.error.errors.first())
            .map_or_else(|| "noReason".to_owned(), |reason| reason.reason.clone()),
        message: answer.map_or_else(
            || "Google refused to list the bucket".to_owned(),
            |answer| answer.error.message,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_urls_name_the_bucket_prefix_and_page() {
        let cases = [
            (
                None,
                "",
                None,
                "https://storage.googleapis.com/storage/v1/b/sales/o?delimiter=%2F\
                 &maxResults=1000&prefix=",
            ),
            (
                Some("http://localhost:59100/"),
                "2026/q 1/",
                Some("Cg5z"),
                "http://localhost:59100/storage/v1/b/sales/o?delimiter=%2F&maxResults=1000\
                 &prefix=2026%2Fq%201%2F&pageToken=Cg5z",
            ),
        ];
        for (endpoint, prefix, after, expected) in cases {
            let location = GcsLocation {
                bucket: "sales".to_owned(),
                endpoint: endpoint.map(str::to_owned),
                token: None,
            };
            assert_eq!(location.list_url(prefix, after, 1000), expected);
        }
    }

    #[test]
    fn reads_prefixes_items_and_the_next_page() {
        let body = br#"{"kind": "storage#objects",
            "prefixes": ["2026/01/"],
            "items": [{"name": "2026/orders.parquet", "size": "4096"}],
            "nextPageToken": "Cg5z"}"#;

        let level = level(body).unwrap();

        assert_eq!(level.folders, ["2026/01/"]);
        assert_eq!(
            level.files,
            [StoredFile {
                key: "2026/orders.parquet".to_owned(),
                size: 4096,
            }]
        );
        assert_eq!(level.next.as_deref(), Some("Cg5z"));
        assert_eq!(level_of_empty(), Level::default());
    }

    fn level_of_empty() -> Level {
        level(br#"{"kind": "storage#objects"}"#).unwrap()
    }

    #[test]
    fn a_refusal_carries_googles_reason_and_message() {
        let body = br#"{"error": {"code": 403, "message": "reader@x.iam.gserviceaccount.com does not have storage.objects.list access",
            "errors": [{"domain": "global", "reason": "forbidden", "message": "Forbidden"}]}}"#;

        let refusal = refusal(403, body).to_string();

        assert_eq!(
            refusal,
            "reader@x.iam.gserviceaccount.com does not have storage.objects.list access \
             (forbidden, HTTP 403)"
        );
        assert_eq!(
            super::refusal(502, b"Bad gateway").to_string(),
            "Google refused to list the bucket (noReason, HTTP 502)"
        );
    }
}
