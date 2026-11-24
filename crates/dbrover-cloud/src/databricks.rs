//! Signing in to Databricks (`docs/cloud-data.md`, C6c): every request carries a bearer token,
//! which is a personal access token kept as the data source's password, a service principal's
//! OAuth token from the workspace's own token endpoint, the token the Databricks CLI prints
//! after the user's `databricks auth login`, or an Entra ID token for Azure Databricks.

use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use dbrover_core::Password;
use serde::Deserialize;

use crate::entra::{EntraError, EntraResource, EntraSignIn};
use crate::http::{HttpError, Https};
use crate::program::{self, Invocation, ProgramError};
use crate::token::{AccessToken, TokenCache};

/// Where a workspace hands out OAuth tokens, below its address.
const TOKEN_PATH: &str = "/oidc/v1/token";

/// The scope Databricks's documentation asks a service principal's token for: every API the
/// principal is allowed.
const ALL_APIS: &str = "all-apis";

/// The Databricks CLI's name, run without a shell. On Windows it is `databricks.exe`, which
/// Windows finds by this name, unlike the Azure CLI's batch file.
const CLI: &str = "databricks";

/// What the user runs once so the Databricks CLI can hand out tokens.
const CLI_SIGN_IN: &str = "databricks auth login --host <workspace>";

/// How long a service principal's token lasts when the answer doesn't say: Databricks's are
/// an hour.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum DatabricksError {
    #[error("Couldn't reach the workspace's token endpoint: {0}")]
    Http(#[from] HttpError),
    #[error("The workspace refused the service principal (HTTP {status}): {message}")]
    Refused { status: u16, message: String },
    #[error("The workspace's token can't be read: {0}")]
    Unreadable(String),
    #[error(
        "The Databricks CLI gave no token ({status}): {stderr}. Sign in once with `{CLI_SIGN_IN}`"
    )]
    CliFailed { status: String, stderr: String },
    #[error("The Databricks CLI's token can't be read: {0}")]
    CliUnreadable(String),
    #[error(transparent)]
    Program(#[from] ProgramError),
    #[error(transparent)]
    Entra(#[from] EntraError),
}

/// How a Databricks data source signs in, once its secrets are in hand.
#[derive(Debug)]
pub enum DatabricksSignIn {
    /// A personal access token, a user's or a service principal's, sent as it is.
    Token(Password),
    /// A service principal's OAuth secret, exchanged for an hour's token. Boxed: the client
    /// and its cache are larger than a token.
    ServicePrincipal(Box<ServicePrincipalSignIn>),
    /// The token the Databricks CLI prints for the workspace.
    DatabricksCli(CliSignIn),
    /// An Entra ID token for Azure Databricks, from the Azure CLI's sign-in.
    Entra(EntraDatabricksSignIn),
}

impl DatabricksSignIn {
    /// The bearer token for the next request.
    ///
    /// # Errors
    ///
    /// [`DatabricksError`] when a token can't be had: the workspace refused the service
    /// principal, or a CLI isn't installed, isn't signed in, or answered with something else.
    pub async fn bearer(&self) -> Result<Password, DatabricksError> {
        match self {
            Self::Token(token) => Ok(token.clone()),
            Self::ServicePrincipal(sign_in) => Ok(sign_in.token().await?.secret),
            Self::DatabricksCli(sign_in) => Ok(sign_in.token().await?.secret),
            Self::Entra(sign_in) => Ok(sign_in.token().await?.secret),
        }
    }
}

/// A service principal's OAuth machine-to-machine sign-in: its client id and secret posted to
/// the workspace's `/oidc/v1/token` for a token of every API it is allowed.
pub struct ServicePrincipalSignIn {
    https: Https,
    token_address: String,
    client_id: String,
    secret: Password,
    cache: TokenCache,
}

/// Names the client and where it signs in; the secret and the cached token never print.
impl std::fmt::Debug for ServicePrincipalSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServicePrincipalSignIn")
            .field("token_address", &self.token_address)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// The token endpoint's answer, as OAuth 2.0 writes it.
#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    expires_in: Option<u64>,
}

/// An OAuth error's body: `{"error": "invalid_client", "error_description": "…"}`.
#[derive(Deserialize)]
struct OAuthError {
    error: Option<String>,
    error_description: Option<String>,
}

