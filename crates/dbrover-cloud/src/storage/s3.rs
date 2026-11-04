//! S3's `ListObjectsV2`, a level at a time, signed with SigV4 by AWS's own signer.

use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
    UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4;
use dbrover_core::Password;

use super::{Level, StorageError, xml};
use crate::http::{Answer, Https};

/// The service name S3's requests are signed for.
const SIGNING_NAME: &str = "s3";

/// AWS's own tools' region when nothing names one. A bucket elsewhere answers with a redirect
/// that names its region, which [`S3Bucket::list`] follows once.
const DEFAULT_REGION: &str = "us-east-1";

/// The header in which S3 names a bucket's region when a request went to the wrong one.
const BUCKET_REGION_HEADER: &str = "x-amz-bucket-region";

/// Keys to sign with: an access key, and the session token temporary ones come with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Keys {
    pub key_id: String,
    pub secret: Password,
    pub session_token: Option<Password>,
}

/// A bucket, where it is, and who lists it.
#[derive(Debug, Clone)]
pub struct S3Location {
    pub bucket: String,
    /// Empty for [`DEFAULT_REGION`].
    pub region: String,
    /// An S3-compatible store's address (`http://localhost:9000`), whose buckets are reached by
    /// path; `None` for AWS's own.
    pub endpoint: Option<String>,
    /// `None` for a public bucket, listed without signing.
    pub keys: Option<S3Keys>,
}

/// Lists one bucket over HTTPS, or plain HTTP to an S3-compatible store that has no TLS.
pub struct S3Lister {
    https: Https,
    location: S3Location,
}

impl S3Lister {
    /// # Errors
    ///
    /// [`StorageError::Http`] when TLS can't be set up.
    pub fn new(location: S3Location) -> Result<Self, StorageError> {
        Ok(Self {
            https: Https::new()?,
            location,
        })
    }

    /// The folders and files directly under `prefix` (empty, or ending in `/`), at most `max`
    /// of them, from where `after`, a previous level's [`Level::next`], left off. A bucket in
    /// another region than the one asked answers with its own, which is asked once more.
    ///
    /// # Errors
    ///
    /// [`StorageError`] when the request fails or S3 refuses it, with S3's code and message.
    pub async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        max: usize,
    ) -> Result<Level, StorageError> {
        let region = Some(self.location.region.as_str())
            .filter(|region| !region.is_empty())
            .unwrap_or(DEFAULT_REGION);
        let answer = self.request(region, prefix, after, max).await?;
        let moved_to = (answer.status == 301)
            .then(|| answer.header(BUCKET_REGION_HEADER))
            .flatten()
            .filter(|moved_to| *moved_to != region)
            .map(str::to_owned);
        let answer = match moved_to {
            Some(region) => self.request(&region, prefix, after, max).await?,
            None => answer,
        };
        if answer.status != 200 {
            return Err(xml::s3_refusal(answer.status, &answer.body));
        }
        xml::s3_level(&answer.body)
    }

    async fn request(
        &self,
        region: &str,
        prefix: &str,
        after: Option<&str>,
        max: usize,
    ) -> Result<Answer, StorageError> {
        let url = self.location.list_url(region, prefix, after, max);
        let headers = match &self.location.keys {
            Some(keys) => signed_headers(&url, region, keys, SystemTime::now())?,
            None => Vec::new(),
        };
        Ok(self.https.get_signed(&url, &headers).await?)
    }
}

