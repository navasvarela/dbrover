//! A bucket a DuckDB data source reads from (C2, `docs/cloud-data.md`): which provider, where
//! the bucket is, and how DbRover signs in to it. Nothing here is secret: a cloud sign-in names
//! where the sign-in already on the machine is, and an access key's secret is kept in the
//! keychain as the data source's password.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::AzureTool;
use crate::placeholder::{PlaceholderError, Setting, Variables, expand, expand_path};

/// A bucket, written in `data_sources.toml` as `[data_source.storage]` with its `provider`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum ObjectStorage {
    /// Amazon S3, or a store that speaks its API (MinIO, Cloudflare R2) at an endpoint.
    S3(S3Bucket),
    /// Google Cloud Storage.
    Gcs(GcsBucket),
    /// A container of an Azure Blob Storage account.
    Azure(AzureContainer),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct S3Bucket {
    pub bucket: String,
    /// Empty for the profile's or the environment's region, or `us-east-1` when neither has
    /// one, as AWS's own tools default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
    /// The address of an S3-compatible store (`http://localhost:9000`), whose buckets are
    /// reached by path rather than by host name; empty for AWS's own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "S3SignIn::is_default")]
    pub sign_in: S3SignIn,
}

/// Who an S3 bucket is read as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "with", rename_all = "kebab-case")]
pub enum S3SignIn {
    /// AWS's credential chain, as C0 reads it for every AWS data source: the profile alone
    /// when one is named, else AWS's default chain.
    #[default]
    Aws,
    /// One profile of the AWS config and credentials files.
    Profile { profile: String },
    /// An access key of the store's own (MinIO, R2), whose secret is the data source's
    /// password.
    AccessKey { key_id: String },
    /// No credentials, for a public bucket.
    Anonymous,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GcsBucket {
    pub bucket: String,
    /// The address of a stand-in for Google's (`http://localhost:4443`); empty for Google's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "GcsSignIn::is_default")]
    pub sign_in: GcsSignIn,
}

/// Who a Google Cloud Storage bucket is read as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "with", rename_all = "kebab-case")]
pub enum GcsSignIn {
    /// Application Default Credentials (C0b), or a credentials file of the data source's own.
    #[default]
    Google,
    CredentialsFile {
        credentials_file: PathBuf,
    },
    /// No credentials, for a public bucket.
    Anonymous,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AzureContainer {
    /// The storage account, the first label of `<account>.blob.core.windows.net`.
    pub account: String,
    pub container: String,
    /// The address of the account's blob service when it isn't
    /// `https://<account>.blob.core.windows.net`: Azurite, or a private endpoint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "AzureSignIn::is_default")]
    pub sign_in: AzureSignIn,
}

/// Who an Azure container is read as.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "with", rename_all = "kebab-case")]
pub enum AzureSignIn {
    /// A Microsoft Entra ID token for Azure Storage, from the Azure CLI's or the Azure
    /// Developer CLI's sign-in (C0c).
    Entra {
        #[serde(default, skip_serializing_if = "AzureTool::is_default")]
        tool: AzureTool,
        /// Empty for the CLI's default directory.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        tenant: String,
    },
    /// No credentials, for a container that allows public reads.
    Anonymous,
}

impl Default for AzureSignIn {
    fn default() -> Self {
        Self::Entra {
            tool: AzureTool::default(),
            tenant: String::new(),
        }
    }
}

impl S3SignIn {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl GcsSignIn {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl AzureSignIn {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Which provider an [`ObjectStorage`] is, for choosing one in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageProvider {
    S3,
    Gcs,
    Azure,
}

impl StorageProvider {
    /// In the providers' order (`docs/cloud-data.md`): AWS, Google Cloud, then Azure.
    pub const ALL: [Self; 3] = [Self::S3, Self::Gcs, Self::Azure];

    /// The scheme DuckDB reads the provider's files with, and which its secrets are scoped to.
    pub fn scheme(self) -> &'static str {
        match self {
            Self::S3 => "s3",
            Self::Gcs => "gs",
            Self::Azure => "az",
        }
    }

    /// An empty bucket of this provider, as the dialog starts one.
    pub fn empty(self) -> ObjectStorage {
        match self {
            Self::S3 => ObjectStorage::S3(S3Bucket::default()),
            Self::Gcs => ObjectStorage::Gcs(GcsBucket::default()),
            Self::Azure => ObjectStorage::Azure(AzureContainer::default()),
        }
    }
}

impl fmt::Display for StorageProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::S3 => "Amazon S3",
            Self::Gcs => "Google Cloud Storage",
            Self::Azure => "Azure Blob Storage",
        })
    }
}