impl ServicePrincipalSignIn {
    /// A sign-in at `workspace` (`https://…`, no path) as the client `client_id`.
    ///
    /// # Errors
    ///
    /// When the system's root certificates can't be loaded.
    pub fn new(
        workspace: &str,
        client_id: &str,
        secret: Password,
    ) -> Result<Self, DatabricksError> {
        Ok(Self {
            https: Https::new()?,
            token_address: format!("{}{TOKEN_PATH}", workspace.trim_end_matches('/')),
            client_id: client_id.trim().to_owned(),
            secret,
            cache: TokenCache::default(),
        })
    }

    /// The cached token, or a new one from the workspace.
    ///
    /// # Errors
    ///
    /// [`DatabricksError`] when the endpoint can't be reached, refuses the client, or answers
    /// with something that isn't a token.
    pub async fn token(&self) -> Result<AccessToken, DatabricksError> {
        self.cache.token(|| self.fetch(SystemTime::now())).await
    }

    async fn fetch(&self, now: SystemTime) -> Result<AccessToken, DatabricksError> {
        let credentials = STANDARD.encode(format!("{}:{}", self.client_id, self.secret.expose()));
        let answer = self
            .https
            .post_form_with_basic(
                &self.token_address,
                &[("grant_type", "client_credentials"), ("scope", ALL_APIS)],
                &Password::new(credentials),
            )
            .await?;
        if answer.status != 200 {
            return Err(refusal(answer.status, &answer.body));
        }
        let token: TokenAnswer = serde_json::from_slice(&answer.body)
            .map_err(|error| DatabricksError::Unreadable(error.to_string()))?;
        let lifetime = token
            .expires_in
            .map_or(DEFAULT_LIFETIME, Duration::from_secs);
        Ok(AccessToken {
            secret: Password::new(token.access_token),
            expires_at: now + lifetime,
        })
    }
}

/// The endpoint's refusal, in its own words where its body has them.
fn refusal(status: u16, body: &[u8]) -> DatabricksError {
    let message = serde_json::from_slice::<OAuthError>(body)
        .ok()
        .and_then(|error| error.error_description.or(error.error))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_owned());
    DatabricksError::Refused { status, message }
}

/// The token the Databricks CLI prints for a workspace, after the user's own `databricks auth
/// login`: OAuth for a user, the browser's part done by the CLI, which refreshes the token
/// itself, so DbRover holds no refresh token.
#[derive(Debug)]
pub struct CliSignIn {
    workspace: String,
    cache: TokenCache,
}

/// `databricks auth token`'s answer.
#[derive(Deserialize)]
struct CliToken {
    access_token: String,
    /// RFC 3339, with a zone: `2026-11-24T16:00:00.123456+01:00`.
    expiry: String,
}

impl CliSignIn {
    /// The CLI's sign-in for `workspace` (`https://…`, no path).
    pub fn new(workspace: &str) -> Self {
        Self {
            workspace: workspace.trim_end_matches('/').to_owned(),
            cache: TokenCache::default(),
        }
    }

    /// The cached token, or a new one from the CLI.
    ///
    /// # Errors
    ///
    /// [`DatabricksError`] when the CLI isn't installed, isn't signed in to the workspace,
    /// runs past its time, or prints something other than a token.
    pub async fn token(&self) -> Result<AccessToken, DatabricksError> {
        self.cache.token(|| self.fetch()).await
    }

    fn invocation(&self) -> Invocation {
        Invocation {
            program: CLI,
            arguments: vec!["auth".into(), "token".into(), self.workspace.clone()],
        }
    }

