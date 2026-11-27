//! Passwords in DDL, hashed on the client before a statement holds one.

mod mysql;
mod sqlserver;

use std::fmt;

pub use mysql::AccountHash;
pub use sqlserver::SqlServerHash;

/// A password hashed for PostgreSQL's SCRAM-SHA-256 authentication: the verifier the server
/// stores in its place, `SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>`.
/// `CREATE ROLE … PASSWORD` accepts it instead of the password itself (PostgreSQL 10 and
/// later), so the password never leaves the client.
///
/// It isn't the password, but it lets whoever holds it guess the password offline, so its
/// `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct ScramVerifier(String);

impl ScramVerifier {
    /// Hashes `password` with a new random salt, as libpq's `PQencryptPasswordConn` does:
    /// SASLprep first, then 4096 iterations of PBKDF2 with HMAC-SHA-256.
    pub fn hash(password: &str) -> Self {
        Self(postgres_protocol::password::scram_sha_256(
            password.as_bytes(),
        ))
    }

    /// The verifier as `PASSWORD` takes it. It is ASCII letters, digits and `$:+/=` only, so
    /// it needs no escaping inside a string literal.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A verifier computed elsewhere, for golden tests that need the same text every run.
    #[cfg(test)]
    pub(crate) fn fixed(verifier: &str) -> Self {
        Self(verifier.to_owned())
    }
}

impl fmt::Debug for ScramVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScramVerifier(<hidden>)")
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn debug_hides_the_verifier() {
        let verifier = ScramVerifier::hash("hunter2");
        assert_eq!(format!("{verifier:?}"), "ScramVerifier(<hidden>)");
    }

    /// Hi() from RFC 5802, PBKDF2 with HMAC-SHA-256 and one block of output, written out here
    /// so the hashing is checked against something other than itself.
    fn salted_password(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
        let mac = |data: &[&[u8]]| {
            let mut mac = Hmac::<Sha256>::new_from_slice(password.as_bytes()).unwrap();
            for part in data {
                mac.update(part);
            }
            <[u8; 32]>::from(mac.finalize().into_bytes())
        };
        let mut block = mac(&[salt, &1u32.to_be_bytes()]);
        let mut result = block;
        for _ in 1..iterations {
            block = mac(&[&block]);
            for (byte, next) in result.iter_mut().zip(block) {
                *byte ^= next;
            }
        }
        result
    }

    fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    /// The client key, and the stored key and server key a verifier for `password` and
    /// `salt` must hold.
    fn keys(password: &str, salt: &[u8], iterations: u32) -> ([u8; 32], [u8; 32], [u8; 32]) {
        let salted = salted_password(password, salt, iterations);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key: [u8; 32] = Sha256::digest(client_key).into();
        (client_key, stored_key, hmac(&salted, b"Server Key"))
    }

    /// The exchange in RFC 7677 § 3, which proves `keys` computes what a SCRAM-SHA-256 server
    /// stores: the client's proof and the server's signature come out as published.
    #[test]
    fn the_reference_keys_match_rfc_7677() {
        let salt = STANDARD.decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        let (client_key, stored_key, server_key) = keys("pencil", &salt, 4096);
        let auth_message = "n=user,r=rOprNGfwEbeRWgbNEkqO,\
            r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
            s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096,\
            c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
        let server_signature = hmac(&server_key, auth_message.as_bytes());
        assert_eq!(
            STANDARD.encode(server_signature),
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
        let client_signature = hmac(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature)
            .map(|(key, signature)| key ^ signature)
            .collect();
        assert_eq!(
            STANDARD.encode(proof),
            "dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
    }

    #[test]
    fn hashes_a_password_into_a_verifier_that_proves_it() {
        let verifier = ScramVerifier::hash("correct horse");
        let text = verifier.as_str();
        let (scheme, rest) = text.split_once('$').unwrap();
        let (parameters, keys_text) = rest.split_once('$').unwrap();
        let (iterations, salt) = parameters.split_once(':').unwrap();
        let (stored_key, server_key) = keys_text.split_once(':').unwrap();
        assert_eq!(scheme, "SCRAM-SHA-256");
        assert_eq!(iterations, "4096");
        let salt = STANDARD.decode(salt).unwrap();
        assert_eq!(salt.len(), 16);
        let (_, expected_stored_key, expected_server_key) = keys("correct horse", &salt, 4096);
        assert_eq!(STANDARD.decode(stored_key).unwrap(), expected_stored_key);
        assert_eq!(STANDARD.decode(server_key).unwrap(), expected_server_key);
        let (_, other_stored_key, _) = keys("correct horsf", &salt, 4096);
        assert_ne!(other_stored_key, expected_stored_key);
        assert!(
            text.chars()
                .all(|c| c.is_ascii_alphanumeric() || "$:+/=-".contains(c)),
            "{text}"
        );
    }

    #[test]
    fn salts_every_hash_afresh() {
        assert_ne!(ScramVerifier::hash("same"), ScramVerifier::hash("same"));
    }
}
