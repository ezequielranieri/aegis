//! Network HTTP capability integration tests (Phase 4, tasks 4.1-4.5).
//!
//! Hermetic end-to-end tests for `aegis::http_fetch` (REQ-603..REQ-610):
//!
//! - S-601 endpoint allowlist (scheme/port/hostname), E-602 case-insensitive host
//! - S-603 DNS/connect failures, S-604 stalls, S-605 1 MiB cap, S-606 rate limit
//! - E-603 port 8443 trap, E-604 exact 1 MiB succeeds, E-605 2/1 burst
//! - REQ-609 fetch receipts (success + every trap path, signed + chain-verifiable)
//! - REQ-610 execute-level mapping via REAL gRPC (trap arm AND success arm —
//!   the success triple is driven through the handler's AEGIS_TEST_MODE-gated
//!   transport-env hook, see `network_execute_req610_success_via_grpc`) plus
//!   the sandbox-level D5 contract (`network_execute_req610_success_triple`).
//!
//! The TLS stub is an rcgen CA + `localhost` server certificate served by a
//! tokio-rustls acceptor on an ephemeral port (design.md §Testing Strategy).
//! The `test-utils` sandbox override (`network_test_port` + `network_test_ca_pem`)
//! redirects only the transport socket — URL-policy validation is unchanged.
//! Sandbox-level tests inject it directly (`point_at_stub`); the gRPC E2E tests
//! inject it through the handler via `AEGIS_TEST_NETWORK_PORT`/`AEGIS_TEST_CA_PEM`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair as RcgenKeyPair,
};
use ring::error::Unspecified;
use ring::signature::{Ed25519KeyPair, KeyPair as RingKeyPair};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tonic::transport::{Certificate as TonicCertificate, Channel, ClientTlsConfig, Identity};
use tonic::Request;

use aegis::capabilities::{Capability, NetworkHttpParams};
use aegis::config::runtime::{
    ExecutionConfig, ReceiptsConfig, RuntimeConfig, ServerConfig, TlsConfig,
};
use aegis::grpc::server::start_server_internal_with_emitter;
use aegis::proto::aegis::v1::{aegis_runtime_client::AegisRuntimeClient, ExecuteRequest};
use aegis::receipts::{ExecutionReceipt, ReceiptChain, ReceiptEmitter};
use aegis::sandbox::{load_receipt_keypair, Sandbox, SandboxConfig};
use wat::parse_str;

/// Enable test mode for the gRPC handler (disables epoch interruption).
fn enable_test_mode() {
    std::env::set_var("AEGIS_TEST_MODE", "1");
}

// ─── WAT module builders ────────────────────────────────────────────────────

/// WAT module calling `aegis::http_fetch` once, exporting `_start`.
/// Method data at 1024, URL at 2048, output buffer at 4096.
fn http_fetch_wat(method: &str, url: &str, out_len: i32, memory_pages: i32) -> String {
    format!(
        r#"(module
  (import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") {pages})
  (data (i32.const 1024) "{method}")
  (data (i32.const 2048) "{url}")
  (func (export "_start")
    (call $fetch (i32.const 1024) (i32.const {mlen}) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const {out_len}))
    drop
  )
)"#,
        pages = memory_pages,
        method = method,
        url = url,
        mlen = method.len(),
        ulen = url.len(),
        out_len = out_len,
    )
}

/// WAT module calling `aegis::http_fetch` three times (rate-limit burst),
/// exporting `_start`.
fn http_fetch_x3_wat(url: &str) -> String {
    format!(
        r#"(module
  (import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "GET")
  (data (i32.const 2048) "{url}")
  (func (export "_start")
    (call $fetch (i32.const 1024) (i32.const 3) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const 1024)) drop
    (call $fetch (i32.const 1024) (i32.const 3) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const 1024)) drop
    (call $fetch (i32.const 1024) (i32.const 3) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const 1024)) drop
  )
)"#,
        url = url,
        ulen = url.len(),
    )
}

/// WAT module calling `aegis::http_fetch` once and exporting `execute`
/// returning `(out_ptr, bytes_copied)` — the shape the gRPC handler expects.
fn http_fetch_execute_wat(method: &str, url: &str, out_len: i32) -> String {
    format!(
        r#"(module
  (import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{method}")
  (data (i32.const 2048) "{url}")
  (func (export "execute") (result i32 i32)
    (local $n i32)
    (call $fetch (i32.const 1024) (i32.const {mlen}) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const {out_len}))
    local.set $n
    i32.const 4096
    local.get $n
  )
)"#,
        method = method,
        url = url,
        mlen = method.len(),
        ulen = url.len(),
        out_len = out_len,
    )
}

// ─── Sandbox helpers ────────────────────────────────────────────────────────

/// Sandbox with a signed receipt emitter wired, ready for one network
/// capability. The test-utils transport override is applied by the caller.
fn network_sandbox(
    hosts: &[&str],
    methods: &[&str],
    rate: u64,
) -> (Sandbox, Capability, Ed25519KeyPair) {
    network_sandbox_with_memory(hosts, methods, rate, SandboxConfig::default().memory_size)
}

