//! A bucket a DuckDB data source reads from (C2, `docs/cloud-data.md`): which provider, where
//! the bucket is, and how DbRover signs in to it. Nothing here is secret: a cloud sign-in names
//! where the sign-in already on the machine is, and an access key's secret is kept in the
//! keychain as the data source's password.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::placeholder::{PlaceholderError, Setting, Variables, expand, expand_path};
use crate::{AzureTool, CatalogPath, NodeKind, ObjectKind, TableFormat};

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
    /// No credentials, for a public bucket or a local emulator.
    Anonymous,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GcsBucket {
    pub bucket: String,
    /// The address of a stand-in for Google's (`http://localhost:4443`); empty for Google's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "GoogleCloudSignIn::is_default")]
    pub sign_in: GoogleCloudSignIn,
}

/// Who a Google Cloud data source signs in as: a Cloud Storage bucket (C2), or BigQuery (C4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "with", rename_all = "kebab-case")]
pub enum GoogleCloudSignIn {
    /// Application Default Credentials (C0b), or a credentials file of the data source's own.
    #[default]
    Google,
    CredentialsFile {
        credentials_file: PathBuf,
    },
    /// No credentials, for a public bucket or a local emulator.
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

impl GoogleCloudSignIn {
    /// The sign-in with the placeholders (W9) in its credentials file's path resolved.
    pub(crate) fn resolve(&self, variables: &dyn Variables) -> Result<Self, PlaceholderError> {
        Ok(match self {
            Self::CredentialsFile { credentials_file } => Self::CredentialsFile {
                credentials_file: expand_path(credentials_file, variables)
                    .map_err(PlaceholderError::in_setting(Setting::CredentialsFile))?,
            },
            sign_in @ (Self::Google | Self::Anonymous) => sign_in.clone(),
        })
    }

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
                sign_in: gcs.sign_in.resolve(variables)?,
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

/// The extensions of the files DuckDB reads as a table on its own, by their name: Parquet, CSV
/// and JSON, compressed or not.
const TABLE_EXTENSIONS: &[&str] = &[
    ".parquet",
    ".csv",
    ".tsv",
    ".json",
    ".jsonl",
    ".ndjson",
    ".csv.gz",
    ".tsv.gz",
    ".json.gz",
    ".jsonl.gz",
    ".ndjson.gz",
    ".csv.zst",
    ".json.zst",
    ".jsonl.zst",
    ".ndjson.zst",
];

/// Whether DuckDB reads the file `name` as a table with `SELECT * FROM '<address>'`, choosing
/// its reader by the name's extension, as its replacement scans do.
pub fn reads_as_table(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    TABLE_EXTENSIONS
        .iter()
        .any(|extension| name.ends_with(extension))
}

/// The address of the bucket, folder or file at `path`: the bucket's segment's name
/// (`s3://sales`), then each folder's and the file's, joined by `/`. A folder's ends with `/`,
/// as a prefix of keys does. `None` for a path that isn't in a bucket.
pub fn object_url(path: &CatalogPath) -> Option<String> {
    let (bucket, rest) = path.segments().split_first()?;
    if bucket.kind != NodeKind::Object(ObjectKind::Bucket) {
        return None;
    }
    let mut url = format!("{}/", bucket.name);
    for (index, segment) in rest.iter().enumerate() {
        let is_last = index + 1 == rest.len();
        match segment.kind {
            NodeKind::Object(ObjectKind::Folder) => {
                url.push_str(&segment.name);
                url.push('/');
            }
            NodeKind::Object(ObjectKind::File | ObjectKind::Table(_)) if is_last => {
                url.push_str(&segment.name);
            }
            _ => return None,
        }
    }
    Some(url)
}

/// The end of an Iceberg table's metadata files' names, which `iceberg_scan` reads a table
/// from.
pub const ICEBERG_METADATA_SUFFIX: &str = ".metadata.json";

/// What DuckDB reads the file or table at `path` from, as a `FROM` clause takes it: the
/// address itself for a file it reads as a table (`'s3://sales/orders.parquet'`), and
/// `iceberg_scan` or `delta_scan` of it for a table, or for an Iceberg metadata file. `None`
/// for anything else.
pub fn object_source(path: &CatalogPath) -> Option<String> {
    let last = path.last()?;
    let reader = match last.kind {
        NodeKind::Object(ObjectKind::Table(TableFormat::Iceberg)) => Some("iceberg_scan"),
        NodeKind::Object(ObjectKind::Table(TableFormat::Delta)) => Some("delta_scan"),
        NodeKind::Object(ObjectKind::File) if last.name.ends_with(ICEBERG_METADATA_SUFFIX) => {
            Some("iceberg_scan")
        }
        NodeKind::Object(ObjectKind::File) if reads_as_table(&last.name) => None,
        _ => return None,
    };
    let address = format!("'{}'", object_url(path)?.replace('\'', "''"));
    Some(match reader {
        Some(reader) => format!("{reader}({address})"),
        None => address,
    })
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
                    sign_in: GoogleCloudSignIn::Google,
                }),
                "provider = \"gcs\"",
            ),
            (
                ObjectStorage::Gcs(GcsBucket {
                    bucket: "sales".to_owned(),
                    endpoint: String::new(),
                    sign_in: GoogleCloudSignIn::CredentialsFile {
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
    fn a_paths_address_joins_its_folders_and_file_to_the_buckets() {
        let bucket = CatalogPath::root().child(NodeKind::Object(ObjectKind::Bucket), "s3://sales");
        let folder = bucket
            .child(NodeKind::Object(ObjectKind::Folder), "year=2025")
            .child(NodeKind::Object(ObjectKind::Folder), "month=01");
        let file = folder.child(NodeKind::Object(ObjectKind::File), "part 0.parquet");
        let cases = [
            (bucket, Some("s3://sales/")),
            (folder, Some("s3://sales/year=2025/month=01/")),
            (file, Some("s3://sales/year=2025/month=01/part 0.parquet")),
            (
                CatalogPath::root().child(NodeKind::Database, "memory"),
                None,
            ),
            (CatalogPath::root(), None),
        ];
        for (path, expected) in cases {
            assert_eq!(object_url(&path).as_deref(), expected, "{path}");
        }
    }

    #[test]
    fn duckdb_reads_files_by_their_address_and_tables_with_their_formats_reader() {
        let warehouse = CatalogPath::root()
            .child(NodeKind::Object(ObjectKind::Bucket), "s3://lake")
            .child(NodeKind::Object(ObjectKind::Folder), "warehouse");
        let orders = warehouse.child(NodeKind::Object(ObjectKind::Folder), "orders");
        let products = warehouse.child(NodeKind::Object(ObjectKind::Folder), "products");
        let object =
            |parent: &CatalogPath, kind, name: &str| parent.child(NodeKind::Object(kind), name);
        let cases = [
            (
                object(&orders, ObjectKind::File, "O'Brien.csv"),
                Some("'s3://lake/warehouse/orders/O''Brien.csv'"),
            ),
            (
                object(
                    &orders,
                    ObjectKind::Table(TableFormat::Iceberg),
                    "metadata/00002-a.metadata.json",
                ),
                Some("iceberg_scan('s3://lake/warehouse/orders/metadata/00002-a.metadata.json')"),
            ),
            (
                object(&orders, ObjectKind::File, "00002-a.metadata.json"),
                Some("iceberg_scan('s3://lake/warehouse/orders/00002-a.metadata.json')"),
            ),
            (
                object(&products, ObjectKind::Table(TableFormat::Delta), ""),
                Some("delta_scan('s3://lake/warehouse/products/')"),
            ),
            (object(&orders, ObjectKind::File, "snap-1.avro"), None),
            (orders.clone(), None),
        ];
        for (path, expected) in cases {
            assert_eq!(object_source(&path).as_deref(), expected, "{path}");
        }
    }

    #[test]
    fn files_read_as_tables_by_their_extension() {
        let cases = [
            ("part-0.parquet", true),
            ("events.JSONL", true),
            ("orders.csv.gz", true),
            ("00002-1f.metadata.json", true),
            ("_SUCCESS", false),
            ("photo.png", false),
            ("snap-1.avro", false),
        ];
        for (name, expected) in cases {
            assert_eq!(reads_as_table(name), expected, "{name}");
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
