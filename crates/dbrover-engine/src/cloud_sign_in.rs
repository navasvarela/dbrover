//! A database password from a cloud account (C0d, `docs/cloud-data.md`): an RDS IAM token, a
//! Google Cloud token or an Entra ID token, fetched on every connection from the sign-in
//! already on the machine, and never written anywhere.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use dbrover_cloud::TokenCache;
use dbrover_cloud::aws::{AwsCredentials, AwsError, AwsSignIn};
use dbrover_cloud::entra::{EntraError, EntraResource, EntraSignIn, EntraTool};
use dbrover_cloud::google::{GoogleError, GoogleSignIn};
use dbrover_cloud::rds::{self, RdsDatabase};
use dbrover_core::{AzureTool, CloudSignIn, DriverError, DriverKind, Password, ServerConfig};

use crate::environment::ProcessEnvironment;

#[derive(Debug, thiserror::Error)]
pub enum CloudSignInError {
    #[error(
        "the data source has both a password command and a cloud sign-in; keep one of them in \
         its settings"
    )]
    Both,
    #[error(transparent)]
    Aws(#[from] AwsError),
    #[error(transparent)]
    Google(#[from] GoogleError),
    #[error(transparent)]
    Entra(#[from] EntraError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// What a token is cached for: Entra ID hands out a token per resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenKey {
    sign_in: CloudSignIn,
    resource: Option<EntraResource>,
}

/// The tokens fetched for each sign-in, kept for the app's lifetime so that every console
/// opened in a row doesn't run `az` or ask Google again.
///
/// Process-wide rather than passed to every place that opens a connection: a token depends
/// only on the sign-in's settings, as the CLIs' own caches do, and the MCP server, the
/// connection test and sessions all open connections through `open::open`.
static TOKENS: LazyLock<Mutex<HashMap<TokenKey, Arc<TokenCache>>>> = LazyLock::new(Mutex::default);

fn cache_for(key: TokenKey) -> Arc<TokenCache> {
    let mut tokens = TOKENS.lock().unwrap_or_else(PoisonError::into_inner);
    tokens.entry(key).or_default().clone()
}

/// The password `sign_in` gives for `server`, on an engine of `driver`.
///
/// # Errors
///
/// [`CloudSignInError`] with the provider's reason when no token can be had.
pub(crate) async fn password(
    sign_in: &CloudSignIn,
    server: &ServerConfig,
    driver: DriverKind,
) -> Result<Password, CloudSignInError> {
    let token = match sign_in {
        CloudSignIn::Aws { profile, region } => {
            let aws = AwsSignIn {
                credentials: match profile.as_str() {
                    "" => AwsCredentials::DefaultChain,
                    profile => AwsCredentials::Profile(profile.to_owned()),
                },
                region: Some(region.clone()).filter(|region| !region.is_empty()),
                endpoint: None,
            };
            let config = aws.sdk_config().await?;
            let database = RdsDatabase {
                host: &server.host,
                port: server.port_number()?,
                user: &server.user,
            };
            // Made on the machine in microseconds, from credentials AWS's SDK caches itself.
            rds::auth_token(&config, &database).await?
        }
        CloudSignIn::Google { credentials_file } => {
            let google = GoogleSignIn {
                credentials_file: credentials_file.clone(),
            };
            google.cached_token(&ProcessEnvironment).await?
        }
        CloudSignIn::Entra { tool, tenant } => {
            let entra = EntraSignIn {
                tool: match tool {
                    AzureTool::AzureCli => EntraTool::AzureCli,
                    AzureTool::AzureDeveloperCli => EntraTool::AzureDeveloperCli,
                },
                tenant: Some(tenant.clone()).filter(|tenant| !tenant.is_empty()),
            };
            let resource = match driver {
                DriverKind::SqlServer => EntraResource::AzureSql,
                _ => EntraResource::AzureDatabase,
            };
            let key = TokenKey {
                sign_in: sign_in.clone(),
                resource: Some(resource),
            };
            cache_for(key)
                .token(|| async { Ok::<_, CloudSignInError>(entra.token(resource).await?) })
                .await?
        }
    };
    Ok(token.secret)
}
