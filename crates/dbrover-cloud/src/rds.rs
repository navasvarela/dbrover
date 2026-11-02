//! IAM authentication tokens for Amazon RDS and Aurora (PostgreSQL and MySQL): a request to
//! connect, presigned on the machine with the sign-in's AWS credentials, which the database
//! takes as the password (`docs/cloud-data.md`, C0d). Nothing is sent to AWS to make one.

use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_credential_types::provider::ProvideCredentials;
use aws_sigv4::http_request::{
    SignableBody, SignableRequest, SignatureLocation, SigningSettings, sign,
};
use aws_sigv4::sign::v4;
use dbrover_core::Password;

use crate::aws::{AwsError, SdkConfig};
use crate::token::AccessToken;
use crate::with_causes;

/// How long RDS accepts a token for opening a connection, the longest it allows. A connection
/// opened with it stays open after it expires.
const TOKEN_LIFETIME: Duration = Duration::from_secs(900);

/// The service name RDS's tokens are signed for.
const SIGNING_NAME: &str = "rds-db";

/// The database a token opens a connection to, as its endpoint names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdsDatabase<'a> {
    pub host: &'a str,
    pub port: u16,
    /// The database user that was granted `rds_iam` (PostgreSQL) or created with
    /// `AWSAuthenticationPlugin` (MySQL).
    pub user: &'a str,
}

/// A token for `database`, signed with `config`'s credentials in its region.
///
/// # Errors
///
/// [`AwsError`] when the configuration has no credentials or region, or they can't sign.
pub async fn auth_token(
    config: &SdkConfig,
    database: &RdsDatabase<'_>,
) -> Result<AccessToken, AwsError> {
    let region = config.region().ok_or(AwsError::NoRegion)?;
    let provider = config
        .credentials_provider()
        .ok_or_else(|| AwsError::Credentials("the sign-in has no credentials".to_owned()))?;
    let credentials = provider
        .provide_credentials()
        .await
        .map_err(|error| AwsError::Credentials(with_causes(&error)))?;
    let now = SystemTime::now();
    let token = token_at(&credentials, region.as_ref(), database, now)?;
    Ok(AccessToken {
        secret: Password::new(token),
        expires_at: now + TOKEN_LIFETIME,
    })
}

/// The token as AWS's own tools make it: the connect request's address without its scheme,
/// with the signature in its query.
fn token_at(
    credentials: &Credentials,
    region: &str,
    database: &RdsDatabase<'_>,
    now: SystemTime,
) -> Result<String, AwsError> {
    let signing_failed = |error: &dyn std::error::Error| {
        AwsError::Credentials(format!(
            "the token couldn't be signed: {}",
            with_causes(error)
        ))
    };
    let mut settings = SigningSettings::default();
    settings.expires_in = Some(TOKEN_LIFETIME);
    settings.signature_location = SignatureLocation::QueryParams;
    let identity = credentials.clone().into();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name(SIGNING_NAME)
        .time(now)
        .settings(settings)
        .build()
        .map_err(|error| signing_failed(&error))?;
    let address = format!(
        "{}:{}/?Action=connect&DBUser={}",
        database.host,
        database.port,
        encode(database.user)
    );
    let url = format!("https://{address}");
    let request = SignableRequest::new("GET", &url, std::iter::empty(), SignableBody::Bytes(&[]))
        .map_err(|error| signing_failed(&error))?;
    let (instructions, _signature) = sign(request, &params.into())
        .map_err(|error| signing_failed(&error))?
        .into_parts();
    let mut token = address;
    for (name, value) in instructions.params() {
        token.push('&');
        token.push_str(&encode(name));
        token.push('=');
        token.push_str(&encode(value));
    }
    Ok(token)
}

/// Percent-encodes everything but SigV4's unreserved characters, so that the query read back
/// is the one that was signed: form encoding's `+` for a space would read back as a plus.
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
    use super::*;

    /// 2026-11-02T12:00:00Z.
    const SIGNED_AT: u64 = 1_793_620_800;

    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    /// The expected tokens were made by a separate implementation of SigV4's presigning, in
    /// Python's standard library, which made the same token as `aws rds
    /// generate-db-auth-token` for the same made-up keys and time.
    #[test]
    fn a_token_is_the_one_aws_tools_make() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(SIGNED_AT);
        let long_term = Credentials::new("AKIDEXAMPLE", SECRET, None, None, "test");
        let postgres = RdsDatabase {
            host: "db.example.eu-west-1.rds.amazonaws.com",
            port: 5432,
            user: "iam_user",
        };
        assert_eq!(
            token_at(&long_term, "eu-west-1", &postgres, now).unwrap(),
            "db.example.eu-west-1.rds.amazonaws.com:5432/?Action=connect&DBUser=iam_user\
             &X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIDEXAMPLE%2F20261102%2Feu-west-1%2Frds-db%2Faws4_request\
             &X-Amz-Date=20261102T120000Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=829d15a1ad6d85fb33c45f69ba5523401174203a328765d4678ab56b571928bc"
        );

        // A role's or IAM Identity Center's credentials carry a session token, and a user
        // name may need encoding.
        let session = Credentials::new(
            "ASIAEXAMPLE",
            SECRET,
            Some("Fwo/session+token=".to_owned()),
            None,
            "test",
        );
        let mysql = RdsDatabase {
            port: 3306,
            user: "iam user+1",
            ..postgres
        };
        let token = token_at(&session, "eu-west-1", &mysql, now).unwrap();
        let mut query: Vec<&str> = token.split_once('?').unwrap().1.split('&').collect();
        query.sort_unstable();
        assert_eq!(
            query,
            [
                "Action=connect",
                "DBUser=iam%20user%2B1",
                "X-Amz-Algorithm=AWS4-HMAC-SHA256",
                "X-Amz-Credential=ASIAEXAMPLE%2F20261102%2Feu-west-1%2Frds-db%2Faws4_request",
                "X-Amz-Date=20261102T120000Z",
                "X-Amz-Expires=900",
                "X-Amz-Security-Token=Fwo%2Fsession%2Btoken%3D",
                "X-Amz-Signature=55f52c72f02fc7dde6dc101bf4c64fbaa7fed4b30a61ed1ac1ada0028db88de7",
                "X-Amz-SignedHeaders=host",
            ]
        );
    }
}