impl ObjectStorage {
    pub fn provider(&self) -> StorageProvider {
        match self {
            Self::S3(_) => StorageProvider::S3,
            Self::Gcs(_) => StorageProvider::Gcs,
            Self::Azure(_) => StorageProvider::Azure,
        }
    }

    /// The bucket's name, or the Azure container's.
    pub fn bucket(&self) -> &str {
        match self {
            Self::S3(s3) => &s3.bucket,
            Self::Gcs(gcs) => &gcs.bucket,
            Self::Azure(azure) => &azure.container,
        }
    }

    /// The address DuckDB reads the bucket's files under, without a trailing `/`:
    /// `s3://sales`, `gs://sales` or `az://sales`.
    pub fn url(&self) -> String {
        format!("{}://{}", self.provider().scheme(), self.bucket())
    }

    /// Whether the data source's password is this bucket's secret: an access key's.
    pub fn uses_password(&self) -> bool {
        matches!(
            self,
            Self::S3(S3Bucket {
                sign_in: S3SignIn::AccessKey { .. },
                ..
            })
        )
    }

    /// The bucket with every placeholder (W9) replaced by its value.
    pub(crate) fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        let text = |text: &str, setting| {
            expand(text, variables).map_err(PlaceholderError::in_setting(setting))
        };
        Ok(match self {
            Self::S3(s3) => Self::S3(S3Bucket {
                bucket: text(&s3.bucket, Setting::Bucket)?,
                region: text(&s3.region, Setting::Region)?,
                endpoint: text(&s3.endpoint, Setting::Endpoint)?,
                sign_in: match &s3.sign_in {
                    S3SignIn::Profile { profile } => S3SignIn::Profile {
                        profile: text(profile, Setting::Profile)?,
                    },
                    S3SignIn::AccessKey { key_id } => S3SignIn::AccessKey {
                        key_id: text(key_id, Setting::AccessKey)?,
                    },
                    sign_in @ (S3SignIn::Aws | S3SignIn::Anonymous) => sign_in.clone(),
                },
            }),
            Self::Gcs(gcs) => Self::Gcs(GcsBucket {
                bucket: text(&gcs.bucket, Setting::Bucket)?,
                endpoint: text(&gcs.endpoint, Setting::Endpoint)?,
                sign_in: match &gcs.sign_in {
                    GcsSignIn::CredentialsFile { credentials_file } => GcsSignIn::CredentialsFile {
                        credentials_file: expand_path(credentials_file, variables)
                            .map_err(PlaceholderError::in_setting(Setting::CredentialsFile))?,
                    },
                    sign_in @ (GcsSignIn::Google | GcsSignIn::Anonymous) => sign_in.clone(),
                },
            }),
            Self::Azure(azure) => Self::Azure(AzureContainer {
                account: text(&azure.account, Setting::StorageAccount)?,
                container: text(&azure.container, Setting::Bucket)?,
                endpoint: text(&azure.endpoint, Setting::Endpoint)?,
                sign_in: match &azure.sign_in {
                    AzureSignIn::Entra { tool, tenant } => AzureSignIn::Entra {
                        tool: *tool,
                        tenant: text(tenant, Setting::Tenant)?,
                    },
                    AzureSignIn::Anonymous => AzureSignIn::Anonymous,
                },
            }),
        })
    }
}

