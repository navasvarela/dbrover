//! MySQL's and MariaDB's password hashes, written by DbRover so that `IDENTIFIED WITH … AS`
//! (and MariaDB's `IDENTIFIED BY PASSWORD`) carries a hash in place of the password.

use std::fmt;

use rand::Rng;
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// How many bytes of salt `caching_sha2_password` takes, after `$A$005$`.
const CACHING_SHA2_SALT_LENGTH: usize = 20;
/// `005`: thousands of SHA-256 crypt rounds, the count MySQL writes itself.
const CACHING_SHA2_ROUNDS: u32 = 5_000;
const CACHING_SHA2_PREFIX: &str = "$A$005$";
/// The salt's characters: letters and digits only. MySQL's own salts are any 7-bit byte but
/// `\0` and `$`, which may be unprintable; these keep the hash readable as a plain string
/// literal on every server, whatever `NO_BACKSLASH_ESCAPES` says.
const SALT_CHARACTERS: &[u8; 62] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// SHA-crypt's base-64 alphabet, which isn't RFC 4648's.
const CRYPT_ALPHABET: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A password hashed in one of MySQL's or MariaDB's plugins' forms, as the server stores it.
/// It isn't the password, but whoever holds it can guess the password offline, so its `Debug`
/// hides it.
#[derive(Clone, PartialEq, Eq)]
pub enum AccountHash {
    /// `caching_sha2_password`, MySQL's default since 8.0: `$A$005$`, a 20-byte salt, then
    /// SHA-256 crypt of 5,000 rounds.
    CachingSha2(String),
    /// `mysql_native_password`: `*` and the upper-case hex of `SHA1(SHA1(password))`. MySQL
    /// 8.4 turns the plugin off by default and 9.0 removes it; MariaDB still defaults to it.
    MysqlNative(String),
}

impl AccountHash {
    /// Hashes `password` for `caching_sha2_password`, with a new random salt.
    pub fn caching_sha2(password: &str) -> Self {
        let mut random = [0_u8; CACHING_SHA2_SALT_LENGTH];
        rand::rng().fill_bytes(&mut random);
        let salt = random.map(|byte| SALT_CHARACTERS[usize::from(byte) % SALT_CHARACTERS.len()]);
        Self::caching_sha2_salted(password, &salt)
    }

    fn caching_sha2_salted(password: &str, salt: &[u8; CACHING_SHA2_SALT_LENGTH]) -> Self {
        let digest = sha256_crypt(password.as_bytes(), salt, CACHING_SHA2_ROUNDS);
        let mut hash = String::from(CACHING_SHA2_PREFIX);
        hash.extend(salt.iter().map(|&byte| char::from(byte)));
        hash.push_str(&digest);
        Self::CachingSha2(hash)
    }

    pub fn mysql_native(password: &str) -> Self {
        let twice = Sha1::digest(Sha1::digest(password.as_bytes()));
        Self::MysqlNative(format!("*{}", upper_hex(&twice)))
    }

    /// The hash as `AS '…'` takes it: printable ASCII, with no quote or backslash.
    pub fn as_str(&self) -> &str {
        match self {
            Self::CachingSha2(hash) | Self::MysqlNative(hash) => hash,
        }
    }

    /// The plugin that reads it.
    pub fn plugin(&self) -> &'static str {
        match self {
            Self::CachingSha2(_) => "caching_sha2_password",
            Self::MysqlNative(_) => "mysql_native_password",
        }
    }
}

impl fmt::Debug for AccountHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountHash({}, <hidden>)", self.plugin())
    }
}

pub(super) fn upper_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// SHA-256 crypt, the digest part only (43 characters), for a salt of any length.
///
/// Follows Ulrich Drepper's specification, <https://www.akkadia.org/drepper/SHA-crypt.txt>,
/// steps 1 to 22, with one difference MySQL makes: the salt isn't cut to 16 bytes, since
/// `caching_sha2_password` salts with 20. The rounds and salt aren't written into the result;
/// MySQL's `$A$005$` prefix says them instead.
fn sha256_crypt(password: &[u8], salt: &[u8], rounds: u32) -> String {
    // Steps 4–8: digest B, of password, salt, password.
    let alternate = Sha256::new()
        .chain_update(password)
        .chain_update(salt)
        .chain_update(password)
        .finalize();
    // Steps 1–3 and 9–12: digest A, of password, salt, B repeated over the password's
    // length, then B or the password for each bit of that length.
    let mut first = Sha256::new().chain_update(password).chain_update(salt);
    for chunk in password.chunks(alternate.len()) {
        first.update(&alternate[..chunk.len()]);
    }
    let mut length = password.len();
    while length > 0 {
        if length & 1 == 1 {
            first.update(alternate);
        } else {
            first.update(password);
        }
        length >>= 1;
    }
    let mut digest = first.finalize();
    // Steps 13–16: P, the password's digest repeated to its length.
    let mut password_digest = Sha256::new();
    for _ in 0..password.len() {
        password_digest.update(password);
    }
    let password_digest = password_digest.finalize();
    let p_sequence = repeated(&password_digest, password.len());
    // Steps 17–20: S, the salt's digest repeated 16 + A[0] times, cut to the salt's length.
    let mut salt_digest = Sha256::new();
    for _ in 0..16 + usize::from(digest[0]) {
        salt_digest.update(salt);
    }
    let salt_digest = salt_digest.finalize();
    let s_sequence = repeated(&salt_digest, salt.len());
    // Step 21: the rounds.
    for round in 0..rounds {
        let mut next = Sha256::new();
        if round % 2 == 1 {
            next.update(&p_sequence);
        } else {
            next.update(digest);
        }
        if round % 3 != 0 {
            next.update(&s_sequence);
        }
        if round % 7 != 0 {
            next.update(&p_sequence);
        }
        if round % 2 == 1 {
            next.update(digest);
        } else {
            next.update(&p_sequence);
        }
        digest = next.finalize();
    }
    crypt_base64(&digest)
}

