//! Signing in to Snowflake's SQL API (`docs/cloud-data.md`, C5c): a JWT DbRover signs with
//! the user's RSA key, or a token Snowflake issued (a programmatic access token, or an OAuth
//! access token), each sent as a bearer token with the header that names its kind.
//!
//! The key pair is Snowflake's own scheme rather than a cloud's: the user registers the public
//! half on their Snowflake user (`ALTER USER … SET RSA_PUBLIC_KEY`), and every request carries
//! a JWT signed with the private half that names the account, the user and the public key's
//! fingerprint. Nothing is asked of Snowflake to make one, so a JWT is made on the machine,
//! for an hour, and replaced through a [`TokenCache`] as a fetched token is.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use dbrover_core::Password;
use pkcs8::EncryptedPrivateKeyInfoRef;
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

use crate::{AccessToken, TokenCache};

/// How long a JWT is valid. Snowflake refuses one that expires more than an hour after it was
/// issued; the cache replaces it five minutes before it expires.
const JWT_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// What Snowflake is told a bearer token is, in `X-Snowflake-Authorization-Token-Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    /// A JWT signed with the user's key.
    KeyPairJwt,
    /// An access token from Snowflake OAuth or an external identity provider.
    OAuth,
    /// A token generated in Snowflake for the user (`ALTER USER … ADD PROGRAMMATIC ACCESS
    /// TOKEN`).
    ProgrammaticAccessToken,
}

impl TokenType {
    /// The header's value, as Snowflake's reference spells it.
    pub fn header_value(self) -> &'static str {
        match self {
            Self::KeyPairJwt => "KEYPAIR_JWT",
            Self::OAuth => "OAUTH",
            Self::ProgrammaticAccessToken => "PROGRAMMATIC_ACCESS_TOKEN",
        }
    }
}

/// Why a Snowflake sign-in couldn't make or find its token.
#[derive(Debug, thiserror::Error)]
pub enum SnowflakeError {
    #[error("Couldn't read the private key file {}: {reason}", path.display())]
    KeyFile { path: PathBuf, reason: String },
    #[error(
        "The private key file {} is encrypted: give its passphrase in the data source's settings",
        path.display()
    )]
    PassphraseNeeded { path: PathBuf },
    #[error("The passphrase doesn't decrypt the private key file {}", path.display())]
    WrongPassphrase { path: PathBuf },
    #[error(
        "The private key file {} is encrypted in a way DbRover doesn't read ({reason}). \
         Re-encrypt it with AES: openssl pkcs8 -topk8 -v2 aes-256-cbc -in <this file> -out \
         <a new file>",
        path.display()
    )]
    UnreadableEncryption { path: PathBuf, reason: String },
    #[error("Couldn't sign the request to Snowflake with the private key")]
    Signing,
}

/// A Snowflake user's RSA key, read from its file, and the fingerprint Snowflake knows its
/// public half by. Its `Debug` shows the fingerprint only.
pub struct KeyPair {
    key: RsaKeyPair,
    /// `SHA256:` and the base64 of the SHA-256 of the public key's `SubjectPublicKeyInfo`, as
    /// `DESC USER` shows it in `RSA_PUBLIC_KEY_FP`.
    fingerprint: String,
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPair")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl KeyPair {
    /// Reads the key in the PEM file at `path`: PKCS #8, as Snowflake's guide writes it,
    /// encrypted with `passphrase` or not, or PKCS #1 (`RSA PRIVATE KEY`).
    ///
    /// # Errors
    ///
    /// [`SnowflakeError`] naming the file and what to do: it can't be read, isn't an RSA key,
    /// needs a passphrase, or is encrypted with a cipher DbRover doesn't read (DES3).
    pub fn read(path: &Path, passphrase: Option<&Password>) -> Result<Self, SnowflakeError> {
        let pem = std::fs::read_to_string(path).map_err(|error| SnowflakeError::KeyFile {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
        Self::from_pem(&pem, passphrase).map_err(|failure| failure.in_file(path))
    }

    fn from_pem(pem: &str, passphrase: Option<&Password>) -> Result<Self, KeyFailure> {
        let block = Pem::parse(pem)?;
        let key = match block.label.as_str() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&block.der),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(&block.der),
            "ENCRYPTED PRIVATE KEY" => {
                let passphrase = passphrase.ok_or(KeyFailure::PassphraseNeeded)?;
                let der = decrypt(&block.der, passphrase)?;
                RsaKeyPair::from_pkcs8(&der)
            }
            other => return Err(KeyFailure::Unreadable(format!("it holds a {other}"))),
        }
        .map_err(|error| KeyFailure::Unreadable(format!("it isn't an RSA key: {error}")))?;
        let fingerprint = fingerprint(&key);
        Ok(Self { key, fingerprint })
    }

