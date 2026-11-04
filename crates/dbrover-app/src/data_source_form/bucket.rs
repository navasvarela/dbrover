//! A DuckDB data source's bucket (C2), as typed: every provider's fields are kept while
//! another is chosen, so switching back loses nothing.

use std::path::PathBuf;

use dbrover_core::{
    AzureContainer, AzureSignIn, AzureTool, GcsBucket, GcsSignIn, ObjectStorage, S3Bucket,
    S3SignIn, StorageProvider,
};

use super::FormError;

/// How a bucket signs in, as the dialog offers it for its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketSignIn {
    /// The provider's own sign-in on this machine: AWS's chain, Application Default
    /// Credentials, or the Azure CLI's or Azure Developer CLI's.
    Provider,
    /// One AWS profile.
    Profile,
    /// An S3-compatible store's access key, whose secret is the data source's password.
    AccessKey,
    /// A Google Cloud key file of the data source's own.
    CredentialsFile,
    /// None, for a public bucket.
    Anonymous,
}

impl BucketSignIn {
    /// The sign-ins `provider`'s buckets take, the provider's own first.
    pub fn offered(provider: StorageProvider) -> &'static [Self] {
        match provider {
            StorageProvider::S3 => &[
                Self::Provider,
                Self::Profile,
                Self::AccessKey,
                Self::Anonymous,
            ],
            StorageProvider::Gcs => &[Self::Provider, Self::CredentialsFile, Self::Anonymous],
            StorageProvider::Azure => &[Self::Provider, Self::Anonymous],
        }
    }

    /// The name the dialog shows for this sign-in on `provider`'s buckets.
    pub fn label(self, provider: StorageProvider) -> &'static str {
        match (self, provider) {
            (Self::Provider, StorageProvider::S3) => "AWS (default chain)",
            (Self::Provider, StorageProvider::Gcs) => "Application Default Credentials",
            (Self::Provider, StorageProvider::Azure) => "Microsoft Entra ID",
            (Self::Profile, _) => "AWS profile",
            (Self::AccessKey, _) => "Access key",
            (Self::CredentialsFile, _) => "Credentials file",
            (Self::Anonymous, _) => "None (public)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketFields {
    /// Whether the data source reads from a bucket at all.
    pub enabled: bool,
    pub provider: StorageProvider,
    /// The S3 or Google Cloud Storage bucket, or the Azure container.
    pub bucket: String,
    pub account: String,
    pub region: String,
    pub endpoint: String,
    pub sign_in: BucketSignIn,
    pub profile: String,
    pub key_id: String,
    pub credentials_file: String,
    pub tool: AzureTool,
    pub tenant: String,
}

impl Default for BucketFields {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: StorageProvider::S3,
            bucket: String::new(),
            account: String::new(),
            region: String::new(),
            endpoint: String::new(),
            sign_in: BucketSignIn::Provider,
            profile: String::new(),
            key_id: String::new(),
            credentials_file: String::new(),
            tool: AzureTool::default(),
            tenant: String::new(),
        }
    }
}

impl BucketFields {
    pub fn from_storage(storage: Option<&ObjectStorage>) -> Self {
        let Some(storage) = storage else {
            return Self::default();
        };
        let mut fields = Self {
            enabled: true,
            provider: storage.provider(),
            ..Self::default()
        };
        match storage {
            ObjectStorage::S3(s3) => {
                s3.bucket.clone_into(&mut fields.bucket);
                s3.region.clone_into(&mut fields.region);
                s3.endpoint.clone_into(&mut fields.endpoint);
                fields.sign_in = match &s3.sign_in {
                    S3SignIn::Aws => BucketSignIn::Provider,
                    S3SignIn::Profile { profile } => {
                        profile.clone_into(&mut fields.profile);
                        BucketSignIn::Profile
                    }
                    S3SignIn::AccessKey { key_id } => {
                        key_id.clone_into(&mut fields.key_id);
                        BucketSignIn::AccessKey
                    }
                    S3SignIn::Anonymous => BucketSignIn::Anonymous,
                };
            }
            ObjectStorage::Gcs(gcs) => {
                gcs.bucket.clone_into(&mut fields.bucket);
                gcs.endpoint.clone_into(&mut fields.endpoint);
                fields.sign_in = match &gcs.sign_in {
                    GcsSignIn::Google => BucketSignIn::Provider,
                    GcsSignIn::CredentialsFile { credentials_file } => {
                        fields.credentials_file = credentials_file.display().to_string();
                        BucketSignIn::CredentialsFile
                    }
                    GcsSignIn::Anonymous => BucketSignIn::Anonymous,
                };
            }
            ObjectStorage::Azure(azure) => {
                azure.account.clone_into(&mut fields.account);
                azure.container.clone_into(&mut fields.bucket);
                azure.endpoint.clone_into(&mut fields.endpoint);
                fields.sign_in = match &azure.sign_in {
                    AzureSignIn::Entra { tool, tenant } => {
                        fields.tool = *tool;
                        tenant.clone_into(&mut fields.tenant);
                        BucketSignIn::Provider
                    }
                    AzureSignIn::Anonymous => BucketSignIn::Anonymous,
                };
            }
        }
        fields
    }

