//! Two-Phase Receipts E2E Tests — Phase 9
//!
//! These tests exercise the two-phase execution flow (ExecutePrepare, ExecuteCommit, ExecuteAbort)
//! end-to-end through the gRPC boundary, validating receipt chain integrity and AD-016 mitigation.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair as RcgenKeyPair,
};

use tempfile::TempDir;
use tokio::task::JoinHandle;
use tonic::transport::{Certificate as TonicCertificate, Channel, ClientTlsConfig, Identity};
use tonic::Request;

use aegis::config::runtime::{
    ExecutionConfig, ReceiptsConfig, RuntimeConfig, ServerConfig, TlsConfig,
};
use aegis::grpc::server::{start_server_internal, start_server_internal_with_emitter};
use aegis::proto::aegis::v1::{
    aegis_runtime_client::AegisRuntimeClient, ExecuteAbortRequest, ExecuteCommitRequest,
    ExecutePrepareRequest, GetReceiptChainRequest, VerifyChainRequest,
};
use aegis::receipts::{ExecutionReceipt, ReceiptEmitter};
use aegis::sandbox::load_receipt_keypair;

/// Enable test mode for the gRPC handler (disables epoch interruption)
fn enable_test_mode() {
    env::set_var("AEGIS_TEST_MODE", "1");
}

/// WASM test module generator using wat::parse_str
mod wasm_test_modules {
    /// Safe filesystem.read module - reads a valid file within allowed_root
    /// Returns (ptr, len) of the content read by fs_read
    /// Only imports fs_read since config only grants filesystem.read
    pub fn safe_read_module() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
              (memory 1)
              (export "memory" (memory 0))
              (data (i32.const 0) "safe_file.txt\00")
              (data (i32.const 16) "output_buffer\00")
              (func $execute (export "execute") (result i32 i32)
                (local $bytes_read i32)
                ;; Call fs_read to read file into output_buffer
                i32.const 0    ;; path_ptr
                i32.const 13   ;; path_len ("safe_file.txt")
                i32.const 16   ;; out_ptr
                i32.const 100  ;; out_len
                call $fs_read  ;; stack = [bytes_read]
                local.set $bytes_read
                ;; Return (ptr, len) = (out_ptr, bytes_read)
                i32.const 16   ;; ptr = out_ptr
                local.get $bytes_read
              )
            )
            "#,
        )
        .expect("valid WAT")
    }
}

/// Test certificate and key material for mTLS testing
struct TestCerts {
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
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "aegis-test-ca");
        let ca_key = RcgenKeyPair::generate()?;
        let ca_cert = ca_params.self_signed(&ca_key)?;

        // Server certificate (signed by CA)
        let mut server_params = CertificateParams::new(vec!["aegis-runtime".into()])?;
        server_params.distinguished_name = DistinguishedName::new();
        server_params
            .distinguished_name
            .push(DnType::CommonName, "aegis-runtime");
        let server_key = RcgenKeyPair::generate()?;
        let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key)?;

        // Client certificate (signed by CA) - CN=agent-gateway matches expected_identity
        let mut client_params = CertificateParams::new(vec!["agent-gateway".into()])?;
        client_params.distinguished_name = DistinguishedName::new();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "agent-gateway");
        let client_key = RcgenKeyPair::generate()?;
        let client_cert = client_params.signed_by(&client_key, &ca_cert, &ca_key)?;

        // Write all certs/keys to temp dir
        std::fs::write(temp_dir.path().join("ca.crt"), ca_cert.pem())?;
        std::fs::write(temp_dir.path().join("ca.key"), ca_key.serialize_pem())?;
        std::fs::write(temp_dir.path().join("server.crt"), server_cert.pem())?;
        std::fs::write(
            temp_dir.path().join("server.key"),
            server_key.serialize_pem(),
        )?;
        std::fs::write(temp_dir.path().join("client.crt"), client_cert.pem())?;
        std::fs::write(
            temp_dir.path().join("client.key"),
            client_key.serialize_pem(),
        )?;

        // Generate Ed25519 signing key for receipts (base64 PKCS8 in TOML)
        let rng = ring::rand::SystemRandom::new();
        let signing_key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|e| anyhow::anyhow!("failed to generate signing key: {}", e))?;
        let private_key_b64 =
            base64::engine::general_purpose::STANDARD.encode(signing_key_pair.as_ref());
        let key_toml = format!(
            r#"
[signing_key]
private_key = "{}"
"#,
            private_key_b64
        );
        std::fs::write(temp_dir.path().join("signing_key.toml"), key_toml)?;
        // Set 0600 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp_dir.path().join("signing_key.toml"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }

        Ok(Self { temp_dir })
    }

    fn ca_path(&self) -> PathBuf {
        self.temp_dir.path().join("ca.crt")
    }
    fn server_cert_path(&self) -> PathBuf {
        self.temp_dir.path().join("server.crt")
    }
    fn server_key_path(&self) -> PathBuf {
        self.temp_dir.path().join("server.key")
    }
    fn client_cert_path(&self) -> PathBuf {
        self.temp_dir.path().join("client.crt")
    }
    fn client_key_path(&self) -> PathBuf {
        self.temp_dir.path().join("client.key")
    }
    fn signing_key_path(&self) -> PathBuf {
        self.temp_dir.path().join("signing_key.toml")
    }
}