    /// `SHA256:…`, which Snowflake compares with the public key registered on the user.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SnowflakeError> {
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                message,
                &mut signature,
            )
            .map_err(|_| SnowflakeError::Signing)?;
        Ok(signature)
    }
}

/// Why a key's text couldn't be used, before the file's name is added.
#[derive(Debug)]
enum KeyFailure {
    Unreadable(String),
    PassphraseNeeded,
    WrongPassphrase,
    UnreadableEncryption(String),
}

impl KeyFailure {
    fn in_file(self, path: &Path) -> SnowflakeError {
        let path = path.to_owned();
        match self {
            Self::Unreadable(reason) => SnowflakeError::KeyFile { path, reason },
            Self::PassphraseNeeded => SnowflakeError::PassphraseNeeded { path },
            Self::WrongPassphrase => SnowflakeError::WrongPassphrase { path },
            Self::UnreadableEncryption(reason) => {
                SnowflakeError::UnreadableEncryption { path, reason }
            }
        }
    }
}

/// One PEM block: its label and its bytes.
struct Pem {
    label: String,
    der: Vec<u8>,
}

impl Pem {
    /// The first block of `text`. An OpenSSL key encrypted in its traditional form carries
    /// `Proc-Type` and `DEK-Info` headers, which are refused: only PKCS #8 is decrypted.
    fn parse(text: &str) -> Result<Self, KeyFailure> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .skip_while(|line| line.is_empty());
        let label = lines
            .next()
            .and_then(|line| line.strip_prefix("-----BEGIN "))
            .and_then(|line| line.strip_suffix("-----"))
            .ok_or_else(|| KeyFailure::Unreadable("it isn't a PEM file".to_owned()))?
            .to_owned();
        let mut body = String::new();
        for line in lines {
            if line.starts_with("-----END ") {
                break;
            }
            if line.starts_with("Proc-Type:") {
                return Err(KeyFailure::UnreadableEncryption(
                    "OpenSSL's traditional encryption".to_owned(),
                ));
            }
            body.push_str(line);
        }
        let der = STANDARD
            .decode(body)
            .map_err(|error| KeyFailure::Unreadable(format!("it isn't PEM: {error}")))?;
        Ok(Self { label, der })
    }
}

/// The PKCS #8 key inside an encrypted one, through PBES2 (AES, with PBKDF2 or scrypt).
fn decrypt(der: &[u8], passphrase: &Password) -> Result<Vec<u8>, KeyFailure> {
    let encrypted = EncryptedPrivateKeyInfoRef::try_from(der)
        .map_err(|error| KeyFailure::UnreadableEncryption(error.to_string()))?;
    match encrypted.decrypt(passphrase.expose()) {
        Ok(document) => Ok(document.as_bytes().to_vec()),
        Err(pkcs8::Error::EncryptedPrivateKey(pkcs8::pkcs5::Error::DecryptFailed)) => {
            Err(KeyFailure::WrongPassphrase)
        }
        // A wrong passphrase can also decrypt to bytes that happen to pass the padding check
        // but aren't a key.
        Err(pkcs8::Error::Asn1(_)) => Err(KeyFailure::WrongPassphrase),
        Err(error) => Err(KeyFailure::UnreadableEncryption(error.to_string())),
    }
}

