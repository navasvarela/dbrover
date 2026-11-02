//! Signing in to Google Cloud with Application Default Credentials, found as Google's own
//! libraries find them: the file `GOOGLE_APPLICATION_CREDENTIALS` names, else the one
//! `gcloud auth application-default login` writes (`docs/cloud-data.md`, C0b).
//!
//! DbRover reads the file's two common kinds itself, and keeps nothing: a user's refresh token
//! (`authorized_user`) and a service account's key (`service_account`) are each exchanged at
//! Google's token endpoint for an access token that lasts an hour.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use dbrover_core::{Password, Variables};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::Deserialize;

use crate::http::{Answer, HttpError, Https};
use crate::token::AccessToken;

/// Where Google's token endpoint is, for files that don't say.
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// What a service account's token may be used for: Google Cloud's APIs (BigQuery, C4) and
/// signing in to Cloud SQL as an IAM user, which asks for a scope of its own.
const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                      https://www.googleapis.com/auth/sqlservice.login";

/// How long a service account's signed request is good for: the most Google accepts.
const ASSERTION_LIFETIME: Duration = Duration::from_secs(3600);

/// The variable that names a credentials file, as every Google library reads it.
const CREDENTIALS_VARIABLE: &str = "GOOGLE_APPLICATION_CREDENTIALS";

/// The file `gcloud auth application-default login` writes, in gcloud's configuration folder.
const WELL_KNOWN_FILE: &str = "application_default_credentials.json";

/// How a Google Cloud data source signs in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoogleSignIn {
    /// A credentials file of the data source's own; `None` for Application Default
    /// Credentials.
    pub credentials_file: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    #[error(
        "No Google Cloud sign-in was found: run `gcloud auth application-default login`, or \
         set {CREDENTIALS_VARIABLE} to a service account's key file"
    )]
    NoCredentials,
    #[error("Couldn't read the Google credentials file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} isn't a Google credentials file DbRover can read: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error(
        "{path} holds {kind} credentials, which DbRover can't sign in with yet; use \
         `gcloud auth application-default login` or a service account's key file"
    )]
    Unsupported { path: PathBuf, kind: String },
    #[error("The service account's private key in {path} can't be used: {reason}")]
    Key { path: PathBuf, reason: String },
    #[error("The service account's request to Google couldn't be signed")]
    Signing,
    #[error("Google refused the sign-in: {0}")]
    Refused(String),
    #[error("Couldn't reach Google's token endpoint: {0}")]
    Http(#[from] HttpError),
}

impl GoogleSignIn {
    /// The credentials to sign in with, read from their file.
    ///
    /// # Errors
    ///
    /// [`GoogleError`] when no file is found, or it can't be read or used.
    pub fn credentials(&self, variables: &dyn Variables) -> Result<GoogleCredentials, GoogleError> {
        let path = match &self.credentials_file {
            Some(path) => path.clone(),
            None => default_credentials_file(variables)?,
        };
        let text = std::fs::read_to_string(&path).map_err(|source| GoogleError::Read {
            path: path.clone(),
            source,
        })?;
        GoogleCredentials::parse(&path, &text)
    }
}

/// The file Application Default Credentials come from: `GOOGLE_APPLICATION_CREDENTIALS`'s,
/// else gcloud's, when it exists.
fn default_credentials_file(variables: &dyn Variables) -> Result<PathBuf, GoogleError> {
    if let Some(path) = variables
        .var(CREDENTIALS_VARIABLE)
        .filter(|path| !path.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    gcloud_folder(variables)
        .map(|folder| folder.join(WELL_KNOWN_FILE))
        .filter(|path| path.is_file())
        .ok_or(GoogleError::NoCredentials)
}

/// gcloud's configuration folder: `CLOUDSDK_CONFIG`'s, else `%APPDATA%\gcloud` on Windows and
/// `~/.config/gcloud` elsewhere, as gcloud and Google's libraries look for it.
fn gcloud_folder(variables: &dyn Variables) -> Option<PathBuf> {
    if let Some(folder) = variables
        .var("CLOUDSDK_CONFIG")
        .filter(|folder| !folder.is_empty())
    {
        return Some(PathBuf::from(folder));
    }
    if cfg!(windows) {
        return variables
            .var("APPDATA")
            .map(|folder| Path::new(&folder).join("gcloud"));
    }
    variables
        .home()
        .map(|home| home.join(".config").join("gcloud"))
}

/// Credentials read from a file, ready to be exchanged for an access token.
#[derive(Debug)]
pub enum GoogleCredentials {
    /// A user's refresh token, from `gcloud auth application-default login`.
    User(UserCredentials),
    /// A service account's private key, from a key file.
    ServiceAccount(Box<ServiceAccountKey>),
}

#[derive(Debug)]
pub struct UserCredentials {
    client_id: String,
    client_secret: Password,
    refresh_token: Password,
    token_uri: String,
}

#[derive(Debug)]
pub struct ServiceAccountKey {
    client_email: String,
    private_key_id: Option<String>,
    /// Its `Debug` shows only the public half.
    key: RsaKeyPair,
    token_uri: String,
}

/// The fields of every kind of credentials file DbRover reads; which are needed depends on
/// `type`.
#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "type")]
    kind: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    client_email: Option<String>,
    private_key: Option<String>,
    private_key_id: Option<String>,
    token_uri: Option<String>,
}

