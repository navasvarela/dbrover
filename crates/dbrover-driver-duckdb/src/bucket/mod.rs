//! A bucket a DuckDB data source reads from (C2, `docs/cloud-data.md`): the temporary secret
//! DuckDB reads it with and the client that lists it, both made from the data source's
//! sign-in when the connection opens, and made again before the credentials expire.

mod secret;
pub(crate) mod tree;

use std::sync::Arc;
use std::time::SystemTime;

use dbrover_cloud::aws::{AwsCredentials, AwsError, AwsSignIn};
use dbrover_cloud::storage::{S3Keys, S3Lister, S3Location, StorageError};
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
        "the endpoint {0} isn't an address DuckDB can use: write it as http://host:port or \
         https://host"
    )]
    Endpoint(String),
    #[error("reading {0} isn't built yet")]
    Unsupported(StorageProvider),
    #[error(transparent)]
    Aws(#[from] AwsError),
    #[error("couldn't list the bucket: {0}")]
    Listing(#[from] StorageError),
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

impl SignedIn {
    /// Whether the credentials can still be used at `now`: they don't expire, or have more
    /// than C0's margin left, which is when every cloud sign-in of DbRover's fetches anew.
    fn is_fresh_at(&self, now: SystemTime) -> bool {
        self.expires_at.is_none_or(|expires_at| {
            expires_at
                .duration_since(now)
                .is_ok_and(|left| left > REFRESH_MARGIN)
        })
    }
}

/// One sign-in, and what was made from it.
struct Session {
    signed_in: SignedIn,
    /// Whether DuckDB holds a secret made from `signed_in`.
    secret_made: bool,
    lister: Option<Arc<S3Lister>>,
}

/// A connection's bucket.
pub(crate) struct Bucket {
    storage: ObjectStorage,
    /// The data source's password: an access key's secret.
    password: Option<Password>,
    /// `None` until the first sign-in.
    session: Option<Session>,
}

impl Bucket {
    pub(crate) fn new(storage: ObjectStorage, password: Option<Password>) -> Self {
        Self {
            storage,
            password,
            session: None,
        }
    }

    pub(crate) fn storage(&self) -> &ObjectStorage {
        &self.storage
    }

    /// The statement making the secret, if DuckDB holds none made from a sign-in that is
    /// still fresh at `now`; signs in again first when the last one is about to expire.
    ///
    /// # Errors
    ///
    /// [`BucketError`] when the sign-in gives no credentials.
    pub(crate) async fn due_secret(
        &mut self,
        now: SystemTime,
    ) -> Result<Option<SecretStatement>, BucketError> {
        let ObjectStorage::S3(bucket) = &self.storage else {
            return Err(BucketError::Unsupported(self.storage.provider()));
        };
        let session = fresh_session(
            &mut self.session,
            &self.storage,
            self.password.as_ref(),
            now,
        )
        .await?;
        if session.secret_made {
            return Ok(None);
        }
        secret::s3_secret(bucket, &session.signed_in).map(Some)
    }

    /// Records that DuckDB took the secret [`Bucket::due_secret`] gave.
    pub(crate) fn made(&mut self) {
        if let Some(session) = &mut self.session {
            session.secret_made = true;
        }
    }

    /// The client that lists the bucket, signed in as DuckDB's secret is.
    ///
    /// # Errors
    ///
    /// [`BucketError`] when the sign-in gives no credentials.
    pub(crate) async fn lister(&mut self, now: SystemTime) -> Result<Arc<S3Lister>, BucketError> {
        let ObjectStorage::S3(bucket) = &self.storage else {
            return Err(BucketError::Unsupported(self.storage.provider()));
        };
        let session = fresh_session(
            &mut self.session,
            &self.storage,
            self.password.as_ref(),
            now,
        )
        .await?;
        if let Some(lister) = &session.lister {
            return Ok(Arc::clone(lister));
        }
        let lister = Arc::new(S3Lister::new(s3_location(bucket, &session.signed_in))?);
        session.lister = Some(Arc::clone(&lister));
        Ok(lister)
    }

    /// Makes the sign-in expire at `at`, as temporary credentials do.
    #[cfg(test)]
    pub(crate) fn expire_at(&mut self, at: SystemTime) {
        if let Some(session) = &mut self.session {
            session.signed_in.expires_at = Some(at);
        }
    }
}

/// `session` if it is fresh at `now`, else a new one from signing in again.
async fn fresh_session<'a>(
    session: &'a mut Option<Session>,
    storage: &ObjectStorage,
    password: Option<&Password>,
    now: SystemTime,
) -> Result<&'a mut Session, BucketError> {
    if let Some(current) = session.take()
        && current.signed_in.is_fresh_at(now)
    {
        return Ok(session.insert(current));
    }
    let signed_in = sign_in(storage, password).await?;
    Ok(session.insert(Session {
        signed_in,
        secret_made: false,
        lister: None,
    }))
}