    /// Chooses `provider`, and its own sign-in when the one chosen isn't offered for it.
    pub fn choose_provider(&mut self, provider: StorageProvider) {
        self.provider = provider;
        if !BucketSignIn::offered(provider).contains(&self.sign_in) {
            self.sign_in = BucketSignIn::Provider;
        }
    }

    /// Whether the data source's password is this bucket's access key's secret.
    pub fn uses_password(&self) -> bool {
        self.enabled
            && self.provider == StorageProvider::S3
            && self.sign_in == BucketSignIn::AccessKey
    }

    /// The bucket as typed, or `None` when the data source reads from none.
    ///
    /// # Errors
    ///
    /// [`FormError`] for a missing bucket, account, key or file, or a sign-in the provider
    /// doesn't take.
    pub fn to_storage(&self) -> Result<Option<ObjectStorage>, FormError> {
        if !self.enabled {
            return Ok(None);
        }
        let bucket = required(&self.bucket, FormError::BucketMissing)?;
        let endpoint = self.endpoint.trim().to_owned();
        let storage = match self.provider {
            StorageProvider::S3 => ObjectStorage::S3(S3Bucket {
                bucket,
                region: self.region.trim().to_owned(),
                endpoint,
                sign_in: self.s3_sign_in()?,
            }),
            StorageProvider::Gcs => ObjectStorage::Gcs(GcsBucket {
                bucket,
                endpoint,
                sign_in: self.gcs_sign_in()?,
            }),
            StorageProvider::Azure => ObjectStorage::Azure(AzureContainer {
                account: required(&self.account, FormError::StorageAccountMissing)?,
                container: bucket,
                endpoint,
                sign_in: self.azure_sign_in()?,
            }),
        };
        Ok(Some(storage))
    }

    fn s3_sign_in(&self) -> Result<S3SignIn, FormError> {
        Ok(match self.sign_in {
            BucketSignIn::Provider => S3SignIn::Aws,
            BucketSignIn::Profile => S3SignIn::Profile {
                profile: required(&self.profile, FormError::ProfileMissing)?,
            },
            BucketSignIn::AccessKey => S3SignIn::AccessKey {
                key_id: required(&self.key_id, FormError::AccessKeyMissing)?,
            },
            BucketSignIn::Anonymous => S3SignIn::Anonymous,
            BucketSignIn::CredentialsFile => return Err(self.not_offered()),
        })
    }

    fn gcs_sign_in(&self) -> Result<GcsSignIn, FormError> {
        Ok(match self.sign_in {
            BucketSignIn::Provider => GcsSignIn::Google,
            BucketSignIn::CredentialsFile => GcsSignIn::CredentialsFile {
                credentials_file: PathBuf::from(required(
                    &self.credentials_file,
                    FormError::CredentialsFileMissing,
                )?),
            },
            BucketSignIn::Anonymous => GcsSignIn::Anonymous,
            BucketSignIn::Profile | BucketSignIn::AccessKey => return Err(self.not_offered()),
        })
    }