impl GoogleCredentials {
    fn parse(path: &Path, text: &str) -> Result<Self, GoogleError> {
        let malformed = |reason: String| GoogleError::Malformed {
            path: path.to_owned(),
            reason,
        };
        let file: CredentialsFile =
            serde_json::from_str(text).map_err(|error| malformed(error.to_string()))?;
        let required = |field: Option<String>, name: &str| {
            field
                .filter(|value| !value.is_empty())
                .ok_or_else(|| malformed(format!("it has no {name}")))
        };
        let token_uri = file.token_uri.unwrap_or_else(|| TOKEN_ENDPOINT.to_owned());
        match file.kind.as_str() {
            "authorized_user" => Ok(Self::User(UserCredentials {
                client_id: required(file.client_id, "client_id")?,
                client_secret: Password::new(required(file.client_secret, "client_secret")?),
                refresh_token: Password::new(required(file.refresh_token, "refresh_token")?),
                token_uri,
            })),
            "service_account" => {
                let pem = required(file.private_key, "private_key")?;
                let key = private_key(&pem).map_err(|reason| GoogleError::Key {
                    path: path.to_owned(),
                    reason,
                })?;
                Ok(Self::ServiceAccount(Box::new(ServiceAccountKey {
                    client_email: required(file.client_email, "client_email")?,
                    private_key_id: file.private_key_id,
                    key,
                    token_uri,
                })))
            }
            kind => Err(GoogleError::Unsupported {
                path: path.to_owned(),
                kind: kind.to_owned(),
            }),
        }
    }

    /// Exchanges the credentials for an access token at Google's token endpoint.
    ///
    /// # Errors
    ///
    /// [`GoogleError::Refused`] with Google's reason, or [`GoogleError::Http`] when the
    /// endpoint can't be reached.
    pub async fn token(&self) -> Result<AccessToken, GoogleError> {
        let https = Https::new()?;
        let asked_at = SystemTime::now();
        let answer = match self {
            Self::User(user) => {
                https
                    .post_form(
                        &user.token_uri,
                        &[
                            ("grant_type", "refresh_token"),
                            ("client_id", &user.client_id),
                            ("client_secret", user.client_secret.expose()),
                            ("refresh_token", user.refresh_token.expose()),
                        ],
                    )
                    .await?
            }
            Self::ServiceAccount(account) => {
                let assertion = account.assertion(asked_at)?;
                https
                    .post_form(
                        &account.token_uri,
                        &[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                            ("assertion", &assertion),
                        ],
                    )
                    .await?
            }
        };
        read_token(&answer, asked_at, matches!(self, Self::User(_)))
    }
}