/// Variant with an explicit linear-memory ceiling (bytes). E-604 needs a
/// guest buffer of exactly 1 MiB; the default 1 MiB cap would trap on the
/// `memory.grow` that allocates buffer + overhead.
fn network_sandbox_with_memory(
    hosts: &[&str],
    methods: &[&str],
    rate: u64,
    memory_size: usize,
) -> (Sandbox, Capability, Ed25519KeyPair) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
    // Re-derive from the same PKCS8 bytes so the emitter signs with the exact
    // key we verify against (ring's Ed25519KeyPair is not Clone).
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("key parse");

    let config = SandboxConfig {
        memory_size,
        ..SandboxConfig::default()
    };
    let mut sandbox = Sandbox::new_with_config(config, false).expect("failed to create sandbox");
    let emitter =
        ReceiptEmitter::new(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("emitter key parse"));
    sandbox.store_mut().data_mut().receipt_emitter = Some(Arc::new(Mutex::new(emitter)));

    let cap = Capability::NetworkHttp(NetworkHttpParams {
        allowed_hosts: hosts.iter().map(|s| s.to_string()).collect(),
        allowed_methods: methods.iter().map(|s| s.to_string()).collect(),
        max_requests_per_second: rate,
    });

    (sandbox, cap, key_pair)
}

/// Point the sandbox's transport at the TLS stub (task 2.7 override):
/// socket port + CA trust root. URL-policy validation is unaffected.
fn point_at_stub(sandbox: &mut Sandbox, stub: &TlsStub) {
    let state = sandbox.store_mut().data_mut();
    state.network_test_port = Some(stub.addr.port());
    state.network_test_ca_pem = Some(stub.ca_pem.clone());
}

/// Instantiate `_start` from a WAT source and return the typed function.
fn instantiate_start(
    sandbox: &mut Sandbox,
    cap: &Capability,
    wat_src: &str,
) -> wasmtime::TypedFunc<(), ()> {
    let wasm = parse_str(wat_src).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, std::slice::from_ref(cap))
        .expect("module should instantiate");
    instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("_start export")
}

/// Instantiate an `execute` module (returns `(ptr, len)`) for the
/// handler-shaped flow.
fn instantiate_execute(
    sandbox: &mut Sandbox,
    cap: &Capability,
    wat_src: &str,
) -> (wasmtime::Instance, wasmtime::TypedFunc<(), (i32, i32)>) {
    let wasm = parse_str(wat_src).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, std::slice::from_ref(cap))
        .expect("module should instantiate");
    let func = instance
        .get_typed_func::<(), (i32, i32)>(sandbox.store_mut(), "execute")
        .expect("execute export");
    (instance, func)
}

/// wasmtime 24 wraps host errors in a backtrace error; the custom message
/// lives in the "Caused by:" chain, not in `to_string()`.
fn error_chain_contains(err: &wasmtime::Error, needle: &str) -> bool {
    if err.to_string().contains(needle) {
        return true;
    }
    let mut source: Option<&dyn std::error::Error> = err.source();
    while let Some(s) = source {
        if s.to_string().contains(needle) {
            return true;
        }
        source = s.source();
    }
    format!("{:?}", err).contains(needle)
}

fn assert_network_trap(err: &wasmtime::Error, needle: &str) {
    assert!(
        error_chain_contains(err, needle),
        "trap message must contain {:?}, got: {:?}",
        needle,
        err
    );
}

/// Assert the exact fields of one fetch receipt in the chain (REQ-609).
fn assert_fetch_receipt(
    chain: &[ExecutionReceipt],
    index: usize,
    path: &str,
    size: u64,
    result: &str,
    pub_key: &[u8],
) {
    let receipt = &chain[index];
    assert_eq!(
        receipt.capability_name, "network.http",
        "receipt {index} capability"
    );
    assert_eq!(receipt.action, "fetch", "receipt {index} action");
    assert_eq!(receipt.path, path, "receipt {index} path");
    assert_eq!(receipt.size, size, "receipt {index} size");
    assert_eq!(receipt.result, result, "receipt {index} result");
    let valid = receipt
        .verify_signature(pub_key)
        .expect("signature verification must not error");
    assert!(valid, "receipt {index} signature must verify");
}

// ─── TLS stub server (design.md §Testing Strategy) ──────────────────────────

/// Stub behaviour for a single TLS connection.
#[derive(Clone)]
enum StubMode {
    /// Reply `200 OK` with the given body once the request head arrives.
    Respond(Vec<u8>),
    /// Accept the connection, read the request, then never respond — the
    /// client's global timeout (5s) must fire (S-604, total stage).
    Stall,
    /// Accept the TCP connection but NEVER complete the TLS handshake —
    /// the client's connect timeout (2s) must fire during handshake
    /// establishment (S-604, connect stage). Bounded: closes after 3s.
    StallHandshake,
}

/// Serializes gRPC tests that set `AEGIS_TEST_NETWORK_PORT` /
/// `AEGIS_TEST_CA_PEM` against tests that rely on `AEGIS_TEST_MODE` alone.
/// Env vars are process-global, so `network_execute_traps_via_grpc` (S-603
/// relies on localhost:443 being refused, i.e. NO transport override active)
/// must never run while the override env is set.
static GRPC_NETWORK_ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
    std::sync::OnceLock::new();