/// The bucket's address and where it is served from, for logs and tooltips. Never a secret,
/// and never the key's id either, which names an account.
impl fmt::Display for ObjectStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url())?;
        let endpoint = match self {
            Self::S3(s3) => &s3.endpoint,
            Self::Gcs(gcs) => &gcs.endpoint,
            Self::Azure(azure) => &azure.endpoint,
        };
        match (self, endpoint.as_str()) {
            (Self::Azure(azure), "") => write!(f, " on {}", azure.account),
            (_, "") => Ok(()),
            (_, endpoint) => write!(f, " at {endpoint}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placeholder::tests::Fixed;

    fn s3(sign_in: S3SignIn) -> ObjectStorage {
        ObjectStorage::S3(S3Bucket {
            bucket: "sales".to_owned(),
            region: "eu-west-1".to_owned(),
            endpoint: String::new(),
            sign_in,
        })
    }

    #[test]
    fn every_provider_and_sign_in_is_written_and_read_back() {
        let cases = [
            (s3(S3SignIn::Aws), "provider = \"s3\""),
            (
                s3(S3SignIn::Profile {
                    profile: "analytics".to_owned(),
                }),
                "with = \"profile\"",
            ),
            (
                s3(S3SignIn::AccessKey {
                    key_id: "minio".to_owned(),
                }),
                "with = \"access-key\"",
            ),
            (s3(S3SignIn::Anonymous), "with = \"anonymous\""),
            (
                ObjectStorage::Gcs(GcsBucket {
                    bucket: "sales".to_owned(),
                    endpoint: "http://localhost:4443".to_owned(),
                    sign_in: GcsSignIn::Google,
                }),
                "provider = \"gcs\"",
            ),
            (
                ObjectStorage::Gcs(GcsBucket {
                    bucket: "sales".to_owned(),
                    endpoint: String::new(),
                    sign_in: GcsSignIn::CredentialsFile {
                        credentials_file: "/keys/reader.json".into(),
                    },
                }),
                "with = \"credentials-file\"",
            ),
            (
                ObjectStorage::Azure(AzureContainer {
                    account: "acme".to_owned(),
                    container: "sales".to_owned(),
                    endpoint: String::new(),
                    sign_in: AzureSignIn::Entra {
                        tool: AzureTool::AzureDeveloperCli,
                        tenant: "acme.onmicrosoft.com".to_owned(),
                    },
                }),
                "tool = \"azure-developer-cli\"",
            ),
            (
                ObjectStorage::Azure(AzureContainer {
                    account: "acme".to_owned(),
                    container: "sales".to_owned(),
                    endpoint: String::new(),
                    sign_in: AzureSignIn::Anonymous,
                }),
                "provider = \"azure\"",
            ),
        ];
        for (storage, expected) in cases {
            let text = toml::to_string(&storage).unwrap();
            assert!(text.contains(expected), "{text}");
            assert_eq!(toml::from_str::<ObjectStorage>(&text).unwrap(), storage);
        }
    }

    #[test]
    fn the_default_sign_ins_are_left_out() {
        let text = toml::to_string(&s3(S3SignIn::Aws)).unwrap();

        assert_eq!(
            text,
            "provider = \"s3\"\nbucket = \"sales\"\nregion = \"eu-west-1\"\n"
        );
    }

    #[test]
    fn urls_use_the_scheme_duckdb_reads_each_provider_with() {
        let cases = [
            (s3(S3SignIn::Aws), "s3://sales", "s3://sales"),
            (
                ObjectStorage::S3(S3Bucket {
                    bucket: "dbrover".to_owned(),
                    endpoint: "http://localhost:59000".to_owned(),
                    ..S3Bucket::default()
                }),
                "s3://dbrover",
                "s3://dbrover at http://localhost:59000",
            ),
            (
                ObjectStorage::Gcs(GcsBucket {
                    bucket: "logs".to_owned(),
                    ..GcsBucket::default()
                }),
                "gs://logs",
                "gs://logs",
            ),
            (
                ObjectStorage::Azure(AzureContainer {
                    account: "acme".to_owned(),
                    container: "raw".to_owned(),
                    ..AzureContainer::default()
                }),
                "az://raw",
                "az://raw on acme",
            ),
        ];
        for (storage, url, shown) in cases {
            assert_eq!(storage.url(), url);
            assert_eq!(storage.to_string(), shown);
        }
    }

    #[test]
    fn only_an_access_key_uses_the_password() {
        let key = s3(S3SignIn::AccessKey {
            key_id: "minio".to_owned(),
        });

        assert!(key.uses_password());
        assert!(!s3(S3SignIn::Aws).uses_password());
        assert!(!StorageProvider::Gcs.empty().uses_password());
        assert!(!StorageProvider::Azure.empty().uses_password());
    }

    #[test]
    fn placeholders_are_resolved_in_every_field() {
        let variables = Fixed::new(&[("BUCKET", "sales"), ("KEY", "minio")]);
        let storage = ObjectStorage::S3(S3Bucket {
            bucket: "${BUCKET}".to_owned(),
            region: String::new(),
            endpoint: String::new(),
            sign_in: S3SignIn::AccessKey {
                key_id: "${KEY}".to_owned(),
            },
        });

        let resolved = storage.resolve(&variables).unwrap();

        assert_eq!(resolved.url(), "s3://sales");
        let ObjectStorage::S3(S3Bucket {
            sign_in: S3SignIn::AccessKey { key_id },
            ..
        }) = resolved
        else {
            panic!("not an access key: {resolved:?}");
        };
        assert_eq!(key_id, "minio");
    }

    #[test]
    fn an_unset_placeholder_names_its_setting() {
        let storage = ObjectStorage::Azure(AzureContainer {
            account: "${ACCOUNT}".to_owned(),
            container: "raw".to_owned(),
            ..AzureContainer::default()
        });

        let error = storage.resolve(&Fixed::new(&[])).unwrap_err();

        assert!(error.to_string().contains("storage account"), "{error}");
    }
}