/// Test server handle with address and shutdown.
struct TestServer {
    addr: std::net::SocketAddr,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    _handle: JoinHandle<anyhow::Result<std::net::SocketAddr>>,
}

impl TestServer {
    /// Start a test gRPC server on a fixed port with mTLS
    /// Creates the ReceiptEmitter internally (production-like path).
    async fn start(config: RuntimeConfig) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let port = config.server.port;

        // Construct proper SocketAddr
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

        // Wrap shutdown_rx in a future that resolves to ()
        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };

        let handle =
            tokio::spawn(async move { start_server_internal(&config, addr, shutdown_fut).await });

        // Wait a bit for server to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(Self {
            addr,
            _shutdown: shutdown_tx,
            _handle: handle,
        })
    }

    /// Start a test gRPC server with a pre-created ReceiptEmitter.
    /// Returns the TestServer plus the emitter for test-only operations.
    async fn start_with_emitter(
        config: RuntimeConfig,
    ) -> Result<(Self, Arc<StdMutex<ReceiptEmitter>>)> {
        let key_path = config.receipts.key_path.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let port = config.server.port;

        // Construct proper SocketAddr
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

        // Create the emitter upfront
        let key_pair = load_receipt_keypair(&key_path)
            .map_err(|e| anyhow::anyhow!("failed to load receipt key: {}", e))?;
        let receipt_emitter = Arc::new(StdMutex::new(ReceiptEmitter::new(key_pair)));
        let emitter_for_server = Arc::clone(&receipt_emitter);

        // Wrap shutdown_rx in a future that resolves to ()
        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };

        let handle = tokio::spawn(async move {
            start_server_internal_with_emitter(&config, addr, shutdown_fut, emitter_for_server)
                .await
        });

        // Wait a bit for server to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok((
            Self {
                addr,
                _shutdown: shutdown_tx,
                _handle: handle,
            },
            receipt_emitter,
        ))
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Create a test gRPC client with mTLS configured
async fn create_client(
    certs: &TestCerts,
    addr: std::net::SocketAddr,
) -> Result<AegisRuntimeClient<Channel>> {
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
            fuel_budget: None,
        },
    }
}

/// Helper to create a two-phase capable config (includes two_phase_receipts capability)
fn two_phase_config(allowed_root: &str, max_read_bytes: u64) -> Vec<u8> {
    let toml = format!(
        r#"
capabilities = [
  {{ name = "filesystem.read", allowed_root = "{}", max_read_bytes = {} }},
  {{ name = "two_phase_receipts" }}
]
"#,
        allowed_root, max_read_bytes
    );
    toml.into_bytes()
}

/// Helper to create an ExecutePrepareRequest with a WASM module
fn execute_prepare_request(
    capability_name: &str,
    config_bytes: Vec<u8>,
    wasm_module: Vec<u8>,
) -> ExecutePrepareRequest {
    ExecutePrepareRequest {
        capability_name: capability_name.into(),
        config: config_bytes,
        wasm_module,
    }
}