async fn grpc_network_env_guard() -> tokio::sync::MutexGuard<'static, ()> {
    GRPC_NETWORK_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// RAII guard for the transport-override env vars (`AEGIS_TEST_NETWORK_PORT`,
/// `AEGIS_TEST_CA_PEM`). Captures the previous state on acquisition and
/// restores it in `Drop` — unlike manual `remove_var`, it never assumes the
/// vars were unset before, and it runs even on panic or early return, so a
/// failed test can never leak a phantom override into the process for a later
/// sibling test.
struct TestNetworkEnvGuard {
    _gate: tokio::sync::MutexGuard<'static, ()>,
    saved_port: Option<String>,
    saved_ca: Option<String>,
}

impl TestNetworkEnvGuard {
    /// Serializes against the other gRPC env-mutating tests AND snapshots the
    /// current env state so `Drop` can restore it.
    async fn acquire() -> Self {
        let _gate = grpc_network_env_guard().await;
        Self {
            _gate,
            saved_port: std::env::var("AEGIS_TEST_NETWORK_PORT").ok(),
            saved_ca: std::env::var("AEGIS_TEST_CA_PEM").ok(),
        }
    }

    /// Point the handler-created sandbox transport at a test stub.
    fn set(&self, port: u16, ca_pem_path: &std::path::Path) {
        std::env::set_var("AEGIS_TEST_NETWORK_PORT", port.to_string());
        std::env::set_var("AEGIS_TEST_CA_PEM", ca_pem_path);
    }

    /// Ensure no transport override is active (e.g. the traps test relies on
    /// localhost:443 being refused — the default, non-overridden transport).
    fn clear(&self) {
        std::env::remove_var("AEGIS_TEST_NETWORK_PORT");
        std::env::remove_var("AEGIS_TEST_CA_PEM");
    }
}

impl Drop for TestNetworkEnvGuard {
    fn drop(&mut self) {
        match &self.saved_port {
            Some(p) => std::env::set_var("AEGIS_TEST_NETWORK_PORT", p),
            None => std::env::remove_var("AEGIS_TEST_NETWORK_PORT"),
        }
        match &self.saved_ca {
            Some(c) => std::env::set_var("AEGIS_TEST_CA_PEM", c),
            None => std::env::remove_var("AEGIS_TEST_CA_PEM"),
        }
    }
}

/// An rcgen CA + `localhost`-certificate HTTPS stub on an ephemeral port.
struct TlsStub {
    addr: SocketAddr,
    ca_pem: Vec<u8>,
    /// Accepted TCP connections so far — proves a fetch really reached the
    /// stub (REQ-610 success E2E scenario evidence).
    requests: Arc<AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl TlsStub {
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for TlsStub {
    fn drop(&mut self) {
        // `oneshot::Sender::send` consumes the sender, so `take` it out.
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

/// Bind an HTTPS stub and spawn its accept loop on the runtime.
async fn spawn_stub(mode: StubMode) -> Result<TlsStub> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // CA (self-signed)
    let mut ca_params = CertificateParams::new(vec!["aegis-network-test-ca".into()])?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name = DistinguishedName::new();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "aegis-network-test-ca");
    let ca_key = RcgenKeyPair::generate()?;
    let ca_cert = ca_params.self_signed(&ca_key)?;

    // Server certificate with SAN DNS `localhost`, signed by the CA. The
    // sandbox fetches `https://localhost/<path>` and the test override only
    // redirects the socket, so hostname verification needs this SAN.
    let mut server_params = CertificateParams::new(vec!["localhost".into()])?;
    server_params.distinguished_name = DistinguishedName::new();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    let server_key = RcgenKeyPair::generate()?;
    let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key)?;

    let cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().as_ref().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
    );

    let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert_der], key_der)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_loop = Arc::clone(&requests);

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (tcp, _peer) = match accepted {
                        Ok(socket) => socket,
                        Err(_) => continue,
                    };
                    requests_loop.fetch_add(1, Ordering::SeqCst);
                    let acceptor = acceptor.clone();
                    let mode = mode.clone();
                    tokio::spawn(async move {
                        match mode {
                            // S-604 connect stage: keep the TCP connection open
                            // without ever completing the TLS handshake — ureq
                            // runs handshake establishment under
                            // `Timeout::Connect`, so the 2s connect timeout
                            // fires while the ClientHello is unanswered.
                            StubMode::StallHandshake => {
                                tokio::time::sleep(Duration::from_secs(3)).await;
                            }
                            mode => {
                                if let Ok(tls) = acceptor.accept(tcp).await {
                                    handle_conn(tls.into(), mode).await
                                }
                            }
                        }
                    });
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    Ok(TlsStub {
        addr,
        ca_pem: ca_cert.pem().into_bytes(),
        requests,
        shutdown: Some(shutdown_tx),
        handle,
    })
}