impl S3Location {
    /// The `ListObjectsV2` request's address: virtual-hosted on AWS, unless the bucket's name
    /// has a dot (which AWS's wildcard certificate doesn't cover), and by path elsewhere.
    fn list_url(&self, region: &str, prefix: &str, after: Option<&str>, max: usize) -> String {
        let base = match &self.endpoint {
            Some(endpoint) => format!(
                "{}/{}",
                endpoint.trim_end_matches('/'),
                encode(&self.bucket)
            ),
            None if self.bucket.contains('.') => {
                format!("https://s3.{region}.amazonaws.com/{}", encode(&self.bucket))
            }
            None => format!("https://{}.s3.{region}.amazonaws.com", self.bucket),
        };
        let mut query = vec![
            ("list-type", "2".to_owned()),
            ("delimiter", "/".to_owned()),
            ("max-keys", max.to_string()),
            ("prefix", prefix.to_owned()),
        ];
        if let Some(after) = after {
            query.push(("continuation-token", after.to_owned()));
        }
        // SigV4 signs the query in this order, by name.
        query.sort_by_key(|(name, _)| *name);
        let query = query
            .iter()
            .map(|(name, value)| format!("{name}={}", encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{base}/?{query}")
    }
}

/// The headers that sign a `GET` of `url` with `keys` in `region` at `now`.
fn signed_headers(
    url: &str,
    region: &str,
    keys: &S3Keys,
    now: SystemTime,
) -> Result<Vec<(String, String)>, StorageError> {
    let signing_failed =
        |error: &dyn std::error::Error| StorageError::Signing(crate::with_causes(error));
    let credentials = Credentials::new(
        &keys.key_id,
        keys.secret.expose(),
        keys.session_token
            .as_ref()
            .map(|token| token.expose().to_owned()),
        None,
        "bucket",
    );
    let identity = credentials.into();
    // S3 signs paths and queries as they are sent, without normalising or encoding them
    // twice, and wants the payload's hash in a header of its own.
    let mut settings = SigningSettings::default();
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name(SIGNING_NAME)
        .time(now)
        .settings(settings)
        .build()
        .map_err(|error| signing_failed(&error))?;
    let request = SignableRequest::new("GET", url, std::iter::empty(), SignableBody::Bytes(&[]))
        .map_err(|error| signing_failed(&error))?;
    let (instructions, _signature) = sign(request, &params.into())
        .map_err(|error| signing_failed(&error))?
        .into_parts();
    Ok(instructions
        .headers()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

/// Percent-encodes everything but SigV4's unreserved characters, as S3 expects a query's
/// values and a path's bucket name: form encoding's `+` for a space would read back as a plus.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn bucket(name: &str, endpoint: Option<&str>) -> S3Location {
        S3Location {
            bucket: name.to_owned(),
            region: String::new(),
            endpoint: endpoint.map(str::to_owned),
            keys: None,
        }
    }

    #[test]
    fn list_urls_are_virtual_hosted_on_aws_and_by_path_elsewhere() {
        let cases = [
            (
                bucket("sales", None),
                "",
                None,
                "https://sales.s3.eu-west-1.amazonaws.com/?delimiter=%2F&list-type=2\
                 &max-keys=1000&prefix=",
            ),
            (
                bucket("logs.example.com", None),
                "2026/",
                None,
                "https://s3.eu-west-1.amazonaws.com/logs.example.com/?delimiter=%2F\
                 &list-type=2&max-keys=1000&prefix=2026%2F",
            ),
            (
                bucket("dbrover", Some("http://localhost:59000/")),
                "sales/year=2025/",
                Some("1/Ab+c=="),
                "http://localhost:59000/dbrover/?continuation-token=1%2FAb%2Bc%3D%3D\
                 &delimiter=%2F&list-type=2&max-keys=1000&prefix=sales%2Fyear%3D2025%2F",
            ),
        ];
        for (bucket, prefix, after, expected) in cases {
            assert_eq!(bucket.list_url("eu-west-1", prefix, after, 1000), expected);
        }
    }

    /// The signature was checked against a separate implementation of SigV4's header signing
    /// in Python's standard library, for the same request, keys and time: AWS's example keys,
    /// at the time of AWS's own examples.
    #[test]
    fn signs_a_listing_with_the_payload_hash_and_session_token() {
        let keys = S3Keys {
            key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret: Password::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
            session_token: Some(Password::new("SESSION")),
        };
        // 2013-05-24T00:00:00Z, the time of AWS's own examples.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_369_353_600);
        let url = bucket("examplebucket", None).list_url("us-east-1", "", None, 1000);

        let headers = signed_headers(&url, "us-east-1", &keys, now).unwrap();

        let header = |name: &str| {
            headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("x-amz-date"), Some("20130524T000000Z"));
        assert_eq!(
            header("x-amz-content-sha256"),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(header("x-amz-security-token"), Some("SESSION"));
        assert_eq!(
            header("authorization"),
            Some(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/\
                 aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date;\
                 x-amz-security-token, \
                 Signature=5c10cb1c4cf48ffe1b50a288fe91230111a1fec737dcc8cc8ec30c343861589e"
            )
        );
    }
}