/// Helper to create an ExecuteCommitRequest
fn execute_commit_request(prepare_hash: &str, result: Vec<u8>) -> ExecuteCommitRequest {
    ExecuteCommitRequest {
        prepare_hash: prepare_hash.into(),
        result,
    }
}

/// Helper to create an ExecuteAbortRequest
fn execute_abort_request(prepare_hash: &str) -> ExecuteAbortRequest {
    ExecuteAbortRequest {
        prepare_hash: prepare_hash.into(),
    }
}

/// Parse JSON receipt bytes into ExecutionReceipt
fn parse_receipt(bytes: &[u8]) -> ExecutionReceipt {
    serde_json::from_slice(bytes).expect("valid JSON receipt")
}

/// S-755: ExecutePrepare -> ExecuteCommit happy path
///
/// Verifies the full two-phase flow: Prepare creates sandbox and signs prepare receipt,
/// Commit executes WASM in the prepared sandbox and signs commit receipt with actual results.
#[tokio::test]
async fn execute_prepare_then_commit_happy_path() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    // Create a temp directory for allowed_root with a test file
    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "two-phase content")?;

    let config = create_test_config(&certs, 50051);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();

    assert!(prepare_resp.success, "ExecutePrepare should succeed");
    assert!(
        !prepare_resp.receipt.is_empty(),
        "prepare receipt should be present"
    );
    assert!(
        !prepare_resp.prepare_hash.is_empty(),
        "prepare_hash should be present"
    );

    // Parse and verify prepare receipt
    let prepare_receipt = parse_receipt(&prepare_resp.receipt);
    assert_eq!(prepare_receipt.phase, "prepare");
    assert_eq!(prepare_receipt.result, "pending");
    assert_eq!(prepare_receipt.path, "");
    assert_eq!(prepare_receipt.size, 0);
    assert_eq!(prepare_receipt.fuel_consumed, 0);
    assert_ne!(prepare_receipt.pending_hash, [0u8; 32]);
    assert_eq!(
        hex::encode(prepare_receipt.pending_hash),
        prepare_resp.prepare_hash
    );

    // ExecuteCommit with the prepare_hash
    let commit_req = execute_commit_request(&prepare_resp.prepare_hash, b"success".to_vec());
    let commit_resp = client
        .execute_commit(Request::new(commit_req))
        .await?
        .into_inner();

    assert!(
        commit_resp.success,
        "ExecuteCommit should succeed: {:?}",
        commit_resp.error_message
    );
    assert!(
        !commit_resp.receipt.is_empty(),
        "commit receipt should be present"
    );

    // Parse and verify commit receipt
    let commit_receipt = parse_receipt(&commit_resp.receipt);
    assert_eq!(commit_receipt.phase, "commit");
    // The commit receipt's result field contains the actual WASM execution result (file content)
    assert_eq!(commit_receipt.result, "two-phase content");
    assert_eq!(
        commit_receipt.path,
        test_root.path().to_string_lossy().to_string()
    );
    assert_eq!(commit_receipt.size, "two-phase content".len() as u64);
    assert!(
        commit_receipt.fuel_consumed > 0,
        "fuel_consumed should be > 0"
    );
    assert_eq!(commit_receipt.pending_hash, prepare_receipt.pending_hash);
    assert_eq!(commit_receipt.prev_hash, prepare_receipt.pending_hash);

    // Verify the chain via VerifyChain
    let pub_key = {
        let emitter = _emitter.lock().unwrap();
        emitter.public_key()
    };
    let chain = [prepare_receipt, commit_receipt];
    let verify_req = VerifyChainRequest {
        receipts: chain
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap())
            .collect(),
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();
    assert!(
        verify_resp.valid,
        "prepare->commit chain should verify: {:?}",
        verify_resp.error_message
    );

    Ok(())
}

