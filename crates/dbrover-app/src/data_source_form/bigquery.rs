//! A BigQuery data source's fields: how it signs in to Google Cloud (C0b), the project its jobs
//! run in, and the byte limit its dry runs are judged against (C4). Nothing secret is typed
//! here.

use std::path::PathBuf;

use dbrover_core::{BigQueryConfig, ByteCount, CostGuard, GoogleCloudSignIn};

use super::{FormError, byte_limit};

/// The limit's field in the dialog, which its errors name.
pub const LIMIT_LABEL: &str = "Ask above";

/// What a new data source asks above: 1 GB, about $0.006 at on-demand prices, so that a first
/// `SELECT *` on a large public table asks before it runs (`docs/cloud-data.md`, C4).
const NEW_LIMIT: ByteCount = ByteCount(1_000_000_000);

/// How the data source signs in, as the dialog offers it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BigQuerySignIn {
    /// Application Default Credentials, as `gcloud auth application-default login` writes them.
    #[default]
    Google,
    CredentialsFile,
    /// No token, for the emulator at an endpoint.
    Emulator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigQueryFields {
    pub sign_in: BigQuerySignIn,
    pub credentials_file: String,
    pub project: String,
    pub dataset: String,
    pub location: String,
    /// The other projects to show, separated by commas.
    pub other_projects: String,
    pub endpoint: String,
    /// In gigabytes (10⁹ bytes); empty for no limit. Saved as the cost guard's
    /// `ask_above_bytes`.
    pub limit_gb: String,
    pub cost_guard: CostGuard,
}

impl Default for BigQueryFields {
    fn default() -> Self {
        Self::from_config(&BigQueryConfig {
            cost_guard: CostGuard {
                ask_above_bytes: Some(NEW_LIMIT),
                ..CostGuard::default()
            },
            ..BigQueryConfig::default()
        })
    }
}

impl BigQueryFields {
    pub fn from_config(config: &BigQueryConfig) -> Self {
        let (sign_in, credentials_file) = match &config.sign_in {
            GoogleCloudSignIn::Google => (BigQuerySignIn::Google, String::new()),
            GoogleCloudSignIn::CredentialsFile { credentials_file } => (
                BigQuerySignIn::CredentialsFile,
                credentials_file.display().to_string(),
            ),
            GoogleCloudSignIn::Anonymous => (BigQuerySignIn::Emulator, String::new()),
        };
        Self {
            sign_in,
            credentials_file,
            project: config.project.clone(),
            dataset: config.dataset.clone(),
            location: config.location.clone(),
            other_projects: config.other_projects.join(", "),
            endpoint: config.endpoint.clone(),
            limit_gb: config
                .cost_guard
                .ask_above_bytes
                .map(byte_limit::gigabytes)
                .unwrap_or_default(),
            cost_guard: config.cost_guard,
        }
    }

    /// # Errors
    ///
    /// [`FormError::ProjectMissing`] without a project, [`FormError::CredentialsFileMissing`]
    /// and [`FormError::EndpointMissing`] for a sign-in without its file or endpoint, and
    /// [`FormError::InvalidByteLimit`] for a limit that isn't a number of gigabytes.
    pub fn to_config(&self) -> Result<BigQueryConfig, FormError> {
        let project = match self.project.trim() {
            "" => return Err(FormError::ProjectMissing),
            project => project.to_owned(),
        };
        let endpoint = self.endpoint.trim().to_owned();
        let sign_in = match self.sign_in {
            BigQuerySignIn::Google => GoogleCloudSignIn::Google,
            BigQuerySignIn::CredentialsFile => match self.credentials_file.trim() {
                "" => return Err(FormError::CredentialsFileMissing),
                file => GoogleCloudSignIn::CredentialsFile {
                    credentials_file: PathBuf::from(file),
                },
            },
            BigQuerySignIn::Emulator if endpoint.is_empty() => {
                return Err(FormError::EndpointMissing);
            }
            BigQuerySignIn::Emulator => GoogleCloudSignIn::Anonymous,
        };
        let limit = match self.limit_gb.trim() {
            "" => None,
            typed => Some(
                byte_limit::parse_gigabytes(typed)
                    .ok_or(FormError::InvalidByteLimit(LIMIT_LABEL))?,
            ),
        };
        Ok(BigQueryConfig {
            sign_in,
            project,
            dataset: self.dataset.trim().to_owned(),
            location: self.location.trim().to_owned(),
            other_projects: self
                .other_projects
                .split(',')
                .map(str::trim)
                .filter(|project| !project.is_empty())
                .map(str::to_owned)
                .collect(),
            endpoint,
            cost_guard: CostGuard {
                ask_above_bytes: limit,
                ..self.cost_guard
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_data_source_asks_above_a_gigabyte() {
        let fields = BigQueryFields {
            project: " analytics ".into(),
            ..BigQueryFields::default()
        };
        assert_eq!(fields.limit_gb, "1");
        let config = fields.to_config().unwrap();
        assert_eq!(config.project, "analytics");
        assert_eq!(config.sign_in, GoogleCloudSignIn::Google);
        assert_eq!(config.cost_guard.ask_above_bytes, Some(NEW_LIMIT));
    }

    #[test]
    fn each_sign_in_needs_what_it_signs_in_with() {
        let mut fields = BigQueryFields {
            project: "analytics".into(),
            other_projects: "bigquery-public-data, ,acme-shared ".into(),
            limit_gb: String::new(),
            ..BigQueryFields::default()
        };
        let config = fields.to_config().unwrap();
        assert_eq!(
            config.other_projects,
            ["bigquery-public-data", "acme-shared"]
        );
        assert_eq!(config.cost_guard.ask_above_bytes, None);

        fields.sign_in = BigQuerySignIn::CredentialsFile;
        assert_eq!(fields.to_config(), Err(FormError::CredentialsFileMissing));
        fields.credentials_file = "~/keys/reader.json".into();
        assert_eq!(
            fields.to_config().unwrap().sign_in,
            GoogleCloudSignIn::CredentialsFile {
                credentials_file: "~/keys/reader.json".into()
            }
        );

        fields.sign_in = BigQuerySignIn::Emulator;
        assert_eq!(fields.to_config(), Err(FormError::EndpointMissing));
        fields.endpoint = "http://localhost:59050".into();
        let config = fields.to_config().unwrap();
        assert_eq!(config.sign_in, GoogleCloudSignIn::Anonymous);
        assert_eq!(BigQueryFields::from_config(&config).to_config(), Ok(config));

        fields.limit_gb = "lots".into();
        assert_eq!(
            fields.to_config(),
            Err(FormError::InvalidByteLimit(LIMIT_LABEL))
        );
        fields.project.clear();
        assert_eq!(fields.to_config(), Err(FormError::ProjectMissing));
    }
}
