//! Phase 8 Fuel Metering E2E Tests
//!
//! These tests verify fuel metering functionality end-to-end through the gRPC layer.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Global mutex to serialize fuel tests — they all set/remove AEGIS_TEST_MODE
/// which is process-global state. Without serialization, parallel tests clobber
/// each other's env var and fail with "WASM execution trapped" instead of
/// the expected classification. Async mutex so the guard is never held across
/// an await point (clippy::await_holding_lock).
static FUEL_TEST_MUTEX: OnceLock<TokioMutex<()>> = OnceLock::new();

async fn fuel_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    FUEL_TEST_MUTEX
        .get_or_init(|| TokioMutex::new(()))
        .lock()
        .await
}

use anyhow::Result;
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair as RcgenKeyPair,
};
use tempfile::TempDir;
use tokio::{sync::Mutex as TokioMutex, task::JoinHandle};
use tonic::transport::{Certificate as TonicCertificate, Channel, ClientTlsConfig, Identity};
use tonic::Request;

use aegis::config::runtime::{
    ExecutionConfig, ReceiptsConfig, RuntimeConfig, ServerConfig, TlsConfig,
};
use aegis::grpc::server::start_server_internal;
use aegis::proto::aegis::v1::{aegis_runtime_client::AegisRuntimeClient, ExecuteRequest};
use aegis::receipts::ExecutionReceipt;

/// Enable test mode for the gRPC handler (disables epoch interruption)
fn enable_test_mode() {
    env::set_var("AEGIS_TEST_MODE", "1");
}

/// Disable test mode
fn disable_test_mode() {
    env::remove_var("AEGIS_TEST_MODE");
}

/// Test certificate and key material for mTLS testing
struct TestCerts {
    temp_dir: TempDir,
}

impl TestCerts {
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
        let mut server_params = CertificateParams::new(vec!["localhost".into()])?;
        server_params.distinguished_name = DistinguishedName::new();
        server_params
            .distinguished_name
            .push(DnType::CommonName, "aegis-runtime");
        server_params.subject_alt_names =
            vec![rcgen::SanType::DnsName("localhost".try_into().unwrap())];
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

/// Allocate a port free at the moment of the call (bind 127.0.0.1:0, read
/// the assigned port, then release it for the server to bind).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("read local addr")
        .port()
}

/// Wait until the server at `addr` accepts TCP connections (bounded).
async fn wait_for_server(addr: std::net::SocketAddr) {
    for _ in 0..250 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("test gRPC server at {addr} did not become ready");
}

/// Test server handle
struct TestServer {
    addr: SocketAddr,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    _handle: JoinHandle<anyhow::Result<SocketAddr>>,
}

impl TestServer {
    async fn start(config: RuntimeConfig) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let port = config.server.port;
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };
        let handle =
            tokio::spawn(async move { start_server_internal(&config, addr, shutdown_fut).await });
        wait_for_server(addr).await;
        Ok(Self {
            addr,
            _shutdown: shutdown_tx,
            _handle: handle,
        })
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

fn create_test_config(certs: &TestCerts, port: u16, fuel_budget: Option<u64>) -> RuntimeConfig {
    RuntimeConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port,
            tls: Some(TlsConfig {
                cert_path: certs.server_cert_path(),
                key_path: certs.server_key_path(),
                ca_cert_path: certs.ca_path(),
                expected_identity: "agent-gateway".to_string(),
            }),
        },
        receipts: ReceiptsConfig {
            key_path: certs.signing_key_path(),
        },
        execution: ExecutionConfig {
            max_concurrent: 4,
            fuel_budget,
        },
    }
}

async fn create_client(certs: &TestCerts, addr: SocketAddr) -> Result<AegisRuntimeClient<Channel>> {
    let ca_pem = std::fs::read(certs.ca_path())?;
    let client_cert_pem = std::fs::read(certs.client_cert_path())?;
    let client_key_pem = std::fs::read(certs.client_key_path())?;

    let ca_cert = TonicCertificate::from_pem(ca_pem);
    let client_identity = Identity::from_pem(client_cert_pem, client_key_pem);
    let tls = ClientTlsConfig::new()
        .ca_certificate(ca_cert)
        .identity(client_identity)
        .domain_name("localhost");

    let channel = Channel::builder(format!("https://{}", addr).parse()?)
        .tls_config(tls)?
        .connect()
        .await?;
    Ok(AegisRuntimeClient::new(channel))
}