/// S-756: ExecutePrepare -> ExecuteAbort
///
/// Verifies that a prepared execution can be aborted, producing an abort receipt
/// that correctly links to the prepare receipt in the chain.
#[tokio::test]
async fn execute_prepare_then_abort() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "abort test content")?;

    let config = create_test_config(&certs, 50052);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();

    assert!(prepare_resp.success);
    let prepare_receipt = parse_receipt(&prepare_resp.receipt);
    assert_eq!(prepare_receipt.phase, "prepare");

    // ExecuteAbort
    let abort_req = execute_abort_request(&prepare_resp.prepare_hash);
    let abort_resp = client
        .execute_abort(Request::new(abort_req))
        .await?
        .into_inner();

    assert!(abort_resp.success, "ExecuteAbort should succeed");
    assert!(
        !abort_resp.receipt.is_empty(),
        "abort receipt should be present"
    );

    // Parse and verify abort receipt
    let abort_receipt = parse_receipt(&abort_resp.receipt);
    assert_eq!(abort_receipt.phase, "abort");
    assert_eq!(abort_receipt.result, "aborted");
    assert_eq!(abort_receipt.path, "");
    assert_eq!(abort_receipt.size, 0);
    assert_eq!(abort_receipt.fuel_consumed, 0);
    assert_eq!(abort_receipt.pending_hash, prepare_receipt.pending_hash);
    assert_eq!(abort_receipt.prev_hash, prepare_receipt.pending_hash);

    // Verify the chain via VerifyChain
    let pub_key = {
        let emitter = _emitter.lock().unwrap();
        emitter.public_key()
    };
    let chain = [prepare_receipt, abort_receipt];
    let verify_req = VerifyChainRequest {
        receipts: chain
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap())
            .collect(),
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();
    assert!(
        verify_resp.valid,
        "prepare->abort chain should verify: {:?}",
        verify_resp.error_message
    );

    Ok(())
}

/// S-757: ExecutePrepare without two_phase_receipts capability should fail
///
/// Verifies that ExecutePrepare requires the `two_phase_receipts` capability
/// to be granted in the config, otherwise returns success=false with error.
#[tokio::test]
async fn execute_prepare_requires_two_phase_capability() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    std::fs::write(test_root.path().join("safe_file.txt"), "content")?;

    let config = create_test_config(&certs, 50053);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Config WITHOUT two_phase_receipts capability
    let no_two_phase_config = format!(
        r#"
capabilities = [
  {{ name = "filesystem.read", allowed_root = "{}", max_read_bytes = 1024 }}
]
"#,
        test_root.path().to_string_lossy()
    )
    .into_bytes();

    let prepare_req = execute_prepare_request(
        "filesystem.read",
        no_two_phase_config,
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();

    assert!(
        !prepare_resp.success,
        "ExecutePrepare should fail without two_phase_receipts"
    );
    assert!(prepare_resp.receipt.is_empty(), "no receipt on failure");
    assert!(
        prepare_resp.prepare_hash.is_empty(),
        "no prepare_hash on failure"
    );
    assert!(prepare_resp
        .error_message
        .contains("two_phase_receipts capability not granted"));

    Ok(())
}

