//! Signing in to AWS through AWS's own SDK, `aws-config`, with the profile and region a data
//! source names. DbRover reads no secret itself: the SDK reads the shared `config` and
//! `credentials` files, IAM Identity Center's cache, `aws login`'s and the environment, and
//! refreshes what expires. Only the profile's and region's names are kept in DbRover's files.

use std::error::Error;

use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::provider_config::ProviderConfig;
use aws_config::{BehaviorVersion, ConfigLoader, Region};
use aws_credential_types::Credentials;
use aws_credential_types::provider::ProvideCredentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_runtime::env_config::file::EnvConfigFiles;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};

pub use aws_config::SdkConfig;

/// The made-up credentials of [`AwsCredentials::LocalEmulator`]. DynamoDB Local and LocalStack
/// take any; DynamoDB Local keeps a database per access key, so it stays the same.
const EMULATOR_KEY: &str = "dbrover";

/// The region an emulator is told when the data source names none: DynamoDB Local keeps a
/// database per region too, and AWS's tools default to this one.
const EMULATOR_REGION: &str = "us-east-1";

/// Where a data source's AWS calls go, and who they are signed as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsSignIn {
    pub credentials: AwsCredentials,
    /// `None` for the profile's or the environment's region.
    pub region: Option<String>,
    /// An address instead of AWS's own for the service: DynamoDB Local, LocalStack or a VPC
    /// endpoint.
    pub endpoint: Option<String>,
}

/// Who an AWS data source signs in as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwsCredentials {
    /// AWS's default chain: the environment's keys, then the `AWS_PROFILE` or `default` profile,
    /// web identity, and a container's or instance's role.
    DefaultChain,
    /// One profile of the shared files, alone, as the AWS CLI's `--profile` reads it: keys in
    /// the environment don't override the choice. IAM Identity Center, `aws login`, roles to
    /// assume and `credential_process` all work through it.
    Profile(String),
    /// Made-up credentials for DynamoDB Local or LocalStack, which take any and which AWS
    /// never would. Only with an endpoint.
    LocalEmulator,
}

