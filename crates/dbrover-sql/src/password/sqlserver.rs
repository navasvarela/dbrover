//! SQL Server's password hash, written by DbRover so that `CREATE LOGIN … WITH PASSWORD = …
//! HASHED` carries a hash in place of the password.

use std::fmt;

use rand::Rng;
use sha2::{Digest, Sha512};

use super::mysql::upper_hex;

/// The version SQL Server 2012 and later write before the salt: SHA-512.
const VERSION: [u8; 2] = [0x02, 0x00];

/// A SQL login's password as SQL Server stores it: `0x0200`, a 4-byte salt, then SHA-512 of
/// the password in UTF-16LE followed by the salt. Its `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct SqlServerHash(String);

impl SqlServerHash {
    /// Hashes `password` with a new random salt.
    pub fn hash(password: &str) -> Self {
        let mut salt = [0_u8; 4];
        rand::rng().fill_bytes(&mut salt);
        Self::salted(password, salt)
    }

    fn salted(password: &str, salt: [u8; 4]) -> Self {
        let utf16: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let digest = Sha512::new()
            .chain_update(&utf16)
            .chain_update(salt)
            .finalize();
        let mut bytes = VERSION.to_vec();
        bytes.extend(salt);
        bytes.extend(digest);
        Self(format!("0x{}", upper_hex(&bytes)))
    }

    /// The binary literal `PASSWORD = … HASHED` takes.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SqlServerHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SqlServerHash(<hidden>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL Server 2022 CU28 let a login created with this hash log in with `Other-pw1!X`.
    #[test]
    fn matches_a_hash_sql_server_accepts() {
        assert_eq!(
            SqlServerHash::salted("Other-pw1!X", [0xa1, 0xb2, 0xc3, 0xd4]).as_str(),
            "0x0200A1B2C3D41ECBDD91220A96D66BC6FF85045B6B958B1F4C6EFBCC503124B7B7979F37CE430481\
             42B3AD0B1A8D2585A4C4A02C5881F6AE77832FF5A73D7356ABF9A0890197"
        );
    }

    #[test]
    fn salts_every_hash_afresh() {
        let hash = SqlServerHash::hash("same");
        assert_eq!(hash.as_str().len(), 2 + 2 * (2 + 4 + 64));
        assert_ne!(hash, SqlServerHash::hash("same"));
        assert_eq!(format!("{hash:?}"), "SqlServerHash(<hidden>)");
    }
}