impl ServiceAccountKey {
    /// The JWT a service account signs to ask for a token, as Google's OAuth 2.0 for
    /// service accounts describes it: RS256, issued by the account, for the token endpoint.
    fn assertion(&self, now: SystemTime) -> Result<String, GoogleError> {
        let issued_at = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
        if let Some(id) = &self.private_key_id {
            header["kid"] = serde_json::Value::from(id.as_str());
        }
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": SCOPES,
            "aud": self.token_uri,
            "iat": issued_at,
            "exp": issued_at + ASSERTION_LIFETIME.as_secs(),
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut signature,
            )
            .map_err(|_| GoogleError::Signing)?;
        Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

/// An RSA key in PKCS #8 PEM, as service account key files hold it.
fn private_key(pem: &str) -> Result<RsaKeyPair, String> {
    let base64: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(base64)
        .map_err(|error| format!("it isn't PEM: {error}"))?;
    RsaKeyPair::from_pkcs8(&der).map_err(|error| format!("it isn't an RSA key: {error}"))
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct ErrorAnswer {
    error: String,
    error_description: Option<String>,
}

/// The token in the endpoint's answer, or why it refused, with what to do about a user's
/// sign-in that has expired or been revoked.
fn read_token(
    answer: &Answer,
    asked_at: SystemTime,
    user: bool,
) -> Result<AccessToken, GoogleError> {
    if answer.status == 200 {
        let token: TokenAnswer = serde_json::from_slice(&answer.body)
            .map_err(|error| GoogleError::Refused(format!("its answer can't be read: {error}")))?;
        return Ok(AccessToken {
            secret: Password::new(token.access_token),
            expires_at: asked_at + Duration::from_secs(token.expires_in),
        });
    }
    let Ok(refusal) = serde_json::from_slice::<ErrorAnswer>(&answer.body) else {
        return Err(GoogleError::Refused(format!(
            "HTTP {}: {}",
            answer.status,
            String::from_utf8_lossy(&answer.body).trim()
        )));
    };
    let mut reason = match refusal.error_description {
        Some(description) => format!("{} ({description})", refusal.error),
        None => refusal.error,
    };
    if user && reason.starts_with("invalid_grant") {
        reason.push_str("; run `gcloud auth application-default login` again");
    }
    Err(GoogleError::Refused(reason))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;
    use crate::http::stand_in;

    /// A key made for these tests with `openssl genpkey -algorithm RSA`, which signs nothing
    /// anywhere else.
    const TEST_KEY: &str = include_str!("../testdata/service-account-key.pem");

    /// Variables of the tests' own, and never the machine's.
    struct Fake {
        variables: HashMap<&'static str, String>,
        home: Option<PathBuf>,
    }

    impl Variables for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.variables.get(name).cloned()
        }

        fn home(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    fn user_file(token_uri: &str) -> String {
        serde_json::json!({
            "type": "authorized_user",
            "client_id": "client.apps.googleusercontent.com",
            "client_secret": "client-secret",
            "refresh_token": "refresh-token",
            "token_uri": token_uri,
        })
        .to_string()
    }

    fn service_account_file(token_uri: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "client_email": "reader@project.iam.gserviceaccount.com",
            "private_key_id": "key-1",
            "private_key": TEST_KEY,
            "token_uri": token_uri,
        })
        .to_string()
    }

    fn parse(text: &str) -> Result<GoogleCredentials, GoogleError> {
        GoogleCredentials::parse(Path::new("adc.json"), text)
    }

    const TOKEN_ANSWER: &str =
        r#"{"access_token":"ya29.token","expires_in":3599,"token_type":"Bearer"}"#;

    #[tokio::test]
    async fn a_users_refresh_token_is_exchanged_for_an_access_token() {
        let mut endpoint = stand_in::token_endpoint(200, TOKEN_ANSWER).await.unwrap();
        let before = SystemTime::now();
        let credentials = parse(&user_file(&endpoint.address)).unwrap();
        let token = credentials.token().await.unwrap();
        assert_eq!(token.secret.expose(), "ya29.token");
        let lasts = token.expires_at.duration_since(before).unwrap();
        assert!((3590..=3600).contains(&lasts.as_secs()), "{lasts:?}");
        let received = endpoint.received.recv().await.unwrap();
        assert_eq!(received.field("grant_type"), Some("refresh_token"));
        assert_eq!(received.field("refresh_token"), Some("refresh-token"));
        assert_eq!(received.field("client_secret"), Some("client-secret"));
        assert_eq!(
            received.field("client_id"),
            Some("client.apps.googleusercontent.com")
        );
    }