/// AD-016: Legacy Execute with commit signing failure -> abort with error="commit_signing_failed"
///
/// Verifies the documented gap mitigation: when commit signing fails after WASM
/// execution (side effects committed), the legacy Execute handler emits an abort receipt with
/// `error="commit_signing_failed"` and returns success=false.
#[tokio::test]
async fn legacy_execute_commit_signing_failure_emits_abort() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "signing failure test")?;

    // Start server with pre-created emitter so we can force signing failure
    let config = create_test_config(&certs, 50054);
    let (server, emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Use legacy Execute (which internally does Prepare+Commit)
    use aegis::proto::aegis::v1::ExecuteRequest;
    let execute_req = ExecuteRequest {
        capability_name: "filesystem.read".into(),
        config: two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_module: wasm_test_modules::safe_read_module(),
    };

    // Force the NEXT commit signing operation to fail (this will be the commit)
    {
        let mut emitter = emitter.lock().unwrap();
        emitter.force_commit_signing_failure();
    }

    // Execute - should fail with signing failure and emit abort
    let execute_resp = client
        .execute(Request::new(execute_req))
        .await?
        .into_inner();

    // Should return success=false with abort receipt
    assert!(
        !execute_resp.success,
        "Legacy Execute should fail with signing failure"
    );
    assert!(
        !execute_resp.receipt.is_empty(),
        "abort receipt should be present on signing failure"
    );
    assert_eq!(execute_resp.error_message, "commit signing failed");

    // Parse abort receipt
    let abort_receipt = parse_receipt(&execute_resp.receipt);
    assert_eq!(abort_receipt.phase, "abort");
    assert_eq!(abort_receipt.result, "aborted");

    // Verify the chain (prepare -> legacy fs_read -> abort(commit_signing_failed) should be valid terminal)
    // Note: The legacy Execute doesn't expose prepare_hash, so we get the chain from GetReceiptChain
    let pub_key = {
        let emitter = emitter.lock().unwrap();
        emitter.public_key()
    };
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    let chain_receipts: Vec<ExecutionReceipt> = chain_resp
        .receipts
        .iter()
        .map(|b| parse_receipt(b))
        .collect();

    // Should have prepare + legacy fs_read + abort (host function emits legacy receipt during WASM execution)
    assert_eq!(
        chain_receipts.len(),
        3,
        "chain should have prepare + legacy fs_read + abort"
    );
    assert_eq!(chain_receipts[0].phase, "prepare");
    assert_eq!(chain_receipts[1].phase, ""); // legacy fs_read receipt from host function
    assert_eq!(chain_receipts[2].phase, "abort");
    assert_eq!(chain_receipts[2].result, "aborted");

    let verify_req = VerifyChainRequest {
        receipts: chain_receipts
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap())
            .collect(),
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();
    assert!(
        verify_resp.valid,
        "prepare->legacy fs_read->abort(commit_signing_failed) chain should verify: {:?}",
        verify_resp.error_message
    );

    // Verify GetReceiptChain returns the chain
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    assert_eq!(
        chain_resp.receipts.len(),
        3,
        "chain should have prepare + legacy fs_read + abort"
    );
    let chain_receipts: Vec<ExecutionReceipt> = chain_resp
        .receipts
        .iter()
        .map(|b| parse_receipt(b))
        .collect();
    assert_eq!(chain_receipts[0].phase, "prepare");
    assert_eq!(chain_receipts[1].phase, ""); // legacy fs_read receipt from host function
    assert_eq!(chain_receipts[2].phase, "abort");

    Ok(())
}

/// ExecutePrepare -> ExecuteCommit with legacy Execute fallback path
///
/// The legacy Execute RPC internally uses Prepare+Commit two-phase flow.
/// On commit signing failure, it emits abort with error="commit_signing_failed".
/// On success, it returns the commit receipt (phase="commit") directly.
#[tokio::test]
async fn legacy_execute_uses_two_phase_internally() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "legacy execute test")?;

    let config = create_test_config(&certs, 50055);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Use legacy Execute (which internally does Prepare+Commit)
    use aegis::proto::aegis::v1::ExecuteRequest;
    let execute_req = ExecuteRequest {
        capability_name: "filesystem.read".into(),
        config: two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_module: wasm_test_modules::safe_read_module(),
    };
    let execute_resp = client
        .execute(Request::new(execute_req))
        .await?
        .into_inner();

    assert!(
        execute_resp.success,
        "Legacy Execute should succeed: {:?}",
        execute_resp.error_message
    );
    assert!(!execute_resp.result.is_empty(), "result should be present");
    assert!(
        !execute_resp.receipt.is_empty(),
        "receipt should be present"
    );

    // The legacy Execute returns the commit receipt (phase="commit") directly on success.
    // The legacy receipt (phase="") is only created in the error path for backward compatibility.
    // The legacy Execute passes BLAKE3 hash of result as the commit receipt's result field.
    let commit_receipt = parse_receipt(&execute_resp.receipt);
    assert_eq!(commit_receipt.phase, "commit");
    // Result should be BLAKE3 hash of "legacy execute test" (32 bytes = 64 hex chars)
    assert_eq!(
        commit_receipt.result.len(),
        64,
        "result should be BLAKE3 hex (64 chars)"
    );

    // Internally, the chain should have prepare + legacy fs_read receipt + commit
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    let chain_receipts: Vec<ExecutionReceipt> = chain_resp
        .receipts
        .iter()
        .map(|b| parse_receipt(b))
        .collect();

    // Chain should have prepare + legacy fs_read + commit (host function emits legacy receipt)
    assert_eq!(
        chain_receipts.len(),
        3,
        "internal chain should have prepare + legacy fs_read + commit"
    );
    assert_eq!(chain_receipts[0].phase, "prepare");
    assert_eq!(chain_receipts[1].phase, ""); // legacy fs_read receipt from host function
    assert_eq!(chain_receipts[2].phase, "commit");

    Ok(())
}

