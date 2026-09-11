//! gRPC Boundary Failure Tests — AD-007 hardening
//!
//! These tests exercise the gRPC protocol boundary error mapping for scenarios
//! already covered at the unit level but not yet verified end-to-end through
//! the gRPC layer.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair as RcgenKeyPair,
};
use ring::signature::KeyPair as RingKeyPair;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tonic::transport::{Certificate as TonicCertificate, Channel, ClientTlsConfig, Identity};
use tonic::Request;

use aegis::config::runtime::{RuntimeConfig, ServerConfig, TlsConfig, ReceiptsConfig, ExecutionConfig};
use aegis::grpc::server::start_server_internal;
use aegis::proto::aegis::v1::{
    aegis_runtime_client::AegisRuntimeClient,
    ExecuteRequest, VerifyChainRequest,
};
use aegis::receipts::ExecutionReceipt;
use aegis::sandbox::load_receipt_keypair;

/// Test certificate and key material for mTLS testing
struct TestCerts {
    ca_cert: Certificate,
    ca_key: RcgenKeyPair,
    server_cert: Certificate,
    server_key: RcgenKeyPair,
    client_cert: Certificate,
    client_key: RcgenKeyPair,
    temp_dir: TempDir,
}

impl TestCerts {
    /// Generate a complete mTLS certificate chain for testing:
    /// CA -> Server cert (CN=aegis-runtime) -> Client cert (CN=agent-gateway)
    fn generate() -> Result<Self> {
        let temp_dir = TempDir::new()?;

        // CA
        let mut ca_params = CertificateParams::new(vec!["aegis-test-ca".into()])?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = DistinguishedName::new();
        ca_params.distinguished_name.push(DnType::CommonName, "aegis-test-ca");
        let ca_key = RcgenKeyPair::generate()?;
        let ca_cert = ca_params.self_signed(&ca_key)?;

        // Server certificate (signed by CA)
        let mut server_params = CertificateParams::new(vec!["aegis-runtime".into()])?;
        server_params.distinguished_name = DistinguishedName::new();
        server_params.distinguished_name.push(DnType::CommonName, "aegis-runtime");
        let server_key = RcgenKeyPair::generate()?;
        // signed_by(public_key, issuer_cert, issuer_key)
        let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key)?;

        // Client certificate (signed by CA) - CN=agent-gateway matches expected_identity
        let mut client_params = CertificateParams::new(vec!["agent-gateway".into()])?;
        client_params.distinguished_name = DistinguishedName::new();
        client_params.distinguished_name.push(DnType::CommonName, "agent-gateway");
        let client_key = RcgenKeyPair::generate()?;
        let client_cert = client_params.signed_by(&client_key, &ca_cert, &ca_key)?;

        // Write all certs/keys to temp dir
        std::fs::write(temp_dir.path().join("ca.crt"), ca_cert.pem())?;
        std::fs::write(temp_dir.path().join("ca.key"), ca_key.serialize_pem())?;
        std::fs::write(temp_dir.path().join("server.crt"), server_cert.pem())?;
        std::fs::write(temp_dir.path().join("server.key"), server_key.serialize_pem())?;
        std::fs::write(temp_dir.path().join("client.crt"), client_cert.pem())?;
        std::fs::write(temp_dir.path().join("client.key"), client_key.serialize_pem())?;