/// The fingerprint of `key`'s public half: the SHA-256 of its DER `SubjectPublicKeyInfo`, in
/// base64, after `SHA256:`.
fn fingerprint(key: &RsaKeyPair) -> String {
    let info = subject_public_key_info(key.public().as_ref());
    format!("SHA256:{}", STANDARD.encode(digest(&SHA256, &info)))
}

/// `public_key` (PKCS #1's `RSAPublicKey`, which ring gives) inside an X.509
/// `SubjectPublicKeyInfo`, which is what Snowflake fingerprints:
///
/// ```text
/// SEQUENCE {
///   SEQUENCE { OBJECT IDENTIFIER rsaEncryption, NULL }
///   BIT STRING { 0 unused bits, RSAPublicKey }
/// }
/// ```
fn subject_public_key_info(public_key: &[u8]) -> Vec<u8> {
    /// The DER of `SEQUENCE { OID 1.2.840.113549.1.1.1, NULL }`.
    const RSA_ALGORITHM: [u8; 15] = [
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let mut bits = vec![0x00];
    bits.extend_from_slice(public_key);
    let mut body = RSA_ALGORITHM.to_vec();
    body.extend(der_element(0x03, &bits));
    der_element(0x30, &body)
}

/// A DER element: its tag, its length (short form below 128 bytes, long form above) and
/// `content`.
fn der_element(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut element = vec![tag];
    match u8::try_from(content.len()) {
        Ok(short) if short < 0x80 => element.push(short),
        _ => {
            let length = content.len().to_be_bytes();
            let significant: Vec<u8> = length.iter().copied().skip_while(|&b| b == 0).collect();
            // At most eight bytes of length, so the count fits in the low seven bits.
            element.push(0x80 | significant.len() as u8);
            element.extend(significant);
        }
    }
    element.extend_from_slice(content);
    element
}

/// The account as a JWT names it: the identifier's first part, without the region a locator
/// may carry (`xy12345.us-east-2.aws` is `XY12345`), in upper case. Snowflake's own
/// connectors do the same.
pub fn jwt_account(account: &str) -> String {
    account
        .trim()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_uppercase()
}

/// A user signing in with a key pair: makes the JWTs and keeps the last one.
#[derive(Debug)]
pub struct KeyPairSignIn {
    /// `ACCOUNT.USER`, the JWT's subject.
    subject: String,
    key: KeyPair,
    cache: TokenCache,
}

impl KeyPairSignIn {
    /// Signs in as `user` of `account` (the identifier in Snowflake's host name) with `key`.
    pub fn new(account: &str, user: &str, key: KeyPair) -> Self {
        Self {
            subject: format!("{}.{}", jwt_account(account), user.trim().to_uppercase()),
            key,
            cache: TokenCache::default(),
        }
    }

    /// A JWT valid for at least five more minutes: the last one, or a new one.
    ///
    /// # Errors
    ///
    /// [`SnowflakeError::Signing`] when the key can't sign.
    pub async fn token(&self) -> Result<AccessToken, SnowflakeError> {
        self.cache
            .token(|| async { self.jwt_at(SystemTime::now()) })
            .await
    }

    /// The JWT Snowflake's key-pair authentication asks for, issued at `now`: RS256, issued by
    /// `ACCOUNT.USER.SHA256:<fingerprint>` for the subject `ACCOUNT.USER`, for an hour.
    fn jwt_at(&self, now: SystemTime) -> Result<AccessToken, SnowflakeError> {
        let issued_at = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
        let claims = serde_json::json!({
            "iss": format!("{}.{}", self.subject, self.key.fingerprint()),
            "sub": self.subject,
            "iat": issued_at,
            "exp": issued_at + JWT_LIFETIME.as_secs(),
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self.key.sign(signed.as_bytes())?;
        Ok(AccessToken {
            secret: Password::new(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))),
            expires_at: now + JWT_LIFETIME,
        })
    }
}

/// How a Snowflake data source signs in, once its secrets are in hand.
#[derive(Debug)]
pub enum SnowflakeSignIn {
    /// A JWT made from the user's key. Boxed: the key and its cache are larger than a token.
    KeyPair(Box<KeyPairSignIn>),
    /// A token Snowflake or an identity provider issued, sent as it is.
    Token { kind: TokenType, secret: Password },
}

impl SnowflakeSignIn {
    /// The bearer token for the next request, and what kind it is.
    ///
    /// # Errors
    ///
    /// [`SnowflakeError::Signing`] when a key pair's JWT can't be signed.
    pub async fn bearer(&self) -> Result<(Password, TokenType), SnowflakeError> {
        match self {
            Self::KeyPair(key_pair) => Ok((key_pair.token().await?.secret, TokenType::KeyPairJwt)),
            Self::Token { kind, secret } => Ok((secret.clone(), *kind)),
        }
    }
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    const KEY: &str = include_str!("../testdata/snowflake/key.p8");
    const KEY_AES: &str = include_str!("../testdata/snowflake/key-aes.p8");
    const KEY_SCRYPT: &str = include_str!("../testdata/snowflake/key-scrypt.p8");
    const KEY_DES3: &str = include_str!("../testdata/snowflake/key-des3.p8");
    const KEY_PKCS1: &str = include_str!("../testdata/snowflake/key-pkcs1.pem");
    const KEY_EC: &str = include_str!("../testdata/snowflake/ec-key.p8");
    /// What `openssl rsa -in key.p8 -pubout -outform DER | openssl dgst -sha256 -binary |
    /// openssl enc -base64` prints for the test key.
    const FINGERPRINT: &str = "SHA256:qlxIBmpm0dI1cnws2N11/8jsCrfLPEqNCdHES54RIXk=";
    const PASSPHRASE: &str = "correct-horse";

    fn passphrase(text: &str) -> Option<Password> {
        Some(Password::new(text))
    }

    fn claims_of(jwt: &str) -> serde_json::Value {
        let claims = jwt.split('.').nth(1).expect("a JWT has claims");
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).expect("base64"))
            .expect("JSON claims")
    }

    #[test]
    fn every_kind_of_key_file_gives_the_same_fingerprint_as_openssl() {
        for (pem, phrase) in [
            (KEY, None),
            (KEY_PKCS1, None),
            (KEY_AES, passphrase(PASSPHRASE)),
            (KEY_SCRYPT, passphrase(PASSPHRASE)),
        ] {
            let key = KeyPair::from_pem(pem, phrase.as_ref()).expect("the key reads");
            assert_eq!(key.fingerprint(), FINGERPRINT);
        }
    }

    #[test]
    fn a_jwt_carries_the_documented_claims_and_verifies_with_the_public_key() {
        let key = KeyPair::from_pem(KEY, None).expect("the key reads");
        let public = key.key.public().as_ref().to_vec();
        let sign_in = KeyPairSignIn::new("myorg-analytics", "jane.doe", key);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let token = sign_in.jwt_at(now).expect("a JWT");

        let jwt = token.secret.expose();
        assert_eq!(
            claims_of(jwt),
            serde_json::json!({
                "iss": format!("MYORG-ANALYTICS.JANE.DOE.{FINGERPRINT}"),
                "sub": "MYORG-ANALYTICS.JANE.DOE",
                "iat": 1_800_000_000,
                "exp": 1_800_003_600,
            })
        );
        assert_eq!(token.expires_at, now + Duration::from_secs(3600));
        let (signed, signature) = jwt.rsplit_once('.').expect("a signature");
        let header: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(signed.split('.').next().expect("a header"))
                .expect("base64"),
        )
        .expect("JSON");
        assert_eq!(header["alg"], "RS256");
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
            .verify(
                signed.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).expect("base64"),
            )
            .expect("the signature verifies");
    }

    #[test]
    fn the_account_loses_its_region_and_goes_to_upper_case() {
        for (account, named) in [
            ("myorg-myaccount", "MYORG-MYACCOUNT"),
            ("xy12345.us-east-2.aws", "XY12345"),
            ("xy12345", "XY12345"),
            (" acme-marketing_test ", "ACME-MARKETING_TEST"),
        ] {
            assert_eq!(jwt_account(account), named);
        }
    }

    #[tokio::test]
    async fn a_jwt_is_reused_until_it_is_about_to_expire() {
        let key = KeyPair::from_pem(KEY, None).expect("the key reads");
        let sign_in = SnowflakeSignIn::KeyPair(Box::new(KeyPairSignIn::new("acct", "me", key)));
        let (first, kind) = sign_in.bearer().await.expect("a JWT");
        let (second, _) = sign_in.bearer().await.expect("a JWT");
        assert_eq!(kind, TokenType::KeyPairJwt);
        assert_eq!(first, second);
        let token = SnowflakeSignIn::Token {
            kind: TokenType::ProgrammaticAccessToken,
            secret: Password::new("pat-secret"),
        };
        let (secret, kind) = token.bearer().await.expect("the token");
        assert_eq!(secret.expose(), "pat-secret");
        assert_eq!(kind.header_value(), "PROGRAMMATIC_ACCESS_TOKEN");
    }

    #[test]
    fn keys_dbrover_cant_use_say_what_to_do() {
        let path = Path::new("/keys/rsa_key.p8");
        let message = |pem: &str, phrase: Option<&str>| {
            let phrase = phrase.map(Password::new);
            KeyPair::from_pem(pem, phrase.as_ref())
                .expect_err("refused")
                .in_file(path)
                .to_string()
        };
        assert_eq!(
            message(KEY_AES, None),
            "The private key file /keys/rsa_key.p8 is encrypted: give its passphrase in the \
             data source's settings"
        );
        assert_eq!(
            message(KEY_AES, Some("wrong")),
            "The passphrase doesn't decrypt the private key file /keys/rsa_key.p8"
        );
        let des3 = message(KEY_DES3, Some(PASSPHRASE));
        assert!(
            des3.contains("Re-encrypt it with AES: openssl pkcs8 -topk8 -v2 aes-256-cbc"),
            "{des3}"
        );
        let ec = message(KEY_EC, None);
        assert!(ec.contains("it isn't an RSA key"), "{ec}");
        let text = message("not a key", None);
        assert!(text.ends_with("it isn't a PEM file"), "{text}");
    }

    #[test]
    fn a_missing_file_is_named() {
        let error = KeyPair::read(Path::new("/nowhere/rsa_key.p8"), None).expect_err("missing");
        assert!(
            error
                .to_string()
                .starts_with("Couldn't read the private key file /nowhere/rsa_key.p8: "),
            "{error}"
        );
    }

    #[test]
    fn debug_shows_no_secret() {
        let key = KeyPair::from_pem(KEY, None).expect("the key reads");
        let sign_in = SnowflakeSignIn::KeyPair(Box::new(KeyPairSignIn::new("acct", "me", key)));
        let shown = format!("{sign_in:?}");
        assert!(shown.contains(FINGERPRINT), "{shown}");
        assert!(!shown.contains("PRIVATE"), "{shown}");
        let token = SnowflakeSignIn::Token {
            kind: TokenType::OAuth,
            secret: Password::new("oauth-secret"),
        };
        assert!(!format!("{token:?}").contains("oauth-secret"));
    }
}