/// ExecuteCommit - first call succeeds, second call fails (idempotency at emitter level)
///
/// The ReceiptEmitter::commit() is idempotent (returns existing commit if already committed),
/// but the gRPC handler checks for pending sandbox first and returns NOT_FOUND if already committed.
/// This test verifies the current handler behavior.
#[tokio::test]
async fn execute_commit_first_succeeds_second_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "idempotent test")?;

    let config = create_test_config(&certs, 50056);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();

    assert!(prepare_resp.success);

    // First ExecuteCommit - succeeds
    let commit_req1 = execute_commit_request(&prepare_resp.prepare_hash, b"success".to_vec());
    let commit_resp1 = client
        .execute_commit(Request::new(commit_req1))
        .await?
        .into_inner();
    assert!(commit_resp1.success);

    // Second ExecuteCommit with same prepare_hash - fails with NOT_FOUND
    // (handler checks for pending sandbox, which was removed after first commit)
    let commit_req2 = execute_commit_request(&prepare_resp.prepare_hash, b"success".to_vec());
    let result2 = client.execute_commit(Request::new(commit_req2)).await;

    assert!(
        result2.is_err(),
        "Second ExecuteCommit should fail with NOT_FOUND"
    );
    let status2 = result2.unwrap_err();
    assert_eq!(status2.code(), tonic::Code::NotFound);
    assert!(status2
        .message()
        .contains("prepare_hash not found or already committed/aborted"));

    Ok(())
}

/// ExecuteAbort idempotency
///
/// Verifies that calling ExecuteAbort twice with the same prepare_hash
/// returns the same abort receipt (idempotent).
#[tokio::test]
async fn execute_abort_idempotent() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "idempotent abort test")?;

    let config = create_test_config(&certs, 50057);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();

    assert!(prepare_resp.success);

    // First ExecuteAbort
    let abort_req1 = execute_abort_request(&prepare_resp.prepare_hash);
    let abort_resp1 = client
        .execute_abort(Request::new(abort_req1))
        .await?
        .into_inner();
    assert!(abort_resp1.success);

    // Second ExecuteAbort with same prepare_hash (idempotent)
    let abort_req2 = execute_abort_request(&prepare_resp.prepare_hash);
    let abort_resp2 = client
        .execute_abort(Request::new(abort_req2))
        .await?
        .into_inner();
    assert!(abort_resp2.success);

    // Both should return identical abort receipts
    assert_eq!(abort_resp1.receipt, abort_resp2.receipt);

    Ok(())
}