    #[tokio::test]
    async fn a_service_account_signs_its_request_with_its_key() {
        let mut endpoint = stand_in::token_endpoint(200, TOKEN_ANSWER).await.unwrap();
        let credentials = parse(&service_account_file(&endpoint.address)).unwrap();
        let token = credentials.token().await.unwrap();
        assert_eq!(token.secret.expose(), "ya29.token");

        let received = endpoint.received.recv().await.unwrap();
        assert_eq!(
            received.field("grant_type"),
            Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
        );
        let assertion = received.field("assertion").unwrap();
        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3, "{assertion}");
        let decode = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        let header = decode(parts[0]);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], "key-1");
        let claims = decode(parts[1]);
        assert_eq!(claims["iss"], "reader@project.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], endpoint.address.as_str());
        assert_eq!(claims["scope"], SCOPES);
        let lifetime = claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap();
        assert_eq!(lifetime, 3600);

        let GoogleCredentials::ServiceAccount(account) = credentials else {
            panic!("not a service account");
        };
        let public = UnparsedPublicKey::new(
            &RSA_PKCS1_2048_8192_SHA256,
            account.key.public().as_ref().to_vec(),
        );
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let signed = format!("{}.{}", parts[0], parts[1]);
        public.verify(signed.as_bytes(), &signature).unwrap();
    }

    #[tokio::test]
    async fn an_expired_user_sign_in_says_to_sign_in_again() {
        let endpoint = stand_in::token_endpoint(
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
        .await
        .unwrap();
        let error = parse(&user_file(&endpoint.address))
            .unwrap()
            .token()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Token has been expired or revoked"),
            "{error}"
        );
        assert!(
            error.contains("gcloud auth application-default login"),
            "{error}"
        );
    }

    #[test]
    fn files_dbrover_cant_use_say_why() {
        let federation = r#"{"type":"external_account","audience":"x"}"#;
        let no_token = r#"{"type":"authorized_user","client_id":"a","client_secret":"b"}"#;
        let bad_key = r#"{"type":"service_account","client_email":"a","private_key":"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"}"#;
        for (text, expected) in [
            (federation, "external_account credentials"),
            (no_token, "no refresh_token"),
            (bad_key, "isn't an RSA key"),
            ("[]", "isn't a Google credentials file"),
        ] {
            let error = parse(text).unwrap_err().to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    #[test]
    fn credentials_are_found_where_googles_libraries_look() {
        let folder = tempfile::tempdir().unwrap();
        let gcloud = folder.path().join(".config").join("gcloud");
        std::fs::create_dir_all(&gcloud).unwrap();
        let key_file = folder.path().join("key.json");
        std::fs::write(&key_file, service_account_file(TOKEN_ENDPOINT)).unwrap();
        let fake = |variables: &[(&'static str, &Path)]| Fake {
            variables: variables
                .iter()
                .map(|(name, path)| (*name, path.display().to_string()))
                .collect(),
            home: Some(folder.path().to_owned()),
        };
        let sign_in = GoogleSignIn::default();

        // Nothing written yet, and no variable.
        let error = sign_in.credentials(&fake(&[])).unwrap_err();
        assert!(matches!(error, GoogleError::NoCredentials), "{error}");

        let named = sign_in.credentials(&fake(&[(CREDENTIALS_VARIABLE, &key_file)]));
        assert!(matches!(named, Ok(GoogleCredentials::ServiceAccount(_))));

        if !cfg!(windows) {
            std::fs::write(gcloud.join(WELL_KNOWN_FILE), user_file(TOKEN_ENDPOINT)).unwrap();
            let gcloud_default = sign_in.credentials(&fake(&[]));
            assert!(matches!(gcloud_default, Ok(GoogleCredentials::User(_))));
        }

        let elsewhere = folder.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join(WELL_KNOWN_FILE), user_file(TOKEN_ENDPOINT)).unwrap();
        let configured = sign_in.credentials(&fake(&[("CLOUDSDK_CONFIG", &elsewhere)]));
        assert!(matches!(configured, Ok(GoogleCredentials::User(_))));

        let own_file = GoogleSignIn {
            credentials_file: Some(key_file),
        };
        assert!(matches!(
            own_file.credentials(&fake(&[])),
            Ok(GoogleCredentials::ServiceAccount(_))
        ));
    }

    #[test]
    fn debug_shows_no_secret() {
        let shown = format!("{:?}", parse(&user_file(TOKEN_ENDPOINT)).unwrap());
        assert!(!shown.contains("refresh-token"), "{shown}");
        assert!(!shown.contains("client-secret"), "{shown}");
    }
}
