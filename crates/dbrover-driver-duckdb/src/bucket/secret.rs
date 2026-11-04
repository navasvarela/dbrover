//! The statement that hands DuckDB a bucket's credentials: a temporary secret, scoped to the
//! bucket, which DuckDB keeps in memory only.

use std::fmt;

use dbrover_core::{AzureContainer, GcsBucket, ObjectStorage, Password, S3Bucket};

use super::{BucketCredentials, BucketError, SignedIn};

/// The name DbRover's secret has in DuckDB's `duckdb_secrets()`. Made again under the same
/// name, so a fresh secret replaces the expiring one.
pub(crate) const SECRET_NAME: &str = "dbrover_bucket";

/// AWS's own tools' region when nothing names one. A bucket in another region needs its
/// region in the data source's settings: DbRover's own listing follows S3's redirect to the
/// bucket's region, but whether DuckDB's reads do wasn't tried.
const DEFAULT_S3_REGION: &str = "us-east-1";

/// A `CREATE OR REPLACE TEMPORARY SECRET` statement. It holds credentials, so its `Debug`
/// shows none of it, and nothing but DuckDB should read it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SecretStatement(Password);

impl SecretStatement {
    pub(crate) fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl fmt::Debug for SecretStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretStatement(***)")
    }
}

/// One of a secret's parameters, as DuckDB's `CREATE SECRET` takes it.
enum Value<'a> {
    Text(&'a str),
    Boolean(bool),
}

/// The secret DuckDB reads `storage` with, from what signing in gave.
///
/// # Errors
///
/// [`BucketError::Endpoint`] for an S3 or Google Cloud Storage endpoint that isn't an
/// `http://` or `https://` address.
pub(crate) fn secret(
    storage: &ObjectStorage,
    signed_in: &SignedIn,
) -> Result<SecretStatement, BucketError> {
    match storage {
        ObjectStorage::S3(bucket) => s3_secret(bucket, signed_in),
        ObjectStorage::Gcs(bucket) => gcs_secret(bucket, signed_in),
        ObjectStorage::Azure(container) => Ok(azure_secret(container, signed_in)),
    }
}

/// The secret DuckDB reads an S3 `bucket` with, from what signing in gave.
///
/// # Errors
///
/// [`BucketError::Endpoint`] for an endpoint that isn't an `http://` or `https://` address.
fn s3_secret(bucket: &S3Bucket, signed_in: &SignedIn) -> Result<SecretStatement, BucketError> {
    let scope = format!("s3://{}", bucket.bucket);
    let region = Some(bucket.region.as_str())
        .filter(|region| !region.is_empty())
        .or(signed_in.region.as_deref())
        .unwrap_or(DEFAULT_S3_REGION);
    let mut parameters = vec![
        ("TYPE", Value::Text("s3")),
        ("SCOPE", Value::Text(&scope)),
        ("REGION", Value::Text(region)),
    ];
    match &signed_in.credentials {
        BucketCredentials::S3Keys {
            key_id,
            secret,
            session_token,
        } => {
            parameters.push(("KEY_ID", Value::Text(key_id)));
            parameters.push(("SECRET", Value::Text(secret.expose())));
            if let Some(token) = session_token {
                parameters.push(("SESSION_TOKEN", Value::Text(token.expose())));
            }
        }
        // A token is no S3 credential; DuckDB reads the bucket as if anonymously.
        BucketCredentials::BearerToken(_) | BucketCredentials::Anonymous => {}
    }
    let endpoint = Endpoint::parse(&bucket.endpoint)?;
    push_endpoint(&mut parameters, endpoint.as_ref());
    Ok(statement(&parameters))
}

/// The secret DuckDB reads a Google Cloud Storage `bucket` with: C0's Google token as its
/// bearer token, through `httpfs`.
fn gcs_secret(bucket: &GcsBucket, signed_in: &SignedIn) -> Result<SecretStatement, BucketError> {
    let scope = format!("gs://{}", bucket.bucket);
    let mut parameters = vec![("TYPE", Value::Text("gcs")), ("SCOPE", Value::Text(&scope))];
    if let BucketCredentials::BearerToken(token) = &signed_in.credentials {
        parameters.push(("BEARER_TOKEN", Value::Text(token.expose())));
    }
    let endpoint = Endpoint::parse(&bucket.endpoint)?;
    push_endpoint(&mut parameters, endpoint.as_ref());
    Ok(statement(&parameters))
}