/// GetReceiptChain returns the full two-phase chain
///
/// Verifies that GetReceiptChain returns all receipts including two-phase
/// prepare/commit/abort entries in order.
#[tokio::test]
async fn get_receipt_chain_returns_two_phase_chain() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "chain test")?;

    let config = create_test_config(&certs, 50058);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // First execution: prepare -> commit
    let prepare_req1 = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp1 = client
        .execute_prepare(Request::new(prepare_req1))
        .await?
        .into_inner();
    assert!(prepare_resp1.success);

    let commit_req1 = execute_commit_request(&prepare_resp1.prepare_hash, b"success".to_vec());
    let commit_resp1 = client
        .execute_commit(Request::new(commit_req1))
        .await?
        .into_inner();
    assert!(commit_resp1.success);

    // Second execution: prepare -> abort
    let prepare_req2 = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp2 = client
        .execute_prepare(Request::new(prepare_req2))
        .await?
        .into_inner();
    assert!(prepare_resp2.success);

    let abort_req2 = execute_abort_request(&prepare_resp2.prepare_hash);
    let abort_resp2 = client
        .execute_abort(Request::new(abort_req2))
        .await?
        .into_inner();
    assert!(abort_resp2.success);

    // GetReceiptChain should return 5 receipts:
    // prepare1, legacy fs_read receipt, commit1, prepare2, abort2
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    eprintln!("DEBUG: chain has {} receipts", chain_resp.receipts.len());
    for (i, r) in chain_resp.receipts.iter().enumerate() {
        let receipt = parse_receipt(r);
        eprintln!(
            "DEBUG: receipt {}: phase={}, capability={}, prev_hash={:02x?}",
            i, receipt.phase, receipt.capability_name, receipt.prev_hash
        );
    }
    let chain_receipts: Vec<ExecutionReceipt> = chain_resp
        .receipts
        .iter()
        .map(|b| parse_receipt(b))
        .collect();

    assert_eq!(chain_receipts.len(), 5);
    assert_eq!(chain_receipts[0].phase, "prepare");
    assert_eq!(chain_receipts[1].phase, ""); // legacy fs_read receipt from host function
    assert_eq!(chain_receipts[2].phase, "commit");
    assert_eq!(chain_receipts[3].phase, "prepare");
    assert_eq!(chain_receipts[4].phase, "abort");

    // Verify chain integrity
    let pub_key = {
        let emitter = _emitter.lock().unwrap();
        emitter.public_key()
    };
    let verify_req = VerifyChainRequest {
        receipts: chain_receipts
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap())
            .collect(),
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();
    assert!(
        verify_resp.valid,
        "full chain should verify: {:?}",
        verify_resp.error_message
    );

    Ok(())
}

/// VerifyChain validates prepare->commit chain
///
/// Verifies that VerifyChain accepts a valid prepare->commit chain.
#[tokio::test]
async fn verify_chain_accepts_prepare_commit() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "verify chain test")?;

    let config = create_test_config(&certs, 50059);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare -> ExecuteCommit
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();
    assert!(prepare_resp.success);

    let commit_req = execute_commit_request(&prepare_resp.prepare_hash, b"success".to_vec());
    let commit_resp = client
        .execute_commit(Request::new(commit_req))
        .await?
        .into_inner();
    assert!(commit_resp.success);

    // Verify via VerifyChain
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    let pub_key = {
        let emitter = _emitter.lock().unwrap();
        emitter.public_key()
    };

    let verify_req = VerifyChainRequest {
        receipts: chain_resp.receipts,
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();

    assert!(
        verify_resp.valid,
        "prepare->commit chain should verify: {:?}",
        verify_resp.error_message
    );

    Ok(())
}

/// VerifyChain validates prepare->abort chain
///
/// Verifies that VerifyChain accepts a valid prepare->abort chain.
#[tokio::test]
async fn verify_chain_accepts_prepare_abort() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "verify abort test")?;

    let config = create_test_config(&certs, 50060);
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // ExecutePrepare -> ExecuteAbort
    let prepare_req = execute_prepare_request(
        "filesystem.read",
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let prepare_resp = client
        .execute_prepare(Request::new(prepare_req))
        .await?
        .into_inner();
    assert!(prepare_resp.success);

    let abort_req = execute_abort_request(&prepare_resp.prepare_hash);
    let abort_resp = client
        .execute_abort(Request::new(abort_req))
        .await?
        .into_inner();
    assert!(abort_resp.success);

    // Verify via VerifyChain
    let chain_req = GetReceiptChainRequest {};
    let chain_resp = client
        .get_receipt_chain(Request::new(chain_req))
        .await?
        .into_inner();
    let pub_key = {
        let emitter = _emitter.lock().unwrap();
        emitter.public_key()
    };

    let verify_req = VerifyChainRequest {
        receipts: chain_resp.receipts,
        public_key: pub_key,
    };
    let verify_resp = client
        .verify_chain(Request::new(verify_req))
        .await?
        .into_inner();

    assert!(
        verify_resp.valid,
        "prepare->abort chain should verify: {:?}",
        verify_resp.error_message
    );

    Ok(())
}