/// Serve one TLS connection according to the stub mode.
async fn handle_conn(mut tls: tokio_rustls::TlsStream<tokio::net::TcpStream>, mode: StubMode) {
    match mode {
        StubMode::Respond(body) => {
            // Drain until the request head (`\r\n\r\n`) arrives.
            let mut buf = [0u8; 8192];
            loop {
                match tls.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n\r\n",
                body.len()
            );
            let mut response = head.into_bytes();
            response.extend_from_slice(&body);
            let _ = tls.write_all(&response).await;
            let _ = tls.shutdown().await;
        }
        StubMode::Stall => {
            let mut buf = [0u8; 4096];
            let _ = tls.read(&mut buf).await;
            // Hold the connection open without ever completing the response.
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
        // Unreachable — the accept loop filters StallHandshake before the TLS
        // accept; kept only for match exhaustiveness.
        StubMode::StallHandshake => {}
    }
}

// ─── Phase 4 tests ──────────────────────────────────────────────────────────

// 4.1 — happy GET E2E through the TLS stub (REQ-603, E-603 allowlist).

/// Success path E2E: allowed host + GET + free bucket through the real TLS
/// stub. Asserts the bytes landed in guest memory, the host returned the
/// copied length, and REQ-609 emitted a signed success receipt with
/// `path = URL`, `size = body length`, `result = BLAKE3(body)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_https_get_success_e2e() -> Result<()> {
    let body = b"hello from aegis tls stub".to_vec();
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let (instance, func) = instantiate_execute(
        &mut sandbox,
        &cap,
        &http_fetch_execute_wat("GET", "https://localhost/ok", 1024),
    );
    let (ptr, len) = func
        .call(sandbox.store_mut(), ())
        .expect("fetch must succeed");
    assert_eq!(len as usize, body.len(), "host fn must return bytes copied");

    let fetched = {
        let memory = instance
            .get_memory(&mut *sandbox.store_mut(), "memory")
            .expect("memory export");
        let data = memory.data(&mut *sandbox.store_mut());
        data[ptr as usize..(ptr as usize + len as usize)].to_vec()
    };
    assert_eq!(fetched, body, "guest buffer must contain the wire body");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "exactly one fetch receipt (REQ-609)");
    let expected_hash = blake3::hash(&body).to_hex().to_string();
    assert_fetch_receipt(
        &chain,
        0,
        "https://localhost/ok",
        body.len() as u64,
        &expected_hash,
        &pub_key,
    );

    Ok(())
}

/// E-603: explicit non-443 port in the URL must trap S-601 BEFORE any
/// transport I/O — no stub needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_endpoint_https_port_8443_traps_e603() -> Result<()> {
    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost:8443/x", 1024, 1),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("E-603 must trap");
    assert_network_trap(
        &err,
        "network endpoint not allowed: https://localhost:8443/x",
    );

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost:8443/x", 0, "trap", &pub_key);

    Ok(())
}

// 4.2 — S-603..S-606 + buffer-too-small trap receipts (AD-005 class).

/// S-603: DNS / connect failure — nothing listens on localhost:443, so the
/// transport stage traps `network connection failed` with a trap receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_connection_refused_s603() -> Result<()> {
    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    // No `point_at_stub` — validation passes (allowlisted host, https, :443
    // implied), the real socket to localhost:443 is refused.

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1024, 1),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("S-603 must trap");
    assert_network_trap(&err, "network connection failed");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost/x", 0, "trap", &pub_key);

    Ok(())
}

/// S-604: a server that accepts the connection but never completes the
/// response hits the global 5s timeout → `network timeout` trap (bounded:
/// the test completes in ~5s wall).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_stall_timeout_s604() -> Result<()> {
    let stub = spawn_stub(StubMode::Stall).await?;
    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1024, 1),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("S-604 must trap");
    assert_network_trap(&err, "network timeout: total > 5s");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost/x", 0, "trap", &pub_key);

    Ok(())
}

/// S-604 connect-stage disjunct: the stub ACCEPTS the TCP connection but
/// never completes the TLS handshake. ureq performs handshake establishment
/// under `Timeout::Connect`, so the 2s connect timeout fires (bounded: the
/// test completes in ~2s wall, the stub closes after 3s).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_connect_stage_timeout_s604() -> Result<()> {
    let stub = spawn_stub(StubMode::StallHandshake).await?;
    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1024, 1),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("S-604 connect stage must trap");
    assert_network_trap(&err, "network timeout: connect > 2s");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost/x", 0, "trap", &pub_key);

    Ok(())
}

/// S-605: a 1 MiB + 1 byte body exceeds the cap and traps; the trap receipt
/// carries the captured (oversized) byte count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_oversize_response_s605() -> Result<()> {
    let body = vec![0x00u8; (1 << 20) + 1]; // 1 MiB + 1
    let stub = spawn_stub(StubMode::Respond(body)).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1024, 1),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("S-605 must trap");
    assert_network_trap(&err, "network response size 1048577 exceeds limit 1048576");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost/x", 1048577, "trap", &pub_key);

    Ok(())
}