/// The secret DuckDB reads an Azure `container` with: C0's Entra ID token for Azure Storage,
/// through the `azure` extension, or none for a public container.
fn azure_secret(container: &AzureContainer, signed_in: &SignedIn) -> SecretStatement {
    let scope = format!("az://{}", container.container);
    let mut parameters = vec![("TYPE", Value::Text("azure"))];
    match &signed_in.credentials {
        BucketCredentials::BearerToken(token) => {
            parameters.push(("PROVIDER", Value::Text("access_token")));
            parameters.push(("ACCESS_TOKEN", Value::Text(token.expose())));
        }
        BucketCredentials::S3Keys { .. } | BucketCredentials::Anonymous => {
            parameters.push(("PROVIDER", Value::Text("config")));
        }
    }
    parameters.push(("ACCOUNT_NAME", Value::Text(&container.account)));
    parameters.push(("SCOPE", Value::Text(&scope)));
    let endpoint = container.endpoint.trim();
    if !endpoint.is_empty() {
        parameters.push(("ENDPOINT", Value::Text(endpoint)));
    }
    statement(&parameters)
}

/// An S3-compatible store's or a stand-in's address, which serves buckets by path: few have a
/// DNS name per bucket.
fn push_endpoint<'a>(parameters: &mut Vec<(&str, Value<'a>)>, endpoint: Option<&Endpoint<'a>>) {
    if let Some(endpoint) = endpoint {
        parameters.push(("ENDPOINT", Value::Text(endpoint.authority)));
        parameters.push(("URL_STYLE", Value::Text("path")));
        parameters.push(("USE_SSL", Value::Boolean(endpoint.tls)));
    }
}

