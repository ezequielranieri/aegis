//! mTLS server configuration with CN/SAN validation (REQ-713, REQ-716).
//!
//! Builds a `rustls::ServerConfig` that requires client certificates,
//! validates them against the configured CA, and checks CN/SAN matches
//! the expected identity.

use std::io::BufReader;
use std::sync::Arc;

use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::WebPkiClientVerifier;
use rustls::{
    DigitallySignedStruct, DistinguishedName, Error, RootCertStore, ServerConfig, SignatureScheme,
};

use crate::config::runtime::TlsConfig;

/// Custom client certificate verifier that first runs the standard rustls
/// chain validation against the configured CA and then checks CN/SAN against
/// the expected identity from `RuntimeConfig` (REQ-713, REQ-716).
///
/// rustls delegates the whole client-certificate decision to a custom
/// `ClientCertVerifier`, so the CA chain is *not* validated implicitly —
/// delegating to `WebPkiClientVerifier` first is what enforces "signed by
/// the configured CA" (AD-012).
#[derive(Debug)]
struct AegisClientCertVerifier {
    standard: Arc<dyn ClientCertVerifier>,
    expected_identity: String,
}

impl ClientCertVerifier for AegisClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        self.standard.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.standard.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.standard.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        // 1. Standard chain validation against the configured CA (AD-012):
        //    signature chain, validity period, EKU. This rejects certificates
        //    not issued by `ca_cert_path`, including self-signed lookalikes.
        self.standard
            .verify_client_cert(end_entity, intermediates, now)
            .map_err(|e| {
                tracing::warn!(
                    error = %e,
                    "client certificate failed CA chain validation"
                );
                e
            })?;

        // 2. CN/SAN identity check (REQ-716)
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
            return Err(Error::InvalidCertificate(rustls::CertificateError::Other(
                rustls::OtherError(Arc::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "client certificate identity mismatch: expected '{}'",
                        self.expected_identity
                    ),
                ))),
            )));
        }

        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.standard.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.standard.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.standard.supported_verify_schemes()
    }
}

/// Build a `rustls::ServerConfig` from `TlsConfig` (REQ-713, REQ-716).
///
/// Loads the PEM server cert/key chain, the PEM CA cert for client
/// validation, and creates an mTLS config with CN/SAN identity checking.
///
/// # Errors
///
/// Returns `Err` if cert/key files are missing, invalid, or CA cert
/// cannot be loaded.
pub fn build_tls_config(tls_config: &TlsConfig) -> anyhow::Result<ServerConfig> {
    // Load server certificate chain (PEM)
    let server_cert_file = std::fs::File::open(&tls_config.cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server cert {}: {}",
            tls_config.cert_path.display(),
            e
        )
    })?;
    let mut server_cert_reader = BufReader::new(server_cert_file);
    let server_cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut server_cert_reader)
            .collect::<Result<_, _>>()
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse server cert {}: {}",
                    tls_config.cert_path.display(),
                    e
                )
            })?;
    if server_cert_chain.is_empty() {
        return Err(anyhow::anyhow!(
            "no certificates found in server cert {}",
            tls_config.cert_path.display()
        ));
    }

    // Load server private key (PEM PKCS8/PKCS1/SEC1)
    let server_key_file = std::fs::File::open(&tls_config.key_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read server key {}: {}",
            tls_config.key_path.display(),
            e
        )
    })?;
    let mut server_key_reader = BufReader::new(server_key_file);
    let server_key_der = rustls_pemfile::private_key(&mut server_key_reader)
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to parse server key {}: {}",
                tls_config.key_path.display(),
                e
            )
        })?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no private key found in server key {}",
                tls_config.key_path.display()
            )
        })?;

    // Load CA certificate for client validation (PEM)
    let ca_cert_file = std::fs::File::open(&tls_config.ca_cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read CA cert {}: {}",
            tls_config.ca_cert_path.display(),
            e
        )
    })?;
    let mut ca_cert_reader = BufReader::new(ca_cert_file);
    let ca_cert_der = rustls_pemfile::certs(&mut ca_cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to parse CA cert {}: {}",
                tls_config.ca_cert_path.display(),
                e
            )
        })?
        .into_iter()
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no certificates found in CA cert {}",
                tls_config.ca_cert_path.display()
            )
        })?;

    // Build root certificate store
    let mut root_store = RootCertStore::empty();
    root_store
        .add(ca_cert_der)
        .map_err(|e| anyhow::anyhow!("failed to add CA cert to root store: {}", e))?;
    let root_store = Arc::new(root_store);

    // Standard chain validation (signature, validity, EKU) against the CA
    let standard = WebPkiClientVerifier::builder(root_store)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build client cert verifier: {}", e))?;

    // Create the custom client cert verifier (CA chain + CN/SAN)
    let verifier = AegisClientCertVerifier {
        standard,
        expected_identity: tls_config.expected_identity.clone(),
    };

    // Build server config with mTLS
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| anyhow::anyhow!("TLS protocol config error: {}", e))?
            .with_client_cert_verifier(Arc::new(verifier))
            .with_single_cert(server_cert_chain, server_key_der)
            .map_err(|e| anyhow::anyhow!("TLS cert/key error: {}", e))?;

    // Advertise HTTP/2 via ALPN: tonic negotiates h2 itself only when it
    // terminates TLS; here the acceptor is external, so ALPN must be set on
    // the rustls config (REQ-713).
    config.alpn_protocols = vec![b"h2".to_vec()];

    Ok(config)
}