        // Generate Ed25519 signing key for receipts (base64 PKCS8 in TOML)
        let rng = ring::rand::SystemRandom::new();
        let signing_key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|e| anyhow::anyhow!("failed to generate signing key: {}", e))?;
        let private_key_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key_pair.as_ref());
        let key_toml = format!(r#"
[signing_key]
private_key = "{}"
"#, private_key_b64);
        std::fs::write(temp_dir.path().join("signing_key.toml"), key_toml)?;
        // Set 0600 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp_dir.path().join("signing_key.toml"), std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(Self {
            ca_cert,
            ca_key,
            server_cert,
            server_key,
            client_cert,
            client_key,
            temp_dir,
        })
    }

    fn ca_path(&self) -> PathBuf { self.temp_dir.path().join("ca.crt") }
    fn server_cert_path(&self) -> PathBuf { self.temp_dir.path().join("server.crt") }
    fn server_key_path(&self) -> PathBuf { self.temp_dir.path().join("server.key") }
    fn client_cert_path(&self) -> PathBuf { self.temp_dir.path().join("client.crt") }
    fn client_key_path(&self) -> PathBuf { self.temp_dir.path().join("client.key") }
    fn signing_key_path(&self) -> PathBuf { self.temp_dir.path().join("signing_key.toml") }
}

/// Test server handle with address and shutdown
struct TestServer {
    addr: std::net::SocketAddr,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    _handle: JoinHandle<anyhow::Result<std::net::SocketAddr>>,
}

impl TestServer {
    /// Start a test gRPC server on a fixed port with mTLS
    async fn start(config: RuntimeConfig) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let port = config.server.port;

        // Construct proper SocketAddr
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

        // Wrap shutdown_rx in a future that resolves to ()
        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };

        let handle = tokio::spawn(async move {
            start_server_internal(&config, addr, shutdown_fut).await
        });

        // Wait a bit for server to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(Self {
            addr,
            _shutdown: shutdown_tx,
            _handle: handle,
        })
    }

    fn addr(&self) -> SocketAddr { self.addr }
}

/// Create a test gRPC client with mTLS configured
async fn create_client(certs: &TestCerts, addr: std::net::SocketAddr) -> Result<AegisRuntimeClient<Channel>> {
    let ca_pem = std::fs::read(certs.ca_path())?;
    let client_cert_pem = std::fs::read(certs.client_cert_path())?;
    let client_key_pem = std::fs::read(certs.client_key_path())?;

    let ca = TonicCertificate::from_pem(ca_pem);
    let identity = Identity::from_pem(client_cert_pem, client_key_pem);

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .identity(identity)
        .domain_name("aegis-runtime");

    let channel = Channel::from_shared(format!("https://{}", addr))?
        .tls_config(tls)?
        .connect()
        .await?;

    Ok(AegisRuntimeClient::new(channel))
}

/// Create a RuntimeConfig for testing with mTLS enabled
fn create_test_config(certs: &TestCerts, port: u16) -> RuntimeConfig {
    RuntimeConfig {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port,
            tls: Some(TlsConfig {
                cert_path: certs.server_cert_path(),
                key_path: certs.server_key_path(),
                ca_cert_path: certs.ca_path(),
                expected_identity: "agent-gateway".into(),
            }),
        },
        receipts: ReceiptsConfig {
            key_path: certs.signing_key_path(),
        },
        execution: ExecutionConfig {
            max_concurrent: 4,
        },
    }
}

