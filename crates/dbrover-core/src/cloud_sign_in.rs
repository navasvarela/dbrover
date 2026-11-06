//! A database password that comes from a cloud account instead of the keychain (C0d,
//! `docs/cloud-data.md`): a short-lived token that `dbrover-cloud` fetches on every
//! connection. Nothing here is secret; the settings name where the sign-in already on the
//! machine is, never a credential.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::DriverKind;
use crate::placeholder::{PlaceholderError, Setting, Variables, expand, expand_path};

/// The cloud account a database's password comes from, written in `data_sources.toml` as
/// `cloud_sign_in = { provider = "aws", profile = "analytics" }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum CloudSignIn {
    /// An RDS or Aurora IAM authentication token, signed with an AWS profile's credentials.
    Aws {
        /// Empty for AWS's default chain.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        profile: String,
        /// Empty for the profile's or the environment's region.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        region: String,
    },
    /// A Google Cloud access token, for Cloud SQL's IAM users.
    Google {
        /// A credentials file of the data source's own; absent for Application Default
        /// Credentials.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_file: Option<PathBuf>,
    },
    /// A Microsoft Entra ID token from the Azure CLI's or the Azure Developer CLI's sign-in.
    Entra {
        #[serde(default, skip_serializing_if = "AzureTool::is_default")]
        tool: AzureTool,
        /// Empty for the CLI's default directory.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        tenant: String,
    },
}

/// Which provider a [`CloudSignIn`] is, for choosing one in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudProvider {
    Aws,
    Google,
    Entra,
}

/// The program whose sign-in an Entra ID data source uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AzureTool {
    /// `az`.
    #[default]
    AzureCli,
    /// `azd`.
    AzureDeveloperCli,
}

impl AzureTool {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl CloudSignIn {
    pub fn provider(&self) -> CloudProvider {
        match self {
            Self::Aws { .. } => CloudProvider::Aws,
            Self::Google { .. } => CloudProvider::Google,
            Self::Entra { .. } => CloudProvider::Entra,
        }
    }

    pub(crate) fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        let text = |text: &str, setting| {
            expand(text, variables).map_err(PlaceholderError::in_setting(setting))
        };
        Ok(match self {
            Self::Aws { profile, region } => Self::Aws {
                profile: text(profile, Setting::Profile)?,
                region: text(region, Setting::Region)?,
            },
            Self::Google { credentials_file } => Self::Google {
                credentials_file: credentials_file
                    .as_deref()
                    .map(|path| {
                        expand_path(path, variables)
                            .map_err(PlaceholderError::in_setting(Setting::CredentialsFile))
                    })
                    .transpose()?,
            },
            Self::Entra { tool, tenant } => Self::Entra {
                tool: *tool,
                tenant: text(tenant, Setting::Tenant)?,
            },
        })
    }
}

impl CloudProvider {
    /// The sign-in a provider starts with when chosen in the dialog.
    pub fn default_sign_in(self) -> CloudSignIn {
        match self {
            Self::Aws => CloudSignIn::Aws {
                profile: String::new(),
                region: String::new(),
            },
            Self::Google => CloudSignIn::Google {
                credentials_file: None,
            },
            Self::Entra => CloudSignIn::Entra {
                tool: AzureTool::default(),
                tenant: String::new(),
            },
        }
    }
}

impl DriverKind {
    /// The cloud accounts the engine's managed services take tokens from, in the providers'
    /// order: RDS and Aurora, Cloud SQL and Azure Database for PostgreSQL and MySQL, and only
    /// Entra ID for Azure SQL, since neither RDS nor Cloud SQL signs SQL Server in with IAM.
    pub const fn cloud_providers(self) -> &'static [CloudProvider] {
        match self {
            Self::Postgres | Self::Mysql => &[
                CloudProvider::Aws,
                CloudProvider::Google,
                CloudProvider::Entra,
            ],
            Self::SqlServer => &[CloudProvider::Entra],
            Self::Sqlite
            | Self::DuckDb
            | Self::ClickHouse
            | Self::Redis
            | Self::Mongo
            | Self::Elasticsearch
            | Self::Cassandra
            | Self::DynamoDb
            | Self::Athena
            | Self::Trino => &[],
        }
    }
}

impl fmt::Display for CloudProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Aws => "AWS IAM",
            Self::Google => "Google Cloud",
            Self::Entra => "Microsoft Entra ID",
        })
    }
}

impl fmt::Display for AzureTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AzureCli => "Azure CLI",
            Self::AzureDeveloperCli => "Azure Developer CLI",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Holder {
        cloud_sign_in: CloudSignIn,
    }

    #[test]
    fn each_sign_in_writes_only_what_was_set() {
        for (sign_in, written) in [
            (
                CloudProvider::Aws.default_sign_in(),
                r#"cloud_sign_in = { provider = "aws" }"#,
            ),
            (
                CloudSignIn::Aws {
                    profile: "analytics".into(),
                    region: "eu-west-1".into(),
                },
                r#"cloud_sign_in = { provider = "aws", profile = "analytics", region = "eu-west-1" }"#,
            ),
            (
                CloudSignIn::Google {
                    credentials_file: Some("~/keys/reader.json".into()),
                },
                r#"cloud_sign_in = { provider = "google", credentials_file = "~/keys/reader.json" }"#,
            ),
            (
                CloudProvider::Entra.default_sign_in(),
                r#"cloud_sign_in = { provider = "entra" }"#,
            ),
            (
                CloudSignIn::Entra {
                    tool: AzureTool::AzureDeveloperCli,
                    tenant: "contoso.onmicrosoft.com".into(),
                },
                r#"cloud_sign_in = { provider = "entra", tool = "azure-developer-cli", tenant = "contoso.onmicrosoft.com" }"#,
            ),
        ] {
            let holder = Holder {
                cloud_sign_in: sign_in,
            };
            let parsed: Holder = toml::from_str(written).unwrap();
            assert_eq!(parsed, holder, "{written}");
            let text = toml::to_string(&holder).unwrap();
            assert_eq!(toml::from_str::<Holder>(&text).unwrap(), holder, "{text}");
        }
    }

    #[test]
    fn sql_server_signs_in_only_with_entra_id() {
        assert_eq!(
            DriverKind::SqlServer.cloud_providers(),
            [CloudProvider::Entra]
        );
        assert_eq!(DriverKind::Postgres.cloud_providers().len(), 3);
        assert!(DriverKind::DynamoDb.cloud_providers().is_empty());
    }
}