/// S-606 + E-605: bucket = 2 → two burst fetches succeed, the third traps
/// `network rate limit exceeded: bucket empty` BEFORE any transport I/O.
/// One signed receipt per attempt (REQ-609), chain intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_rate_burst_2_of_3_s606_and_e605() -> Result<()> {
    let body = b"rate limited body".to_vec();
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 2);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_x3_wat("https://localhost/x"),
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("third fetch must trap S-606");
    assert_network_trap(&err, "network rate limit exceeded: bucket empty");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 3, "one receipt per attempt (REQ-609)");
    let expected_hash = blake3::hash(&body).to_hex().to_string();
    assert_fetch_receipt(
        &chain,
        0,
        "https://localhost/x",
        body.len() as u64,
        &expected_hash,
        &pub_key,
    );
    assert_fetch_receipt(
        &chain,
        1,
        "https://localhost/x",
        body.len() as u64,
        &expected_hash,
        &pub_key,
    );
    assert_fetch_receipt(&chain, 2, "https://localhost/x", 0, "trap", &pub_key);

    Ok(())
}

/// Buffer-too-small (AD-005 class, REQ-609): the guest buffer is smaller than
/// the fetched body. The remote fetch was already observed, so the host emits
/// a trap receipt with the REAL fetched size before trapping
/// `output buffer too small`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_output_buffer_too_small_trap_receipt() -> Result<()> {
    let body = b"hello from aegis tls stub".to_vec(); // 25 bytes
    let stub = spawn_stub(StubMode::Respond(body)).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 16, 1), // out buffer < body
    );
    let err = func
        .call(sandbox.store_mut(), ())
        .expect_err("undersized buffer must trap");
    assert_network_trap(&err, "output buffer too small");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one trap receipt (REQ-609)");
    assert_fetch_receipt(&chain, 0, "https://localhost/x", 25, "trap", &pub_key);

    Ok(())
}

// 4.4 — E-602 case-insensitive host, E-604 exact 1 MiB, chain verification.

/// E-602: `https://LOCALHOST/ok` passes the endpoint allowlist
/// (`localhost`) case-insensitively and the fetch succeeds through the stub.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_host_case_insensitive_e602() -> Result<()> {
    let body = b"case-insensitive body".to_vec();
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://LOCALHOST/ok", 1024, 1),
    );
    func.call(sandbox.store_mut(), ())
        .expect("E-602: case-insensitive host must be allowed");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "success receipt (REQ-609)");
    let expected_hash = blake3::hash(&body).to_hex().to_string();
    // The host records the URL exactly as fetched (guest-passed casing).
    assert_fetch_receipt(
        &chain,
        0,
        "https://LOCALHOST/ok",
        body.len() as u64,
        &expected_hash,
        &pub_key,
    );

    Ok(())
}

/// E-604: a body of exactly 1 MiB (1,048,576 bytes) succeeds — the cap is
/// `< limit`, not `<= limit` (REQ-608).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_exact_1_mib_succeeds_e604() -> Result<()> {
    let body = vec![0xABu8; 1 << 20]; // exactly 1 MiB
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;

    let (mut sandbox, cap, key_pair) =
        network_sandbox_with_memory(&["localhost"], &["GET"], 10, 2 << 20);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1 << 20, 17), // 17 pages hold 1 MiB + 4096
    );
    func.call(sandbox.store_mut(), ())
        .expect("E-604: exactly 1 MiB must succeed");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "success receipt (REQ-609)");
    let expected_hash = blake3::hash(&body).to_hex().to_string();
    assert_fetch_receipt(
        &chain,
        0,
        "https://localhost/x",
        1 << 20,
        &expected_hash,
        &pub_key,
    );

    Ok(())
}

/// REQ-609 + chain integrity: the receipt chain produced by a real fetch
/// verifies with `ReceiptChain::verify_chain` (signature + hash chain +
/// timestamp window).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_fetch_receipt_chain_verifies() -> Result<()> {
    let stub = spawn_stub(StubMode::Respond(b"chain verify body".to_vec())).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    let func = instantiate_start(
        &mut sandbox,
        &cap,
        &http_fetch_wat("GET", "https://localhost/x", 1024, 1),
    );
    func.call(sandbox.store_mut(), ())
        .expect("fetch must succeed");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one fetch receipt");
    ReceiptChain::verify_chain(&chain, &pub_key)
        .expect("valid chain must verify (REQ-609, REQ-410..412)");

    Ok(())
}

// ─── gRPC harness (4.5 — own harness, grpc_boundary.rs untouched) ───────────

/// mTLS certificate material for the gRPC server + client (mirrors
/// grpc_boundary's TestCerts pattern).
struct GrpcCerts {
    temp_dir: TempDir,
}

impl GrpcCerts {
    /// Generate CA → server (CN=aegis-runtime) → client (CN=agent-gateway)
    /// plus an Ed25519 receipt signing key.
    fn generate() -> Result<Self> {
        let temp_dir = TempDir::new()?;

        let mut ca_params = CertificateParams::new(vec!["aegis-test-ca".into()])?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = DistinguishedName::new();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "aegis-test-ca");
        let ca_key = RcgenKeyPair::generate()?;
        let ca_cert = ca_params.self_signed(&ca_key)?;