/// ExecuteCommit with non-existent prepare_hash should fail
///
/// Verifies that ExecuteCommit returns NOT_FOUND for unknown/already-consumed prepare_hash.
#[tokio::test]
async fn execute_commit_unknown_prepare_hash_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let config = create_test_config(&certs, 50061);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Try to commit with a random prepare_hash
    let fake_prepare_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let commit_req = execute_commit_request(fake_prepare_hash, b"success".to_vec());
    let result = client.execute_commit(Request::new(commit_req)).await;

    // Should return gRPC NOT_FOUND status
    assert!(result.is_err(), "ExecuteCommit should fail with gRPC error");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(status
        .message()
        .contains("prepare_hash not found or already committed/aborted"));

    Ok(())
}

/// ExecuteAbort with non-existent prepare_hash should fail
///
/// Verifies that ExecuteAbort returns INTERNAL for unknown/already-consumed prepare_hash
/// (the emitter's abort returns an error that is mapped to Internal by the handler).
#[tokio::test]
async fn execute_abort_unknown_prepare_hash_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let config = create_test_config(&certs, 50062);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Try to abort with a random prepare_hash
    let fake_prepare_hash = "1111111111111111111111111111111111111111111111111111111111111111";
    let abort_req = execute_abort_request(fake_prepare_hash);
    let result = client.execute_abort(Request::new(abort_req)).await;

    // Should return gRPC INTERNAL status (emitter error mapped to Internal)
    assert!(result.is_err(), "ExecuteAbort should fail with gRPC error");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Internal);
    assert!(status.message().contains("abort not found for handle"));

    Ok(())
}

/// ExecutePrepare with invalid prepare_hash hex should fail
///
/// Verifies that ExecuteCommit/Abort validate prepare_hash format (must be 32 bytes hex).
#[tokio::test]
async fn execute_commit_invalid_prepare_hash_format_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let config = create_test_config(&certs, 50063);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Invalid hex (not 32 bytes)
    let commit_req = execute_commit_request("not-valid-hex", b"success".to_vec());
    let result = client.execute_commit(Request::new(commit_req)).await;

    // Should return gRPC InvalidArgument status
    assert!(result.is_err(), "ExecuteCommit should fail with gRPC error");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("invalid prepare_hash"));

    // Wrong length hex (16 bytes instead of 32)
    let commit_req2 =
        execute_commit_request("00000000000000000000000000000000", b"success".to_vec());
    let result2 = client.execute_commit(Request::new(commit_req2)).await;

    assert!(
        result2.is_err(),
        "ExecuteCommit should fail with gRPC error"
    );
    let status2 = result2.unwrap_err();
    assert_eq!(status2.code(), tonic::Code::InvalidArgument);
    assert!(status2.message().contains("prepare_hash must be 32 bytes"));

    Ok(())
}

/// ExecutePrepare with empty WASM module should fail
///
/// Verifies that ExecutePrepare requires a WASM module.
#[tokio::test]
async fn execute_prepare_empty_wasm_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;

    let config = create_test_config(&certs, 50064);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    let prepare_req = ExecutePrepareRequest {
        capability_name: "filesystem.read".into(),
        config: two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_module: Vec::new(), // empty
    };
    let result = client.execute_prepare(Request::new(prepare_req)).await;

    // Should return gRPC InvalidArgument status
    assert!(
        result.is_err(),
        "ExecutePrepare should fail with gRPC error"
    );
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("wasm_module is required"));

    Ok(())
}

/// ExecutePrepare with invalid capability name should fail
///
/// Verifies that ExecutePrepare rejects capabilities not granted in config.
#[tokio::test]
async fn execute_prepare_invalid_capability_fails() -> Result<()> {
    enable_test_mode();

    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;

    let config = create_test_config(&certs, 50065);
    let server = TestServer::start(config).await?;

    let mut client = create_client(&certs, server.addr()).await?;

    // Request a capability not in config
    let prepare_req = execute_prepare_request(
        "filesystem.write", // not granted in two_phase_config
        two_phase_config(&test_root.path().to_string_lossy(), 1024),
        wasm_test_modules::safe_read_module(),
    );
    let result = client.execute_prepare(Request::new(prepare_req)).await;

    // Should return gRPC FailedPrecondition status
    assert!(
        result.is_err(),
        "ExecutePrepare should fail with gRPC error"
    );
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(status.message().contains("not granted in config"));

    Ok(())
}