    fn azure_sign_in(&self) -> Result<AzureSignIn, FormError> {
        Ok(match self.sign_in {
            BucketSignIn::Provider => AzureSignIn::Entra {
                tool: self.tool,
                tenant: self.tenant.trim().to_owned(),
            },
            BucketSignIn::Anonymous => AzureSignIn::Anonymous,
            BucketSignIn::Profile | BucketSignIn::AccessKey | BucketSignIn::CredentialsFile => {
                return Err(self.not_offered());
            }
        })
    }

    fn not_offered(&self) -> FormError {
        FormError::BucketSignInNotOffered(self.sign_in.label(self.provider), self.provider)
    }
}

fn required(text: &str, missing: FormError) -> Result<String, FormError> {
    match text.trim() {
        "" => Err(missing),
        text => Ok(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_and_sign_in_round_trips_through_the_fields() {
        let storages = [
            ObjectStorage::S3(S3Bucket {
                bucket: "sales".to_owned(),
                region: "eu-west-1".to_owned(),
                endpoint: String::new(),
                sign_in: S3SignIn::Aws,
            }),
            ObjectStorage::S3(S3Bucket {
                bucket: "sales".to_owned(),
                region: String::new(),
                endpoint: String::new(),
                sign_in: S3SignIn::Profile {
                    profile: "analytics".to_owned(),
                },
            }),
            ObjectStorage::S3(S3Bucket {
                bucket: "dbrover".to_owned(),
                region: String::new(),
                endpoint: "http://localhost:59000".to_owned(),
                sign_in: S3SignIn::AccessKey {
                    key_id: "dbrover".to_owned(),
                },
            }),
            ObjectStorage::Gcs(GcsBucket {
                bucket: "logs".to_owned(),
                endpoint: String::new(),
                sign_in: GcsSignIn::CredentialsFile {
                    credentials_file: "/keys/reader.json".into(),
                },
            }),
            ObjectStorage::Azure(AzureContainer {
                account: "acme".to_owned(),
                container: "raw".to_owned(),
                endpoint: String::new(),
                sign_in: AzureSignIn::Entra {
                    tool: AzureTool::AzureDeveloperCli,
                    tenant: "acme.onmicrosoft.com".to_owned(),
                },
            }),
            ObjectStorage::Azure(AzureContainer {
                account: "acme".to_owned(),
                container: "public".to_owned(),
                endpoint: String::new(),
                sign_in: AzureSignIn::Anonymous,
            }),
        ];
        for storage in storages {
            let fields = BucketFields::from_storage(Some(&storage));
            assert_eq!(fields.to_storage(), Ok(Some(storage)));
        }
        assert_eq!(BucketFields::from_storage(None).to_storage(), Ok(None));
    }

    #[test]
    fn missing_fields_are_named() {
        let s3 = |edit: fn(&mut BucketFields)| {
            let mut fields = BucketFields {
                enabled: true,
                bucket: "sales".to_owned(),
                ..BucketFields::default()
            };
            edit(&mut fields);
            fields.to_storage()
        };
        let cases = [
            (s3(|fields| fields.bucket.clear()), FormError::BucketMissing),
            (
                s3(|fields| fields.sign_in = BucketSignIn::AccessKey),
                FormError::AccessKeyMissing,
            ),
            (
                s3(|fields| fields.sign_in = BucketSignIn::Profile),
                FormError::ProfileMissing,
            ),
            (
                s3(|fields| fields.provider = StorageProvider::Azure),
                FormError::StorageAccountMissing,
            ),
            (
                s3(|fields| {
                    fields.provider = StorageProvider::Gcs;
                    fields.sign_in = BucketSignIn::CredentialsFile;
                }),
                FormError::CredentialsFileMissing,
            ),
        ];
        for (result, expected) in cases {
            assert_eq!(result, Err(expected));
        }
    }

    #[test]
    fn choosing_a_provider_keeps_a_sign_in_it_offers() {
        let mut fields = BucketFields {
            sign_in: BucketSignIn::AccessKey,
            ..BucketFields::default()
        };

        fields.choose_provider(StorageProvider::Gcs);
        let on_gcs = fields.sign_in;
        fields.sign_in = BucketSignIn::Anonymous;
        fields.choose_provider(StorageProvider::Azure);

        assert_eq!(on_gcs, BucketSignIn::Provider);
        assert_eq!(fields.sign_in, BucketSignIn::Anonymous);
    }
}