        let mut server_params = CertificateParams::new(vec!["aegis-runtime".into()])?;
        server_params.distinguished_name = DistinguishedName::new();
        server_params
            .distinguished_name
            .push(DnType::CommonName, "aegis-runtime");
        let server_key = RcgenKeyPair::generate()?;
        let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key)?;

        let mut client_params = CertificateParams::new(vec!["agent-gateway".into()])?;
        client_params.distinguished_name = DistinguishedName::new();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "agent-gateway");
        let client_key = RcgenKeyPair::generate()?;
        let client_cert = client_params.signed_by(&client_key, &ca_cert, &ca_key)?;

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

        // Ed25519 signing key for receipts (base64 PKCS8 in TOML, 0600 perms)
        let rng = ring::rand::SystemRandom::new();
        // ring errors do not implement std::error::Error — map to anyhow.
        let signing_key_pair =
            Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e: Unspecified| anyhow::anyhow!(e))?;
        let private_key_b64 =
            base64::engine::general_purpose::STANDARD.encode(signing_key_pair.as_ref());
        let key_toml = format!(
            r#"
[signing_key]
private_key = "{}"
"#,
            private_key_b64
        );
        let key_path = temp_dir.path().join("signing_key.toml");
        std::fs::write(&key_path, key_toml)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(Self { temp_dir })
    }

    fn ca_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join("ca.crt")
    }
    fn server_cert_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join("server.crt")
    }
    fn server_key_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join("server.key")
    }
    fn client_cert_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join("client.crt")
    }
    fn client_key_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join("client.key")
    }
    fn signing_key_path(&self) -> std::path::PathBuf {
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

/// gRPC test server handle (mirrors grpc_boundary's TestServer).
struct TestServer {
    addr: SocketAddr,
    _shutdown: oneshot::Sender<()>,
    _handle: JoinHandle<anyhow::Result<SocketAddr>>,
}

impl TestServer {
    /// Start a test gRPC server with a pre-created ReceiptEmitter.
    async fn start_with_emitter(
        config: RuntimeConfig,
    ) -> Result<(Self, Arc<Mutex<ReceiptEmitter>>)> {
        let key_path = config.receipts.key_path.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let addr: SocketAddr = format!("127.0.0.1:{}", config.server.port).parse()?;

        let key_pair = load_receipt_keypair(&key_path)?;
        let receipt_emitter = Arc::new(Mutex::new(ReceiptEmitter::new(key_pair)));
        let emitter_for_server = Arc::clone(&receipt_emitter);

        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };
        let handle = tokio::spawn(async move {
            start_server_internal_with_emitter(&config, addr, shutdown_fut, emitter_for_server)
                .await
        });
        wait_for_server(addr).await;

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

/// mTLS gRPC client (mirrors grpc_boundary's create_client).
async fn create_client(certs: &GrpcCerts, addr: SocketAddr) -> Result<AegisRuntimeClient<Channel>> {
    let ca_pem = std::fs::read(certs.ca_path())?;
    let client_cert_pem = std::fs::read(certs.client_cert_path())?;
    let client_key_pem = std::fs::read(certs.client_key_path())?;

    let tls = ClientTlsConfig::new()
        .ca_certificate(TonicCertificate::from_pem(ca_pem))
        .identity(Identity::from_pem(client_cert_pem, client_key_pem))
        .domain_name("aegis-runtime");

    let channel = Channel::from_shared(format!("https://{}", addr))?
        .tls_config(tls)?
        .connect()
        .await?;

    Ok(AegisRuntimeClient::new(channel))
}

/// RuntimeConfig with mTLS for a test-allocated ephemeral port.
fn create_test_config(certs: &GrpcCerts, port: u16) -> RuntimeConfig {
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

/// network.http capability config TOML for Execute requests.
fn network_http_config(hosts: &[&str], rate: u64) -> Vec<u8> {
    let hosts_list = hosts
        .iter()
        .map(|h| format!("\"{}\"", h))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "capabilities = [{{ name = \"network.http\", allowed_hosts = [{}], allowed_methods = [\"GET\"], max_requests_per_second = {} }}]",
        hosts_list, rate
    )
    .into_bytes()
}

fn execute_request(
    capability_name: &str,
    config_bytes: Vec<u8>,
    wasm_module: Vec<u8>,
) -> ExecuteRequest {
    ExecuteRequest {
        capability_name: capability_name.into(),
        config: config_bytes,
        wasm_module,
    }
}

// ─── 4.5 — REQ-610 execute-level mapping ────────────────────────────────────

/// REQ-610 trap acceptance over REAL gRPC + mTLS (the full handler path):
/// S-601 (host not in allowlist) and S-603 (transport refused) both map to
/// `success=false` with the D4 network message, and every attempt lands a
/// signed trap receipt (fetch + execute) in the chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_execute_traps_via_grpc() -> Result<()> {
    // Exclude the transport-override env vars: S-603 below relies on
    // localhost:443 being refused, which breaks if a transport override is
    // active (even one leftover from a failed sibling test).
    let _env_guard = TestNetworkEnvGuard::acquire().await;
    _env_guard.clear();
    enable_test_mode();
    let certs = GrpcCerts::generate()?;
    let config = create_test_config(&certs, free_port());
    let (server, _emitter) = TestServer::start_with_emitter(config).await?;
    let mut client = create_client(&certs, server.addr()).await?;

    // S-601: endpoint outside the allowlist → trap before any transport I/O.
    let req1 = execute_request(
        "network.http",
        network_http_config(&["localhost"], 10),
        parse_str(http_fetch_execute_wat("GET", "https://evil.com/x", 1024))?,
    );
    let resp1 = client.execute(Request::new(req1)).await?;
    assert!(!resp1.get_ref().success, "S-601 must fail");
    assert_eq!(
        resp1.get_ref().error_message,
        "network endpoint not allowed"
    );

    // S-603: allowlisted host, but nothing listens on localhost:443.
    let req2 = execute_request(
        "network.http",
        network_http_config(&["localhost"], 10),
        parse_str(http_fetch_execute_wat("GET", "https://localhost/x", 1024))?,
    );
    let resp2 = client.execute(Request::new(req2)).await?;
    assert!(!resp2.get_ref().success, "S-603 must fail");
    assert_eq!(resp2.get_ref().error_message, "network connection failed");

    // Both executes emitted their receipts into the shared chain:
    // With two-phase: [prepare1, fetch trap S-601, abort1, prepare2, fetch trap S-603, abort2] = 6 receipts.
    let chain = {
        let emitter = _emitter.lock().expect("emitter lock");
        emitter.chain().to_vec()
    };
    assert_eq!(
        chain.len(),
        6,
        "two prepare + two abort + two fetch traps = 6 receipts"
    );
    // Receipt 0: prepare for execute 1
    assert_eq!(chain[0].action, "execute");
    assert_eq!(chain[0].phase, "prepare");
    assert_eq!(chain[0].result, "pending");
    // Receipt 1: fetch trap S-601
    assert_eq!(chain[1].capability_name, "network.http");
    assert_eq!(chain[1].action, "fetch");
    assert_eq!(chain[1].path, "https://evil.com/x");
    assert_eq!(chain[1].size, 0);
    assert_eq!(chain[1].result, "trap");
    // Receipt 2: abort for execute 1
    assert_eq!(chain[2].action, "execute");
    assert_eq!(chain[2].phase, "abort");
    assert_eq!(chain[2].result, "aborted");
    // Receipt 3: prepare for execute 2
    assert_eq!(chain[3].action, "execute");
    assert_eq!(chain[3].phase, "prepare");
    assert_eq!(chain[3].result, "pending");
    // Receipt 4: fetch trap S-603
    assert_eq!(chain[4].capability_name, "network.http");
    assert_eq!(chain[4].action, "fetch");
    assert_eq!(chain[4].path, "https://localhost/x");
    assert_eq!(chain[4].size, 0);
    assert_eq!(chain[4].result, "trap");
    // Receipt 5: abort for execute 2
    assert_eq!(chain[5].action, "execute");
    assert_eq!(chain[5].phase, "abort");
    assert_eq!(chain[5].result, "aborted");

    let key_pair = load_receipt_keypair(&certs.signing_key_path())?;
    let public_key = key_pair.public_key().as_ref().to_vec();
    ReceiptChain::verify_chain(&chain, &public_key).expect("trap chain must verify (REQ-609)");

    let _ = server._shutdown.send(());
    Ok(())
}

/// REQ-610 SUCCESS arm over REAL gRPC + mTLS (the full handler path): the
/// handler-created sandbox receives the `test-utils` transport override via
/// the AEGIS_TEST_MODE-gated env hook (`AEGIS_TEST_NETWORK_PORT` +
/// `AEGIS_TEST_CA_PEM`, `src/grpc/handlers/mod.rs` — compiled only under the
/// `test-utils` feature), so a REAL successful fetch is driven through the
/// Execute boundary. Asserts the execute receipt chain ends with the REQ-610
/// triple: `path` = the URL actually fetched, `result` = BLAKE3(body) hex,
/// `size` = body bytes; `success == true`; the response embeds the chain's
/// last receipt; and the stub request counter proves the fetch really went
/// through the TLS stub.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_execute_req610_success_via_grpc() -> Result<()> {
    // Serialize with `network_execute_traps_via_grpc`: env vars are
    // process-global and the trap test's S-603 case relies on localhost:443
    // being refused (no transport override active).
    let _env_guard = TestNetworkEnvGuard::acquire().await;
    enable_test_mode();

    let body = b"stub body for req610 grpc e2e".to_vec();
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;
    let ca_temp = TempDir::new()?;
    let ca_pem_path = ca_temp.path().join("network-test-ca.pem");
    std::fs::write(&ca_pem_path, &stub.ca_pem)?;
    _env_guard.set(stub.addr.port(), &ca_pem_path);

    let certs = GrpcCerts::generate()?;
    let config = create_test_config(&certs, free_port());
    let (server, emitter) = TestServer::start_with_emitter(config).await?;
    let mut client = create_client(&certs, server.addr()).await?;

    let req = execute_request(
        "network.http",
        network_http_config(&["localhost"], 10),
        parse_str(http_fetch_execute_wat("GET", "https://localhost/ok", 1024))?,
    );
    let resp = client.execute(Request::new(req)).await?.into_inner();
    assert!(
        resp.success,
        "REQ-610 success arm must succeed through the gRPC boundary"
    );
    assert!(resp.error_message.is_empty(), "no error message on success");
    assert_eq!(
        resp.result, body,
        "execute result bytes must be the fetched body"
    );

    // REQ-610 triple on the LAST (execute commit) receipt: path = actually-fetched
    // URL, result = BLAKE3(body) hex, size = body bytes.
    let expected_hash = blake3::hash(&body).to_hex().to_string();
    let chain = { emitter.lock().expect("emitter lock").chain().to_vec() };
    // With two-phase Execute (Prepare+Commit), chain has: prepare + fetch + commit = 3 receipts
    // Order: prepare (created first) -> fetch (during WASM execution in commit) -> commit (final)
    assert_eq!(
        chain.len(),
        3,
        "prepare receipt + fetch receipt + execute commit receipt"
    );
    // chain[0] is prepare receipt (phase="prepare", result="pending")
    assert_eq!(chain[0].action, "execute");
    assert_eq!(chain[0].phase, "prepare");
    assert_eq!(chain[0].result, "pending");
    // chain[1] is fetch receipt
    assert_eq!(chain[1].action, "fetch");
    assert_eq!(chain[1].path, "https://localhost/ok");
    assert_eq!(chain[1].size, body.len() as u64);
    assert_eq!(chain[1].result, expected_hash);
    // chain[2] is commit receipt (phase="commit")
    assert_eq!(chain[2].action, "execute");
    assert_eq!(chain[2].phase, "commit");
    assert_eq!(
        chain[2].path, "https://localhost/ok",
        "execute commit receipt path = fetched URL (REQ-610)"
    );
    assert_eq!(
        chain[2].result, expected_hash,
        "execute commit receipt result = BLAKE3(body) hex (REQ-610)"
    );
    assert_eq!(
        chain[2].size,
        body.len() as u64,
        "execute commit receipt size = body bytes (REQ-610)"
    );

    // The response embeds the exact execute commit receipt from the shared chain.
    assert_eq!(
        resp.receipt,
        serde_json::to_vec(chain.last().expect("execute commit receipt"))?,
        "ExecuteResponse.receipt must serialize the chain's last receipt"
    );

    let key_pair = load_receipt_keypair(&certs.signing_key_path())?;
    let public_key = key_pair.public_key().as_ref().to_vec();
    ReceiptChain::verify_chain(&chain, &public_key).expect("success chain must verify (REQ-609)");

    // Scenario proof: the fetch really traversed the TLS stub.
    assert_eq!(
        stub.requests(),
        1,
        "the E2E fetch must hit the TLS stub exactly once"
    );

    // `_env_guard`'s Drop restores the env vars to their pre-test state.
    let _ = server._shutdown.send(());
    Ok(())
}

