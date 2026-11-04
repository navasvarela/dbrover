//! A bucket a DuckDB data source reads from (C2, `docs/cloud-data.md`): the temporary secret
//! DuckDB reads it with, made from the data source's sign-in when the connection opens, and
//! made again before the credentials expire.

mod secret;

use std::time::SystemTime;

use dbrover_cloud::aws::{AwsCredentials, AwsError, AwsSignIn};
use dbrover_cloud::token::REFRESH_MARGIN;
use dbrover_core::{ObjectStorage, Password, S3Bucket, S3SignIn, StorageProvider};

pub(crate) use secret::SecretStatement;

#[derive(Debug, thiserror::Error)]
pub enum BucketError {
    #[error(
        "the bucket signs in with an access key, and the data source has no secret for it: set \
         it in the data source's settings"
    )]
    NoSecret,
    #[error(
        "the endpoint {0} isn't an address DuckDB can use: write it as http://host:port or https://host"
    )]
    Endpoint(String),
    #[error("reading {0} isn't built yet")]
    Unsupported(StorageProvider),
    #[error(transparent)]
    Aws(#[from] AwsError),
}

/// What DuckDB is handed to read a bucket with.
pub(crate) enum BucketCredentials {
    /// An S3 access key, with the session token temporary credentials come with.
    S3Keys {
        key_id: String,
        secret: Password,
        session_token: Option<Password>,
    },
    /// Nothing, for a public bucket.
    Anonymous,
}

/// What signing in to a bucket gave.
pub(crate) struct SignedIn {
    pub(crate) credentials: BucketCredentials,
    /// The region the sign-in named, when the bucket's settings name none.
    pub(crate) region: Option<String>,
    /// When the credentials stop working; `None` for ones that don't expire.
    pub(crate) expires_at: Option<SystemTime>,
}

/// Where a connection's secret for its bucket stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Secret {
    /// Not made yet, or making it failed and is tried again before the next statement.
    Missing,
    Made {
        expires_at: Option<SystemTime>,
    },
}

/// A connection's bucket, and its secret in DuckDB.
pub(crate) struct Bucket {
    storage: ObjectStorage,
    /// The data source's password: an access key's secret.
    password: Option<Password>,
    secret: Secret,
}

impl Bucket {
    pub(crate) fn new(storage: ObjectStorage, password: Option<Password>) -> Self {
        Self {
            storage,
            password,
            secret: Secret::Missing,
        }
    }

    /// Whether the secret needs making at `now`: it never was, or its credentials have less
    /// than C0's margin left, which is when every cloud sign-in of DbRover's fetches anew.
    fn is_due(&self, now: SystemTime) -> bool {
        match self.secret {
            Secret::Missing => true,
            Secret::Made { expires_at: None } => false,
            Secret::Made {
                expires_at: Some(expires_at),
            } => !expires_at
                .duration_since(now)
                .is_ok_and(|left| left > REFRESH_MARGIN),
        }
    }

    /// The statement making the secret, and when what it holds expires, if the secret needs
    /// making at `now`; signs in to get it.
    ///
    /// # Errors
    ///
    /// [`BucketError`] when the sign-in gives no credentials.
    pub(crate) async fn due_secret(
        &self,
        now: SystemTime,
    ) -> Result<Option<(SecretStatement, Option<SystemTime>)>, BucketError> {
        if !self.is_due(now) {
            return Ok(None);
        }
        let ObjectStorage::S3(bucket) = &self.storage else {
            return Err(BucketError::Unsupported(self.storage.provider()));
        };
        let signed_in = self.sign_in_to_s3(bucket).await?;
        let statement = secret::s3_secret(bucket, &signed_in)?;
        Ok(Some((statement, signed_in.expires_at)))
    }

    /// Records that DuckDB took the secret, which works until `expires_at`.
    pub(crate) fn made(&mut self, expires_at: Option<SystemTime>) {
        self.secret = Secret::Made { expires_at };
    }

    async fn sign_in_to_s3(&self, bucket: &S3Bucket) -> Result<SignedIn, BucketError> {
        let credentials = match &bucket.sign_in {
            S3SignIn::Aws => AwsCredentials::DefaultChain,
            S3SignIn::Profile { profile } => AwsCredentials::Profile(profile.clone()),
            S3SignIn::AccessKey { key_id } => {
                let secret = self.password.clone().ok_or(BucketError::NoSecret)?;
                return Ok(SignedIn {
                    credentials: BucketCredentials::S3Keys {
                        key_id: key_id.clone(),
                        secret,
                        session_token: None,
                    },
                    region: None,
                    expires_at: None,
                });
            }
            S3SignIn::Anonymous => {
                return Ok(SignedIn {
                    credentials: BucketCredentials::Anonymous,
                    region: None,
                    expires_at: None,
                });
            }
        };
        let aws = AwsSignIn {
            credentials,
            region: Some(bucket.region.clone()).filter(|region| !region.is_empty()),
            endpoint: None,
        };
        let keys = aws.keys().await?;
        Ok(SignedIn {
            credentials: BucketCredentials::S3Keys {
                key_id: keys.key_id,
                secret: keys.secret,
                session_token: keys.session_token,
            },
            region: keys.region,
            expires_at: keys.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::S3Bucket;

    use super::*;

    fn access_key(password: Option<&str>) -> Bucket {
        Bucket::new(
            ObjectStorage::S3(S3Bucket {
                bucket: "dbrover".to_owned(),
                region: String::new(),
                endpoint: "http://localhost:59000".to_owned(),
                sign_in: S3SignIn::AccessKey {
                    key_id: "dbrover".to_owned(),
                },
            }),
            password.map(Password::new),
        )
    }

    #[test]
    fn a_secret_is_made_again_once_less_than_the_margin_is_left() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let minutes = |minutes: u64| Duration::from_secs(minutes * 60);
        let cases = [
            (Secret::Missing, true),
            (Secret::Made { expires_at: None }, false),
            (
                Secret::Made {
                    expires_at: Some(now + minutes(60)),
                },
                false,
            ),
            (
                Secret::Made {
                    expires_at: Some(now + minutes(6)),
                },
                false,
            ),
            (
                Secret::Made {
                    expires_at: Some(now + minutes(5)),
                },
                true,
            ),
            (
                Secret::Made {
                    expires_at: Some(now - minutes(1)),
                },
                true,
            ),
        ];
        for (secret, due) in cases {
            let mut bucket = access_key(Some("dbrover-test"));
            bucket.secret = secret;
            assert_eq!(bucket.is_due(now), due, "{secret:?}");
        }
    }

    #[tokio::test]
    async fn an_access_key_is_made_into_a_secret_once() {
        let mut bucket = access_key(Some("dbrover-test"));
        let now = SystemTime::now();

        let (statement, expires_at) = bucket.due_secret(now).await.unwrap().unwrap();
        bucket.made(expires_at);

        assert!(statement.expose().contains("SECRET 'dbrover-test'"));
        assert_eq!(expires_at, None);
        assert!(bucket.due_secret(now).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_access_key_without_its_secret_says_where_to_set_it() {
        let bucket = access_key(None);

        let error = bucket.due_secret(SystemTime::now()).await.unwrap_err();

        assert!(matches!(error, BucketError::NoSecret), "{error}");
    }
}
