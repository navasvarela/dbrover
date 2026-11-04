//! Listing a bucket a level at a time (C2, `docs/cloud-data.md`): what DuckDB can't do, since
//! its `glob` lists every key under a prefix and no folders. Each provider's own listing, with
//! `/` as the delimiter, gives a level's folders and files a page at a time, so a bucket of
//! millions of keys loads only the levels that are expanded.

mod s3;
mod xml;

pub use s3::{S3Keys, S3Lister, S3Location};

use crate::HttpError;

/// The folders and files directly under a prefix, in the order the provider lists them (by
/// key), and where the level continues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Level {
    /// The prefixes of the keys one level down, each ending in `/`.
    pub folders: Vec<String>,
    pub files: Vec<StoredFile>,
    /// The provider's token for the rest of the level, when it has more.
    pub next: Option<String>,
}

/// An object, by its whole key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub key: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Http(#[from] HttpError),
    /// The provider refused the request, with its own code and message.
    #[error("{message} ({code}, HTTP {status})")]
    Refused {
        status: u16,
        code: String,
        message: String,
    },
    #[error("the bucket's listing couldn't be read: {0}")]
    Unreadable(String),
    #[error("the request couldn't be signed: {0}")]
    Signing(String),
}