/// REQ-610 success contract at the D5 data level: the `FetchRecord` captured
/// by a REAL fetch through the stub carries exactly the execute-level receipt
/// triple — `path` = the fetched URL, `result` = BLAKE3(body) hex, `size` =
/// body byte length. This pins the data contract the handler arm consumes
/// (`Some(fetch) => (fetch.url, fetch.body_blake3, fetch.body_len)`); the
/// full Execute-boundary E2E is `network_execute_req610_success_via_grpc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_execute_req610_success_triple() -> Result<()> {
    let body = b"stub body for req610".to_vec();
    let stub = spawn_stub(StubMode::Respond(body.clone())).await?;

    let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 10);
    let pub_key = key_pair.public_key().as_ref().to_vec();
    point_at_stub(&mut sandbox, &stub);

    // Handler-shaped flow: test-mode sandbox + signed emitter + execute module.
    let (instance, func) = instantiate_execute(
        &mut sandbox,
        &cap,
        &http_fetch_execute_wat("GET", "https://localhost/ok", 1024),
    );
    let (ptr, len) = func
        .call(sandbox.store_mut(), ())
        .expect("fetch must succeed");
    let fetched = {
        let memory = instance
            .get_memory(&mut *sandbox.store_mut(), "memory")
            .expect("memory export");
        let data = memory.data(&mut *sandbox.store_mut());
        data[ptr as usize..(ptr as usize + len as usize)].to_vec()
    };
    assert_eq!(fetched, body, "wire body must reach the guest buffer");

    // D5 record — the exact triple the handler arm reads (REQ-610).
    let fetch = sandbox
        .store_mut()
        .data_mut()
        .network_fetch
        .take()
        .expect("FetchRecord must be set on success");
    assert_eq!(
        fetch.url, "https://localhost/ok",
        "path = actually-fetched URL"
    );
    assert_eq!(
        fetch.body_blake3,
        blake3::hash(&body).to_hex().to_string(),
        "result = BLAKE3(body) hex"
    );
    assert_eq!(fetch.body_len, body.len() as u64, "size = response bytes");

    // The fetch receipt in the chain carries the same triple (REQ-609).
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "one success receipt");
    assert_eq!(chain[0].capability_name, "network.http");
    assert_eq!(chain[0].path, fetch.url);
    assert_eq!(chain[0].result, fetch.body_blake3);
    assert_eq!(chain[0].size, fetch.body_len);
    ReceiptChain::verify_chain(&chain, &pub_key).expect("chain must verify");

    Ok(())
}
