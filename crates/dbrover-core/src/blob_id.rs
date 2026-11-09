//! A file's Git blob id: what Git calls one version of a file, computed without Git.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

/// The length of an id as `git log --oneline` abbreviates it by default.
const SHORT_LENGTH: usize = 7;

/// A file's Git blob id: SHA-1 over `blob <length>\0` and the file's bytes, which is what
/// `git hash-object` prints. It names a version inside a repository or out of one, and
/// `git log --find-object=<id>` lists the commits that had exactly that file. DbRover uses it
/// as a file's version, to notice changes other programs made (VC1b), and to name the version
/// of a report each run ran (VC1e).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobId([u8; 20]);

impl BlobId {
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha1::new();
        hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// The first characters, as Git shows a commit's: enough to tell versions apart at a
    /// glance, and accepted by Git wherever the whole id is.
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(SHORT_LENGTH);
        text
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0:?} isn't a Git blob id: 40 hexadecimal digits")]
pub struct InvalidBlobId(String);

impl FromStr for BlobId {
    type Err = InvalidBlobId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidBlobId(text.to_owned());
        if text.len() != 40 || !text.is_ascii() {
            return Err(invalid());
        }
        let mut bytes = [0; 20];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for BlobId {
    type Error = InvalidBlobId;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<BlobId> for String {
    fn from(id: BlobId) -> Self {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_what_git_hash_object_prints() {
        // `printf 'hello\n' | git hash-object --stdin` and `git hash-object /dev/null`.
        let cases = [
            (&b"hello\n"[..], "ce013625030ba8dba906f756967f9e9ca394464a"),
            (&b""[..], "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
        ];
        for (bytes, expected) in cases {
            let id = BlobId::of(bytes);
            assert_eq!(id.to_string(), expected);
            assert_eq!(expected.parse::<BlobId>().unwrap(), id);
        }
        assert_eq!(BlobId::of(b"hello\n").short(), "ce01362");
    }

    #[test]
    fn refuses_text_that_isnt_a_whole_id() {
        for text in [
            "",
            "ce01362",
            "zz013625030ba8dba906f756967f9e9ca394464a",
            &"é".repeat(20),
        ] {
            assert!(text.parse::<BlobId>().is_err(), "{text:?}");
        }
    }
}