    async fn fetch(&self) -> Result<AccessToken, DatabricksError> {
        let output = program::output(&self.invocation(), CLI_SIGN_IN).await?;
        if !output.status.success() {
            return Err(DatabricksError::CliFailed {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        read_cli_token(&output.stdout).map_err(DatabricksError::CliUnreadable)
    }
}

fn read_cli_token(stdout: &[u8]) -> Result<AccessToken, String> {
    let answer: CliToken = serde_json::from_slice(stdout).map_err(|error| error.to_string())?;
    let expires_at: jiff::Timestamp = answer
        .expiry
        .parse()
        .map_err(|error| format!("its expiry {:?}: {error}", answer.expiry))?;
    Ok(AccessToken {
        secret: Password::new(answer.access_token),
        expires_at: SystemTime::from(expires_at),
    })
}

/// An Entra ID token for Azure Databricks, from the Azure CLI's or the Azure Developer CLI's
/// sign-in (C0c), kept until five minutes before it expires.
#[derive(Debug)]
pub struct EntraDatabricksSignIn {
    sign_in: EntraSignIn,
    cache: TokenCache,
}

impl EntraDatabricksSignIn {
    pub fn new(sign_in: EntraSignIn) -> Self {
        Self {
            sign_in,
            cache: TokenCache::default(),
        }
    }

    /// The cached token, or a new one from the CLI.
    ///
    /// # Errors
    ///
    /// [`EntraError`] as [`EntraSignIn::token`] says.
    pub async fn token(&self) -> Result<AccessToken, DatabricksError> {
        Ok(self
            .cache
            .token(|| self.sign_in.token(EntraResource::AzureDatabricks))
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::stand_in;

    const SECRET: &str = "dose-secret-1234";

    /// The stand-in's address is `http://127.0.0.1:<port>/token`; the workspace is its origin.
    fn workspace(endpoint: &stand_in::TokenEndpoint) -> String {
        endpoint.address.trim_end_matches("/token").to_owned()
    }

    #[tokio::test]
    async fn a_service_principal_posts_its_client_and_reuses_the_token() {
        let mut endpoint = stand_in::token_endpoint(
            200,
            r#"{"access_token":"eyJ-principal","token_type":"Bearer","expires_in":3600,"scope":"all-apis"}"#,
        )
        .await
        .unwrap();
        let sign_in = DatabricksSignIn::ServicePrincipal(Box::new(
            ServicePrincipalSignIn::new(
                &workspace(&endpoint),
                "a1b2-client",
                Password::new(SECRET),
            )
            .unwrap(),
        ));
        assert_eq!(sign_in.bearer().await.unwrap().expose(), "eyJ-principal");
        assert_eq!(sign_in.bearer().await.unwrap().expose(), "eyJ-principal");

        let received = endpoint.received.recv().await.unwrap();
        assert_eq!(received.request_line, "POST /oidc/v1/token HTTP/1.1");
        assert_eq!(received.field("grant_type"), Some("client_credentials"));
        assert_eq!(received.field("scope"), Some("all-apis"));
        let expected = format!("Basic {}", STANDARD.encode(format!("a1b2-client:{SECRET}")));
        assert_eq!(received.header("authorization"), Some(expected.as_str()));
        // An hour's token is asked for once.
        assert!(endpoint.received.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_refused_service_principal_says_why_in_the_workspaces_words() {
        let endpoint = stand_in::token_endpoint(
            401,
            r#"{"error":"invalid_client","error_description":"Client authentication failed"}"#,
        )
        .await
        .unwrap();
        let sign_in = ServicePrincipalSignIn::new(
            &workspace(&endpoint),
            "a1b2-client",
            Password::new(SECRET),
        )
        .unwrap();
        let error = sign_in.token().await.unwrap_err().to_string();
        assert!(error.contains("HTTP 401"), "{error}");
        assert!(error.contains("Client authentication failed"), "{error}");
        assert!(!error.contains(SECRET), "{error}");
    }

    #[test]
    fn the_cli_is_asked_for_the_workspaces_token_without_a_shell() {
        let sign_in = CliSignIn::new("https://dbc-a1b2c3d4-e5f6.cloud.databricks.com/");
        let invocation = sign_in.invocation();
        assert_eq!(invocation.program, CLI);
        assert_eq!(
            invocation.arguments,
            [
                "auth",
                "token",
                "https://dbc-a1b2c3d4-e5f6.cloud.databricks.com"
            ]
        );
    }

    #[test]
    fn the_clis_answer_becomes_a_token() {
        let answer = br#"{"access_token":"eyJ-user","token_type":"Bearer","expiry":"2026-11-24T16:00:00.123456+01:00"}"#;
        let token = read_cli_token(answer).unwrap();
        assert_eq!(token.secret.expose(), "eyJ-user");
        let expected: jiff::Timestamp = "2026-11-24T15:00:00.123456Z".parse().unwrap();
        assert_eq!(token.expires_at, SystemTime::from(expected));
        for (answer, expected) in [
            (
                &br#"{"access_token":"eyJ-user"}"#[..],
                "missing field `expiry`",
            ),
            (b"Error: cannot fetch credentials", "expected value"),
        ] {
            let error = read_cli_token(answer).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn no_sign_in_prints_its_secret() {
        let token = DatabricksSignIn::Token(Password::new(SECRET));
        let principal = DatabricksSignIn::ServicePrincipal(Box::new(
            ServicePrincipalSignIn::new(
                "https://example.cloud.databricks.com",
                "id",
                Password::new(SECRET),
            )
            .unwrap(),
        ));
        for sign_in in [token, principal] {
            let printed = format!("{sign_in:?}");
            assert!(!printed.contains(SECRET), "{printed}");
        }
    }
}