fn statement(parameters: &[(&str, Value<'_>)]) -> SecretStatement {
    let parameters = parameters
        .iter()
        .map(|(name, value)| match value {
            Value::Text(text) => format!("{name} {}", string_literal(text)),
            Value::Boolean(flag) => format!("{name} {flag}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    SecretStatement(Password::new(format!(
        "CREATE OR REPLACE TEMPORARY SECRET {SECRET_NAME} ({parameters})"
    )))
}

fn string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// An S3-compatible store's address, as DuckDB's secret takes it: the host and port, and
/// whether to use TLS.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Endpoint<'a> {
    pub(crate) authority: &'a str,
    pub(crate) tls: bool,
}

impl<'a> Endpoint<'a> {
    /// `None` for an empty endpoint, which is AWS's own.
    pub(crate) fn parse(endpoint: &'a str) -> Result<Option<Self>, BucketError> {
        let endpoint = endpoint.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            return Ok(None);
        }
        let (authority, tls) = match endpoint.split_once("://") {
            Some(("https", authority)) => (authority, true),
            Some(("http", authority)) => (authority, false),
            _ => return Err(BucketError::Endpoint(endpoint.to_owned())),
        };
        if authority.is_empty() || authority.contains('/') {
            return Err(BucketError::Endpoint(endpoint.to_owned()));
        }
        Ok(Some(Self { authority, tls }))
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::S3SignIn;

    use super::*;

    fn bucket(region: &str, endpoint: &str) -> S3Bucket {
        S3Bucket {
            bucket: "sales".to_owned(),
            region: region.to_owned(),
            endpoint: endpoint.to_owned(),
            sign_in: S3SignIn::Aws,
        }
    }

    fn signed_in(credentials: BucketCredentials, region: Option<&str>) -> SignedIn {
        SignedIn {
            credentials,
            region: region.map(str::to_owned),
            expires_at: None,
        }
    }

    fn keys(session_token: Option<&str>) -> BucketCredentials {
        BucketCredentials::S3Keys {
            key_id: "AKIDEXAMPLE".to_owned(),
            secret: Password::new("wJalr'XUtnFEMI"),
            session_token: session_token.map(Password::new),
        }
    }

    #[test]
    fn s3_secrets_are_scoped_to_the_bucket_with_the_credentials_signing_in_gave() {
        let cases = [
            (
                bucket("", ""),
                signed_in(keys(None), None),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 's3', \
                 SCOPE 's3://sales', REGION 'us-east-1', KEY_ID 'AKIDEXAMPLE', \
                 SECRET 'wJalr''XUtnFEMI')",
            ),
            (
                bucket("", ""),
                signed_in(keys(Some("SESSION")), Some("ap-south-1")),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 's3', \
                 SCOPE 's3://sales', REGION 'ap-south-1', KEY_ID 'AKIDEXAMPLE', \
                 SECRET 'wJalr''XUtnFEMI', SESSION_TOKEN 'SESSION')",
            ),
            (
                bucket("eu-west-1", "http://localhost:59000/"),
                signed_in(keys(None), Some("ap-south-1")),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 's3', \
                 SCOPE 's3://sales', REGION 'eu-west-1', KEY_ID 'AKIDEXAMPLE', \
                 SECRET 'wJalr''XUtnFEMI', ENDPOINT 'localhost:59000', URL_STYLE 'path', \
                 USE_SSL false)",
            ),
            (
                bucket("auto", "https://acme.r2.cloudflarestorage.com"),
                signed_in(BucketCredentials::Anonymous, None),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 's3', \
                 SCOPE 's3://sales', REGION 'auto', \
                 ENDPOINT 'acme.r2.cloudflarestorage.com', URL_STYLE 'path', USE_SSL true)",
            ),
        ];
        for (bucket, signed_in, expected) in cases {
            let statement = s3_secret(&bucket, &signed_in).unwrap();
            assert_eq!(statement.expose(), expected);
        }
    }

    #[test]
    fn gcs_and_azure_secrets_hand_duckdb_the_token() {
        let token = || {
            signed_in(
                BucketCredentials::BearerToken(Password::new("ya29.x")),
                None,
            )
        };
        let anonymous = || signed_in(BucketCredentials::Anonymous, None);
        let gcs = |endpoint: &str| {
            ObjectStorage::Gcs(GcsBucket {
                bucket: "logs".to_owned(),
                endpoint: endpoint.to_owned(),
                ..GcsBucket::default()
            })
        };
        let azure = |endpoint: &str| {
            ObjectStorage::Azure(AzureContainer {
                account: "acme".to_owned(),
                container: "raw".to_owned(),
                endpoint: endpoint.to_owned(),
                ..AzureContainer::default()
            })
        };
        let cases = [
            (
                gcs(""),
                token(),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 'gcs', \
                 SCOPE 'gs://logs', BEARER_TOKEN 'ya29.x')",
            ),
            (
                gcs("http://localhost:59100"),
                anonymous(),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 'gcs', \
                 SCOPE 'gs://logs', ENDPOINT 'localhost:59100', URL_STYLE 'path', \
                 USE_SSL false)",
            ),
            (
                azure(""),
                token(),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 'azure', \
                 PROVIDER 'access_token', ACCESS_TOKEN 'ya29.x', ACCOUNT_NAME 'acme', \
                 SCOPE 'az://raw')",
            ),
            (
                azure("https://acme.privatelink.blob.core.windows.net"),
                anonymous(),
                "CREATE OR REPLACE TEMPORARY SECRET dbrover_bucket (TYPE 'azure', \
                 PROVIDER 'config', ACCOUNT_NAME 'acme', SCOPE 'az://raw', \
                 ENDPOINT 'https://acme.privatelink.blob.core.windows.net')",
            ),
        ];
        for (storage, signed_in, expected) in cases {
            assert_eq!(secret(&storage, &signed_in).unwrap().expose(), expected);
        }
    }

    #[test]
    fn a_secret_statement_never_shows_its_credentials() {
        let statement =
            s3_secret(&bucket("", ""), &signed_in(keys(Some("SESSION")), None)).unwrap();

        assert_eq!(format!("{statement:?}"), "SecretStatement(***)");
    }

    #[test]
    fn endpoints_must_be_http_or_https_addresses_without_a_path() {
        let cases = [
            ("", Ok(None)),
            (
                "http://minio:9000",
                Ok(Some(Endpoint {
                    authority: "minio:9000",
                    tls: false,
                })),
            ),
            (
                " https://s3.example.com/ ",
                Ok(Some(Endpoint {
                    authority: "s3.example.com",
                    tls: true,
                })),
            ),
            ("minio:9000", Err(())),
            ("ftp://minio", Err(())),
            ("https://minio/bucket", Err(())),
            ("https://", Err(())),
        ];
        for (endpoint, expected) in cases {
            let parsed = Endpoint::parse(endpoint).map_err(|_| ());
            assert_eq!(parsed, expected, "{endpoint}");
        }
    }
}