/// `digest` repeated, then cut to `length` bytes.
fn repeated(digest: &[u8], length: usize) -> Vec<u8> {
    digest.iter().copied().cycle().take(length).collect()
}

/// Step 22: the digest's bytes, taken in the specification's order three at a time, written
/// in [`CRYPT_ALPHABET`] six bits at a time, least significant first.
fn crypt_base64(digest: &[u8]) -> String {
    const GROUPS: [(usize, usize, usize); 10] = [
        (0, 10, 20),
        (21, 1, 11),
        (12, 22, 2),
        (3, 13, 23),
        (24, 4, 14),
        (15, 25, 5),
        (6, 16, 26),
        (27, 7, 17),
        (18, 28, 8),
        (9, 19, 29),
    ];
    let mut text = String::with_capacity(43);
    let mut push = |high: u8, middle: u8, low: u8, characters: usize| {
        let mut bits = (u32::from(high) << 16) | (u32::from(middle) << 8) | u32::from(low);
        for _ in 0..characters {
            text.push(char::from(CRYPT_ALPHABET[(bits & 0x3f) as usize]));
            bits >>= 6;
        }
    };
    for (high, middle, low) in GROUPS {
        push(digest[high], digest[middle], digest[low], 4);
    }
    push(0, digest[31], digest[30], 3);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 crypt with its default 5,000 rounds and a salt of 16 bytes or fewer, which
    /// MySQL's longer salt aside is the standard one: as `openssl passwd -5 -salt saltstring
    /// 'Hello world!'` computes it (OpenSSL 4.1).
    #[test]
    fn sha256_crypt_matches_the_specification() {
        assert_eq!(
            sha256_crypt(b"Hello world!", b"saltstring", 5_000),
            "5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5"
        );
    }

    /// A hash MySQL 8.4.13 made for `S3cret-pw` (`CREATE USER … IDENTIFIED BY`), whose salt
    /// holds unprintable bytes as MySQL's own salts may.
    #[test]
    fn caching_sha2_matches_what_mysql_makes() {
        let salt: [u8; 20] = *b"e63x\x04\x03F!\x01/uMONX5\x1f8=T";
        let AccountHash::CachingSha2(hash) = AccountHash::caching_sha2_salted("S3cret-pw", &salt)
        else {
            panic!("not a caching_sha2_password hash");
        };
        assert_eq!(
            hash.as_bytes()[27..],
            *b"56WLYTd3pWy2lBzNfq9jCqwQfO.B0TvRb/6d6gsRsw."
        );
        assert!(hash.starts_with("$A$005$"));
    }

    #[test]
    fn caching_sha2_salts_with_letters_and_digits_afresh() {
        let hash = AccountHash::caching_sha2("same");
        let text = hash.as_str();
        assert_eq!(text.len(), 7 + 20 + 43);
        assert!(text[7..27].bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!(
            text.bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'\'' && byte != b'\\'),
            "{text}"
        );
        assert_ne!(hash, AccountHash::caching_sha2("same"));
    }

    /// What MariaDB 11.4.13 stored for `S3cret-pw` (`IDENTIFIED BY`).
    #[test]
    fn mysql_native_matches_what_mariadb_makes() {
        assert_eq!(
            AccountHash::mysql_native("S3cret-pw").as_str(),
            "*C4436B3D4764E76D1CB42FF3438FBFB8FA7EA7C4"
        );
    }

    #[test]
    fn debug_hides_the_hash() {
        assert_eq!(
            format!("{:?}", AccountHash::mysql_native("x")),
            "AccountHash(mysql_native_password, <hidden>)"
        );
    }
}