fn filesystem_read_config(allowed_root: &str, max_read_bytes: u64) -> Vec<u8> {
    let toml_str = format!(
        r#"
[[capabilities]]
name = "filesystem.read"
allowed_root = "{}"
max_read_bytes = {}
"#,
        allowed_root, max_read_bytes
    );
    toml_str.into_bytes()
}

fn execute_request(capability: &str, config: Vec<u8>, wasm: Vec<u8>) -> ExecuteRequest {
    ExecuteRequest {
        capability_name: capability.to_string(),
        config,
        wasm_module: wasm,
    }
}

/// WASM test module generator
mod wasm_test_modules {
    /// Safe filesystem.read module
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
                i32.const 0
                i32.const 13
                i32.const 16
                i32.const 100
                call $fs_read
                local.set $bytes_read
                i32.const 16
                local.get $bytes_read
              )
            )
        "#,
        )
        .expect("valid WAT")
    }

    /// Module that does a compute loop (consumes fuel) then reads a file
    pub fn compute_then_read_module() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
              (memory 1)
              (export "memory" (memory 0))
              (data (i32.const 0) "safe_file.txt\00")
              (data (i32.const 16) "output_buffer\00")
              (func $execute (export "execute") (result i32 i32)
                (local $i i32)
                (local $bytes_read i32)
                i32.const 0
                local.set $i
                block $loop_end
                  loop $loop
                    local.get $i
                    i32.const 1000
                    i32.lt_s
                    br_if $loop_end
                    local.get $i
                    i32.const 7
                    i32.mul
                    local.get $i
                    i32.add
                    drop
                    local.get $i
                    i32.const 1
                    i32.add
                    local.set $i
                    br $loop
                  end
                end
                i32.const 0
                i32.const 13
                i32.const 16
                i32.const 100
                call $fs_read
                local.set $bytes_read
                i32.const 16
                local.get $bytes_read
              )
            )
        "#,
        )
        .expect("valid WAT")
    }

    /// Hostile infinite loop module - burns all fuel
    pub fn hostile_loop_module() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (memory 1)
              (export "memory" (memory 0))
              (func $execute (export "execute") (result i32 i32)
                loop
                  br 0     ;; Infinite loop - will exhaust fuel
                end
                ;; Never reached
                i32.const 0
                i32.const 0
              )
            )
        "#,
        )
        .expect("valid WAT")
    }
}

async fn run_execute_test(
    certs: &TestCerts,
    port: u16,
    fuel_budget: Option<u64>,
    wasm: Vec<u8>,
    config: Vec<u8>,
) -> Result<tonic::Response<aegis::proto::aegis::v1::ExecuteResponse>> {
    let _lock = fuel_test_lock().await;
    enable_test_mode();
    let config_struct = create_test_config(certs, port, fuel_budget);
    let server = TestServer::start(config_struct).await?;
    let addr = server.addr();
    let mut client = create_client(certs, addr).await?;

    let req = execute_request("filesystem.read", config, wasm);
    let resp = client.execute(Request::new(req)).await?;

    let _ = server._shutdown.send(());
    disable_test_mode();
    Ok(resp)
}

#[tokio::test]
async fn s801_execute_success_reports_fuel() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "hello world")?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::compute_then_read_module();

    let resp = run_execute_test(&certs, free_port(), None, wasm, config).await?;

    assert!(resp.get_ref().success, "Execute should succeed");
    assert!(
        !resp.get_ref().receipt.is_empty(),
        "Receipt should be present"
    );

    let receipt: ExecutionReceipt = serde_json::from_slice(&resp.get_ref().receipt)?;
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "execute");
    // For execute receipts, result is BLAKE3 hash of result bytes
    assert!(!receipt.result.is_empty(), "result should be a hash");
    assert_eq!(
        receipt.result.len(),
        64,
        "result should be 64-char hex (BLAKE3)"
    );
    assert!(
        receipt.fuel_consumed > 0,
        "fuel_consumed should be > 0 for compute module, got {}",
        receipt.fuel_consumed
    );

    Ok(())
}