impl AwsCredentials {
    /// Why no credentials came from this sign-in, and where to look.
    fn not_found(&self) -> String {
        match self {
            Self::DefaultChain => "no credentials were found in the environment, the default \
                 profile or a role; name a profile, or sign in with `aws login` or \
                 `aws sso login`"
                .to_owned(),
            Self::Profile(name) => format!(
                "the profile {name} isn't in the AWS config or credentials files, or holds no \
                 credentials"
            ),
            Self::LocalEmulator => "the emulator's credentials weren't taken".to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AwsError {
    #[error("A local emulator signs in only with its endpoint; set the data source's endpoint")]
    EmulatorWithoutEndpoint,
    #[error("No AWS region: set one on the data source, in the profile, or in AWS_REGION")]
    NoRegion,
    /// The SDK's own message, with each of its causes.
    #[error("Couldn't sign in to AWS: {0}")]
    Credentials(String),
}

impl AwsSignIn {
    /// The SDK's configuration for this sign-in, with credentials checked: a missing profile or
    /// an expired IAM Identity Center session is an error here rather than on the first call.
    ///
    /// # Errors
    ///
    /// [`AwsError`] when the sign-in can't be used, saying why.
    pub async fn sdk_config(&self) -> Result<SdkConfig, AwsError> {
        self.load(self.loader(EnvConfigFiles::default())?).await
    }

    async fn load(&self, loader: ConfigLoader) -> Result<SdkConfig, AwsError> {
        let config = loader.load().await;
        if config.region().is_none() {
            return Err(AwsError::NoRegion);
        }
        self.check_credentials(&config).await?;
        Ok(config)
    }

    /// Asks for credentials once, so that a sign-in that can't give any fails here, saying
    /// which.
    async fn check_credentials(&self, config: &SdkConfig) -> Result<(), AwsError> {
        let error = match config.credentials_provider() {
            None => return Err(AwsError::Credentials(self.credentials.not_found())),
            Some(provider) => match provider.provide_credentials().await {
                Ok(_) => return Ok(()),
                Err(error) => error,
            },
        };
        // A chain that found nothing to read says only that it wasn't enabled.
        let message = match (&error, error.source()) {
            (CredentialsError::CredentialsNotLoaded(_), None) => self.credentials.not_found(),
            _ => with_causes(&error),
        };
        Err(AwsError::Credentials(message))
    }

    fn loader(&self, files: EnvConfigFiles) -> Result<ConfigLoader, AwsError> {
        // The SDK's HTTPS client, on rustls with ring: the workspace builds aws-lc-rs as well
        // (for `tiberius`), so rustls can't choose a provider by itself, and the client names it.
        let https = aws_smithy_http_client::Builder::new()
            .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .http_client(https.clone())
            .profile_files(files.clone());
        let mut region = self.region.clone();
        match &self.credentials {
            AwsCredentials::DefaultChain => {}
            AwsCredentials::Profile(name) => {
                let provider = ProviderConfig::default()
                    .with_http_client(https)
                    .with_region(region.clone().map(Region::new));
                loader = loader.profile_name(name).credentials_provider(
                    ProfileFileCredentialsProvider::builder()
                        .configure(&provider)
                        .profile_files(files)
                        .profile_name(name)
                        .build(),
                );
            }
            AwsCredentials::LocalEmulator => {
                if self.endpoint.is_none() {
                    return Err(AwsError::EmulatorWithoutEndpoint);
                }
                region = region.or_else(|| Some(EMULATOR_REGION.to_owned()));
                loader = loader.credentials_provider(Credentials::new(
                    EMULATOR_KEY,
                    EMULATOR_KEY,
                    None,
                    None,
                    "local-emulator",
                ));
            }
        }
        if let Some(region) = region {
            loader = loader.region(Region::new(region));
        }
        if let Some(endpoint) = &self.endpoint {
            loader = loader.endpoint_url(endpoint);
        }
        Ok(loader)
    }
}

/// `error`'s message followed by each of its causes', as the SDK nests them: its own message
/// alone often says only that a provider failed.
fn with_causes(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use aws_runtime::env_config::file::EnvConfigFileKind;
    use aws_types::os_shim_internal::Env;

    use super::*;

    /// Shared files written for the test, and only those: never the user's own.
    fn files(directory: &Path, config: &str, credentials: &str) -> EnvConfigFiles {
        let config_path = directory.join("config");
        let credentials_path = directory.join("credentials");
        std::fs::write(&config_path, config).unwrap();
        std::fs::write(&credentials_path, credentials).unwrap();
        EnvConfigFiles::builder()
            .with_file(EnvConfigFileKind::Config, config_path)
            .with_file(EnvConfigFileKind::Credentials, credentials_path)
            .build()
    }

    impl AwsSignIn {
        /// [`AwsSignIn::sdk_config`] reading only `files`, and an empty environment.
        async fn load_only(&self, files: EnvConfigFiles) -> Result<SdkConfig, AwsError> {
            let loader = self.loader(files)?.env(Env::from_slice(&[]));
            self.load(loader).await
        }
    }

    fn sign_in(credentials: AwsCredentials) -> AwsSignIn {
        AwsSignIn {
            credentials,
            region: None,
            endpoint: None,
        }
    }

    async fn access_key(config: &SdkConfig) -> String {
        let provider = config.credentials_provider().unwrap();
        let credentials = provider.provide_credentials().await.unwrap();
        credentials.access_key_id().to_owned()
    }

    #[tokio::test]
    async fn a_profile_signs_in_with_its_own_keys_and_region() {
        let directory = tempfile::tempdir().unwrap();
        let files = files(
            directory.path(),
            "[default]\nregion = eu-west-1\n\n[profile analytics]\nregion = ap-south-1\n",
            "[default]\naws_access_key_id = DEFAULTKEY\naws_secret_access_key = s\n\n\
             [analytics]\naws_access_key_id = ANALYTICSKEY\naws_secret_access_key = s\n",
        );
        let config = sign_in(AwsCredentials::Profile("analytics".into()))
            .load_only(files)
            .await
            .unwrap();
        assert_eq!(config.region().map(Region::as_ref), Some("ap-south-1"));
        assert_eq!(access_key(&config).await, "ANALYTICSKEY");
    }

    #[tokio::test]
    async fn the_data_sources_region_wins_over_the_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let files = files(
            directory.path(),
            "[profile analytics]\nregion = ap-south-1\n",
            "[analytics]\naws_access_key_id = K\naws_secret_access_key = s\n",
        );
        let mut sign_in = sign_in(AwsCredentials::Profile("analytics".into()));
        sign_in.region = Some("eu-central-1".into());
        let config = sign_in.load_only(files).await.unwrap();
        assert_eq!(config.region().map(Region::as_ref), Some("eu-central-1"));
    }

    #[tokio::test]
    async fn a_missing_profile_is_an_error_naming_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut sign_in = sign_in(AwsCredentials::Profile("nowhere".into()));
        sign_in.region = Some("eu-west-1".into());
        // With no profile at all, and with others but not this one.
        for config in ["", "[profile elsewhere]\nregion = eu-west-1\n"] {
            let files = files(directory.path(), config, "");
            let error = sign_in.load_only(files).await.unwrap_err();
            assert!(
                matches!(&error, AwsError::Credentials(message) if message.contains("nowhere")),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn an_emulator_signs_in_with_made_up_keys_only_with_an_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let mut emulator = sign_in(AwsCredentials::LocalEmulator);
        let error = emulator
            .load_only(files(directory.path(), "", ""))
            .await
            .unwrap_err();
        assert!(
            matches!(error, AwsError::EmulatorWithoutEndpoint),
            "{error}"
        );
        emulator.endpoint = Some("http://localhost:8000".into());
        let config = emulator
            .load_only(files(directory.path(), "", ""))
            .await
            .unwrap();
        assert_eq!(config.region().map(Region::as_ref), Some(EMULATOR_REGION));
        assert_eq!(config.endpoint_url(), Some("http://localhost:8000"));
        assert_eq!(access_key(&config).await, EMULATOR_KEY);
    }

    /// aws-lc-rs is in the workspace's build for `tiberius`, but no AWS crate may bring it:
    /// built on its own, this crate has ring alone (`docs/nosql.md`, N5).
    #[test]
    fn no_aws_crate_brings_aws_lc_rs() {
        let output = std::process::Command::new(env!("CARGO"))
            .args([
                "tree",
                "--offline",
                "-p",
                "dbrover-cloud",
                "-e",
                "normal",
                "-i",
                "aws-lc-rs",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        let tree = String::from_utf8_lossy(&output.stdout);
        let refusal = String::from_utf8_lossy(&output.stderr);
        assert!(
            tree.trim().is_empty(),
            "aws-lc-rs is built for dbrover-cloud:\n{tree}"
        );
        // `cargo tree -i` of a package not in the build refuses to name it.
        assert!(
            refusal.contains("did not match any packages"),
            "cargo tree didn't run as expected: {refusal}"
        );
    }
}