/// Helper to create a filesystem.read capability config for Execute requests
fn filesystem_read_config(allowed_root: &str, max_read_bytes: u64) -> Vec<u8> {
    let toml = format!(r#"
capabilities = [
  {{ name = "filesystem.read", allowed_root = "{}", max_read_bytes = {} }}
]
"#, allowed_root, max_read_bytes);
    toml.into_bytes()
}

/// Helper to create an ExecuteRequest
fn execute_request(capability_name: &str, config_bytes: Vec<u8>) -> ExecuteRequest {
    ExecuteRequest {
        capability_name: capability_name.into(),
        config: config_bytes,
    }
}

/// S-701: Execute violation trap via gRPC (path traversal, size exceed)
/// 
/// Verifies that sandbox traps (path traversal, size exceed) correctly map
/// to gRPC FAILED_PRECONDITION status codes with appropriate error messages.
/// 
/// NOTE: Currently ignored because the Execute RPC handler doesn't actually
/// execute capabilities in the sandbox via wasmtime - it's a stub that returns
/// dummy success. This test will be enabled when the full Execute implementation
/// is wired up (wasmtime module loading + capability execution).
#[tokio::test]
#[ignore = "Execute RPC not yet wired to wasmtime capability execution"]
async fn execute_rpc_violation_trap_via_grpc() -> Result<()> {
    // Setup test certs and keys
    let certs = TestCerts::generate()?;

    // Start server on a fixed port for this test
    let config = create_test_config(&certs, 50051);
    let server = TestServer::start(config).await?;
    let addr = server.addr();

    // Create client with mTLS
    let mut client = create_client(&certs, addr).await?;

    // Test 1: Path traversal attempt (../../etc/passwd)
    let traversal_config = filesystem_read_config("/safe/root", 1024);
    let req = execute_request("filesystem.read", traversal_config);

    let resp = client.execute(Request::new(req)).await?;
    let _status = resp.metadata().get("grpc-status").and_then(|v| v.to_str().ok());

    // Currently returns success because Execute is a stub
    // When implemented, should fail with FAILED_PRECONDITION (traversal trap)
    // assert_eq!(resp.get_ref().success, false);

    // Shutdown server
    let _ = server._shutdown.send(());

    Ok(())
}

/// S-702: Execute signing failure via gRPC
/// 
/// Verifies that receipt signing failures correctly map to gRPC INTERNAL.
/// 
/// NOTE: Currently ignored because the Execute RPC handler doesn't actually
/// execute capabilities in the sandbox via wasmtime - it's a stub that returns
/// dummy success. This test will be enabled when the full Execute implementation
/// is wired up (wasmtime module loading + capability execution).
#[tokio::test]
#[ignore = "Execute RPC not yet wired to wasmtime capability execution"]
async fn execute_rpc_signing_failure_via_grpc() -> Result<()> {
    Ok(())
}

/// S-704: VerifyChain tampered receipt via gRPC
/// 
/// Verifies that tampered receipts in VerifyChain correctly map to
/// gRPC error responses (valid=false with error message).
#[tokio::test]
async fn verify_chain_tampered_receipt_via_grpc() -> Result<()> {
    let certs = TestCerts::generate()?;
    let config = create_test_config(&certs, 50052);
    let server = TestServer::start(config).await?;
    let addr = server.addr();

    let mut client = create_client(&certs, addr).await?;

    // Create a valid receipt first, then tamper with it
    let key_pair = load_receipt_keypair(&certs.signing_key_path())?;
    let mut receipt = ExecutionReceipt::new(
        "filesystem.read",
        "execute",
        "success",
        "/test.txt",
        100,
        [0u8; 32],
        &key_pair,
    )?;

    // Tamper with the receipt (modify capability_name)
    receipt.capability_name = "tampered".into();

    // Serialize tampered receipt
    let receipt_bytes = serde_json::to_vec(&receipt)?;

    let req = VerifyChainRequest {
        receipts: vec![receipt_bytes],
        public_key: key_pair.public_key().as_ref().to_vec(),
    };

    let resp = client.verify_chain(Request::new(req)).await?;
    assert_eq!(resp.get_ref().valid, false);
    assert!(!resp.get_ref().error_message.is_empty());

    let _ = server._shutdown.send(());

    Ok(())
}

/// S-721: Execute signing failure with corrupt key via gRPC
/// 
/// Verifies that a corrupt signing key (loads but fails to sign)
/// correctly maps to gRPC INTERNAL.
/// 
/// NOTE: Currently ignored because the Execute RPC handler doesn't actually
/// execute capabilities in the sandbox via wasmtime - it's a stub that returns
/// dummy success. This test will be enabled when the full Execute implementation
/// is wired up (wasmtime module loading + capability execution).
#[tokio::test]
#[ignore = "Execute RPC not yet wired to wasmtime capability execution"]
async fn execute_rpc_signing_failure_corrupt_key_via_grpc() -> Result<()> {
    Ok(())
}