#[tokio::test]
async fn s802_fuel_exhaustion() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "hello world")?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::hostile_loop_module();

    let resp = run_execute_test(&certs, free_port(), Some(1000), wasm, config).await?;

    assert!(
        !resp.get_ref().success,
        "Execute should fail due to fuel exhaustion"
    );
    assert_eq!(
        resp.get_ref().error_message,
        "fuel budget exceeded",
        "Error message should be 'fuel budget exceeded', got: {}",
        resp.get_ref().error_message
    );
    assert!(
        !resp.get_ref().receipt.is_empty(),
        "Receipt should be present even on trap"
    );

    let receipt: ExecutionReceipt = serde_json::from_slice(&resp.get_ref().receipt)?;
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "execute");
    assert_eq!(receipt.result, "trap");
    assert_eq!(
        receipt.fuel_consumed, 1000,
        "fuel_consumed should equal budget (1000), got {}",
        receipt.fuel_consumed
    );

    Ok(())
}

#[tokio::test]
async fn e803_d4_arm_isolation_and_ordering() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "hello world")?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::hostile_loop_module();

    let resp = run_execute_test(&certs, free_port(), Some(1000), wasm, config).await?;

    assert!(!resp.get_ref().success);
    assert_eq!(
        resp.get_ref().error_message,
        "fuel budget exceeded",
        "Fuel trap should be classified as 'fuel budget exceeded', not fs error: {}",
        resp.get_ref().error_message
    );

    Ok(())
}

#[tokio::test]
async fn e804_budget_absent_uses_default() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "hello world")?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::safe_read_module();

    let resp = run_execute_test(&certs, free_port(), None, wasm, config).await?;

    assert!(
        resp.get_ref().success,
        "Execute should succeed with default budget"
    );

    let receipt: ExecutionReceipt = serde_json::from_slice(&resp.get_ref().receipt)?;
    assert!(
        receipt.fuel_consumed > 0 && receipt.fuel_consumed < 1000,
        "fuel_consumed should be small for simple read, got {}",
        receipt.fuel_consumed
    );

    Ok(())
}

#[tokio::test]
async fn e805_budget_explicit_honored() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let safe_file = test_root.path().join("safe_file.txt");
    std::fs::write(&safe_file, "hello world")?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::compute_then_read_module();

    let resp = run_execute_test(&certs, free_port(), Some(5_000_000), wasm, config.clone()).await?;

    assert!(
        resp.get_ref().success,
        "Execute should succeed with explicit budget"
    );

    let receipt: ExecutionReceipt = serde_json::from_slice(&resp.get_ref().receipt)?;
    assert!(
        receipt.fuel_consumed > 0 && receipt.fuel_consumed < 5_000_000,
        "fuel_consumed should be under explicit budget, got {}",
        receipt.fuel_consumed
    );

    let wasm2 = wasm_test_modules::hostile_loop_module();
    let resp2 = run_execute_test(&certs, free_port(), Some(5000), wasm2, config).await?;

    assert!(!resp2.get_ref().success);
    assert_eq!(resp2.get_ref().error_message, "fuel budget exceeded");

    let receipt2: ExecutionReceipt = serde_json::from_slice(&resp2.get_ref().receipt)?;
    assert_eq!(
        receipt2.fuel_consumed, 5000,
        "fuel_consumed should equal explicit budget (5000)"
    );

    Ok(())
}

#[tokio::test]
async fn regression_hostile_loop_epoch_still_works() -> Result<()> {
    let certs = TestCerts::generate()?;

    let test_root = TempDir::new()?;
    let allowed_root = test_root.path().to_str().unwrap().to_string();

    let config = filesystem_read_config(&allowed_root, 1024);
    let wasm = wasm_test_modules::hostile_loop_module();

    let resp = run_execute_test(&certs, free_port(), None, wasm, config).await?;

    assert!(!resp.get_ref().success);
    assert_eq!(resp.get_ref().error_message, "fuel budget exceeded");

    Ok(())
}