async fn sign_in(
    storage: &ObjectStorage,
    password: Option<&Password>,
) -> Result<SignedIn, BucketError> {
    match storage {
        ObjectStorage::S3(bucket) => sign_in_to_s3(bucket, password).await,
        ObjectStorage::Gcs(_) | ObjectStorage::Azure(_) => {
            Err(BucketError::Unsupported(storage.provider()))
        }
    }
}

async fn sign_in_to_s3(
    bucket: &S3Bucket,
    password: Option<&Password>,
) -> Result<SignedIn, BucketError> {
    let credentials = match &bucket.sign_in {
        S3SignIn::Aws => AwsCredentials::DefaultChain,
        S3SignIn::Profile { profile } => AwsCredentials::Profile(profile.clone()),
        S3SignIn::AccessKey { key_id } => {
            let secret = password.cloned().ok_or(BucketError::NoSecret)?;
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

/// Where the lister sends its requests, and the keys it signs them with: the same as
/// DuckDB's secret.
fn s3_location(bucket: &S3Bucket, signed_in: &SignedIn) -> S3Location {
    let keys = match &signed_in.credentials {
        BucketCredentials::S3Keys {
            key_id,
            secret,
            session_token,
        } => Some(S3Keys {
            key_id: key_id.clone(),
            secret: secret.clone(),
            session_token: session_token.clone(),
        }),
        BucketCredentials::Anonymous => None,
    };
    S3Location {
        bucket: bucket.bucket.clone(),
        region: Some(bucket.region.clone())
            .filter(|region| !region.is_empty())
            .or_else(|| signed_in.region.clone())
            .unwrap_or_default(),
        endpoint: Some(bucket.endpoint.trim().to_owned()).filter(|endpoint| !endpoint.is_empty()),
        keys,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
    fn a_sign_in_is_fresh_until_less_than_the_margin_is_left() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let minutes = |minutes: u64| Duration::from_secs(minutes * 60);
        let cases = [
            (None, true),
            (Some(now + minutes(60)), true),
            (Some(now + minutes(6)), true),
            (Some(now + minutes(5)), false),
            (Some(now - minutes(1)), false),
        ];
        for (expires_at, fresh) in cases {
            let signed_in = SignedIn {
                credentials: BucketCredentials::Anonymous,
                region: None,
                expires_at,
            };
            assert_eq!(signed_in.is_fresh_at(now), fresh, "{expires_at:?}");
        }
    }

    #[tokio::test]
    async fn a_secret_is_made_once_and_again_after_the_sign_in_expires() {
        let mut bucket = access_key(Some("dbrover-test"));
        let now = SystemTime::now();

        let first = bucket.due_secret(now).await.unwrap().unwrap();
        bucket.made();
        let unchanged = bucket.due_secret(now).await.unwrap();
        bucket.expire_at(now);
        let again = bucket.due_secret(now).await.unwrap();

        assert!(first.expose().contains("SECRET 'dbrover-test'"));
        assert_eq!(unchanged, None);
        assert_eq!(again, Some(first));
    }

    #[tokio::test]
    async fn an_access_key_without_its_secret_says_where_to_set_it() {
        let mut bucket = access_key(None);

        let error = bucket.due_secret(SystemTime::now()).await.unwrap_err();

        assert!(matches!(error, BucketError::NoSecret), "{error}");
    }

    #[test]
    fn the_lister_goes_where_duckdb_does_with_the_same_keys() {
        let ObjectStorage::S3(bucket) = access_key(None).storage else {
            unreachable!("access_key makes an S3 bucket");
        };
        let signed_in = SignedIn {
            credentials: BucketCredentials::S3Keys {
                key_id: "dbrover".to_owned(),
                secret: Password::new("dbrover-test"),
                session_token: None,
            },
            region: Some("eu-west-1".to_owned()),
            expires_at: None,
        };

        let location = s3_location(&bucket, &signed_in);

        assert_eq!(location.bucket, "dbrover");
        assert_eq!(location.region, "eu-west-1");
        assert_eq!(location.endpoint.as_deref(), Some("http://localhost:59000"));
        assert_eq!(
            location.keys.map(|keys| keys.key_id).as_deref(),
            Some("dbrover")
        );
    }
}
