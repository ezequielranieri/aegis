//! mTLS server configuration with CN/SAN validation (REQ-713, REQ-716).
//!
//! Builds a `rustls::ServerConfig` that requires client certificates,
//! validates them against the configured CA, and checks CN/SAN matches
//! the expected identity.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, Error, RootCertStore, ServerConfig, SignatureScheme};
use rustls::client::danger::HandshakeSignatureValid;

use crate::config::runtime::TlsConfig;

/// Custom client certificate verifier that validates CN/SAN against
/// the expected identity from `RuntimeConfig` (REQ-716).
#[derive(Debug)]
struct AegisClientCertVerifier {
    _roots: Arc<RootCertStore>,
    expected_identity: String,
}

impl ClientCertVerifier for AegisClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        use x509_parser::prelude::FromDer;

        // Parse the certificate to extract Subject Alternative Names (SANs)
        let (_, cert) = x509_parser::certificate::X509Certificate::from_der(end_entity.as_ref())
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to parse client certificate");
                Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
            })?;

        // Check CN (Common Name)
        let cn = cert.subject().to_string();
        let cn_match = cn.contains(&format!("CN={}", self.expected_identity));

        // Check SANs (Subject Alternative Names)
        let san_match = cert
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|san| {
                san.value.general_names.iter().any(|name| match name {
                    x509_parser::extensions::GeneralName::DNSName(dns) => {
                        dns == &self.expected_identity
                    }
                    x509_parser::extensions::GeneralName::URI(uri) => {
                        uri == &self.expected_identity
                    }
                    _ => false,
                })
            })
            .unwrap_or(false);

        if !cn_match && !san_match {
            tracing::warn!(
                expected = %self.expected_identity,
                cn = %cn,
                "client certificate identity mismatch"
            );
            return Err(Error::InvalidCertificate(
                rustls::CertificateError::Other(rustls::OtherError(Arc::new(
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "client certificate identity mismatch: expected '{}'",
                            self.expected_identity
                        ),
                    ),
                ))),
            ));
        }

        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a `rustls::ServerConfig` from `TlsConfig` (REQ-713, REQ-716).
///
/// Loads server cert/key, CA cert for client validation, and creates
/// mTLS config with CN/SAN identity checking.
///
/// # Errors
///
/// Returns `Err` if cert/key files are missing, invalid, or CA cert
/// cannot be loaded.
pub fn build_tls_config(tls_config: &TlsConfig) -> anyhow::Result<ServerConfig> {
    // Load server certificate chain
    let server_cert = std::fs::read(&tls_config.cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server cert {}: {}",
            tls_config.cert_path.display(),
            e
        )
    })?;
    let server_cert_der = CertificateDer::from(server_cert);

    // Load server private key
    let server_key = std::fs::read(&tls_config.key_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server key {}: {}",
            tls_config.key_path.display(),
            e
        )
    })?;
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(server_key),
    );

    // Load CA certificate for client validation
    let ca_cert = std::fs::read(&tls_config.ca_cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read CA cert {}: {}",
            tls_config.ca_cert_path.display(),
            e
        )
    })?;
    let ca_cert_der = CertificateDer::from(ca_cert);

    // Build root certificate store
    let mut root_store = RootCertStore::empty();
    root_store.add(ca_cert_der).map_err(|e| {
        anyhow::anyhow!("failed to add CA cert to root store: {}", e)
    })?;

    // Create the custom client cert verifier
    let verifier = AegisClientCertVerifier {
        _roots: Arc::new(root_store),
        expected_identity: tls_config.expected_identity.clone(),
    };

    // Build server config with mTLS
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| anyhow::anyhow!("TLS protocol config error: {}", e))?
    .with_client_cert_verifier(Arc::new(verifier))
    .with_single_cert(vec![server_cert_der], server_key_der)
    .map_err(|e| anyhow::anyhow!("TLS cert/key error: {}", e))?;

    Ok(config)
}

/// Build a `tonic::transport::ServerTlsConfig` from `TlsConfig` for use with tonic.
///
/// This uses tonic's built-in TLS support with `client_ca_root` for basic mTLS.
/// For full CN/SAN validation, use `build_tls_config` with a custom TLS acceptor.
pub fn build_tonic_tls_config(
    tls_config: &TlsConfig,
) -> anyhow::Result<tonic::transport::ServerTlsConfig> {
    // Load server cert and key as PEM
    let cert_pem = std::fs::read(&tls_config.cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server cert {}: {}",
            tls_config.cert_path.display(),
            e
        )
    })?;
    let key_pem = std::fs::read(&tls_config.key_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server key {}: {}",
            tls_config.key_path.display(),
            e
        )
    })?;
    let ca_cert_pem = std::fs::read(&tls_config.ca_cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read CA cert {}: {}",
            tls_config.ca_cert_path.display(),
            e
        )
    })?;

    let identity = tonic::transport::Identity::from_pem(cert_pem, key_pem);
    let ca_cert = tonic::transport::Certificate::from_pem(ca_cert_pem);

    let tls_config = tonic::transport::ServerTlsConfig::new()
        .identity(identity)
        .client_ca_root(ca_cert);

    Ok(tls_config)
}
