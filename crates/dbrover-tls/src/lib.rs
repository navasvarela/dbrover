//! The rustls client configuration a data source's TLS mode asks for, shared by the drivers
//! that make their own TLS connections (PostgreSQL's, ClickHouse's, Cassandra's): ring as the crypto
//! provider, Mozilla's public authorities or a CA file for `verify-full`, and encryption
//! without authenticating the server for `prefer` and `require`, as libpq's modes of the same
//! names do.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dbrover_core::TlsMode;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, ring};
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

/// Why the TLS settings couldn't be turned into a client configuration, before any network traffic.
/// Each message leaves its cause out: `describe_error` appends the source chain.
#[derive(Debug, thiserror::Error)]
pub enum TlsSetupError {
    #[error("could not read the CA file {}", path.display())]
    ReadCaFile { path: PathBuf, source: io::Error },
    #[error("the CA file {} is not valid PEM", path.display())]
    InvalidPem { path: PathBuf, source: pem::Error },
    #[error("the CA file {} holds no usable certificate", path.display())]
    NoCertificates { path: PathBuf },
    #[error("could not set up TLS")]
    Rustls(#[from] rustls::Error),
}

/// The client configuration for `tls`. `disable` gets one too, for callers that build their
/// connector before knowing whether it will be used; it never starts a handshake.
///
/// # Errors
///
/// [`TlsSetupError`] if `verify-full`'s CA file can't be read or holds no certificate.
pub fn client_config(tls: &TlsMode) -> Result<ClientConfig, TlsSetupError> {
    let provider = Arc::new(ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    Ok(match tls {
        // `prefer` and `require` encrypt without authenticating the server.
        TlsMode::Disable | TlsMode::Prefer | TlsMode::Require => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(EncryptOnly {
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth(),
        TlsMode::VerifyFull { ca_file } => builder
            .with_root_certificates(trusted_roots(ca_file.as_deref())?)
            .with_no_client_auth(),
    })
}

/// [`client_config`], except that `verify-full` checks the certificate against `server_name`
/// whatever name the client gives rustls: for clients that name each node by its IP address,
/// as the `scylla` crate does, which a certificate for the server's host name doesn't hold,
/// and which is the local end of an SSH tunnel.
///
/// # Errors
///
/// As [`client_config`].
pub fn client_config_for(
    tls: &TlsMode,
    server_name: ServerName<'static>,
) -> Result<ClientConfig, TlsSetupError> {
    let TlsMode::VerifyFull { ca_file } = tls else {
        return client_config(tls);
    };
    let provider = Arc::new(ring::default_provider());
    let verifier = WebPkiServerVerifier::builder_with_provider(
        Arc::new(trusted_roots(ca_file.as_deref())?),
        provider.clone(),
    )
    .build()
    .map_err(|error| TlsSetupError::Rustls(rustls::Error::General(error.to_string())))?;
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NamedServer {
            verifier,
            server_name,
        }))
        .with_no_client_auth())
}

/// The authorities in `ca_file`, or the public web ones (Mozilla's list) without one.
fn trusted_roots(ca_file: Option<&Path>) -> Result<RootCertStore, TlsSetupError> {
    let Some(path) = ca_file else {
        return Ok(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        });
    };
    let certificates = CertificateDer::pem_file_iter(path)
        .map_err(|error| pem_error(path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| pem_error(path, error))?;
    let mut roots = RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(certificates);
    if added == 0 {
        return Err(TlsSetupError::NoCertificates {
            path: path.to_owned(),
        });
    }
    Ok(roots)
}

/// Opening the file fails with an I/O error wrapped in `pem::Error`; it reads better unwrapped.
fn pem_error(path: &Path, error: pem::Error) -> TlsSetupError {
    let path = path.to_owned();
    match error {
        pem::Error::Io(source) => TlsSetupError::ReadCaFile { path, source },
        source => TlsSetupError::InvalidPem { path, source },
    }
}

/// Accepts any certificate, but still checks that the server holds its private key, so the
/// session is encrypted to whoever answered. What libpq's `prefer` and `require` do.
#[derive(Debug)]
struct EncryptOnly {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for EncryptOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Verifies a certificate as `verifier` does, against `server_name` rather than the name the
/// client connected with.
#[derive(Debug)]
struct NamedServer {
    verifier: Arc<WebPkiServerVerifier>,
    server_name: ServerName<'static>,
}

impl ServerCertVerifier for NamedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.verifier.verify_server_cert(
            end_entity,
            intermediates,
            &self.server_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier
            .verify_tls12_signature(message, certificate, signature)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier
            .verify_tls13_signature(message, certificate, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn builds_a_configuration_for_every_mode() {
        for tls in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull { ca_file: None },
        ] {
            assert!(client_config(&tls).is_ok(), "{tls:?}");
        }
    }

    #[test]
    fn reports_a_ca_file_it_cannot_use() {
        let mut empty = tempfile::NamedTempFile::new().unwrap();
        empty.write_all(b"not a certificate\n").unwrap();
        let cases = [
            (
                PathBuf::from("/nonexistent/dbrover-ca.pem"),
                "could not read the CA file /nonexistent/dbrover-ca.pem",
            ),
            (empty.path().to_owned(), "holds no usable certificate"),
        ];
        for (path, expected) in cases {
            let tls = TlsMode::VerifyFull {
                ca_file: Some(path),
            };
            let Err(error) = client_config(&tls) else {
                panic!("expected {expected:?}");
            };
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
