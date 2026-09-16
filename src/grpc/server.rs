//! gRPC server implementation with tonic, mTLS, and semaphore limiting.
//!
//! The `AegisRuntimeService` handles all three RPCs and is wrapped in
//! `AegisRuntimeServer`. `ExecutionConfig.max_concurrent` semaphore limits
//! concurrent Execute RPCs, returning `RESOURCE_EXHAUSTED` when full.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::config::runtime::RuntimeConfig;
use crate::proto::aegis::v1::aegis_runtime_server::AegisRuntimeServer;
use crate::proto::aegis::v1::health_server::HealthServer;
use crate::receipts::ReceiptEmitter;
use crate::sandbox::load_receipt_keypair;

use super::handlers::health::HealthService;
use super::handlers::AegisRuntimeService;
use super::tls;

/// Start the gRPC server with the given configuration.
///
/// This is the main entry point for `aegis-runtime`. It:
/// 1. Loads the Ed25519 key pair from config
/// 2. Creates a shared `ReceiptEmitter`
/// 3. Sets up mTLS if configured
/// 4. Starts the tonic server with all services
///
/// # Errors
///
/// Returns `Err` on any startup failure (fail-closed, REQ-803).
pub async fn start_server(config: &RuntimeConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid server address: {}", e))?;

    start_server_internal(config, addr, shutdown_signal())
        .await
        .map(|_| ())
}

/// Internal server startup that accepts a pre-resolved address and shutdown future.
///
/// Used by `start_server` (production) and integration tests (port 0).
#[doc(hidden)]
pub async fn start_server_internal(
    config: &RuntimeConfig,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<SocketAddr> {
    // Create emitter internally (production path)
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key_pair = load_receipt_keypair(&config.receipts.key_path)
        .map_err(|e| anyhow::anyhow!("failed to load receipt key: {}", e))?;

    tracing::info!("receipt key loaded successfully");

    let receipt_emitter = Arc::new(Mutex::new(ReceiptEmitter::new(key_pair)));
    ReceiptEmitter::spawn_ttl_sweep(receipt_emitter.clone());

    start_server_with_emitter(config, addr, shutdown, receipt_emitter).await
}

/// Internal server startup that accepts a pre-created ReceiptEmitter.
///
/// Used by integration tests that need to retain a reference to the emitter
/// for test-only operations like `force_signing_failure()`.
#[doc(hidden)]
pub async fn start_server_internal_with_emitter(
    config: &RuntimeConfig,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    receipt_emitter: Arc<Mutex<ReceiptEmitter>>,
) -> anyhow::Result<SocketAddr> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Validate that the emitter has a valid key (test only)
    if config.receipts.key_path.exists() {
        let _ = load_receipt_keypair(&config.receipts.key_path)
            .map_err(|e| anyhow::anyhow!("failed to load receipt key: {}", e))?;
        tracing::info!("receipt key loaded successfully (test mode with injected emitter)");
    }

    start_server_with_emitter(config, addr, shutdown, receipt_emitter).await
}

/// Common server startup logic shared by both entry points.
async fn start_server_with_emitter(
    config: &RuntimeConfig,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    receipt_emitter: Arc<Mutex<ReceiptEmitter>>,
) -> anyhow::Result<SocketAddr> {
    // 3. Create semaphore for concurrency limiting
    let _semaphore = Arc::new(Semaphore::new(config.execution.max_concurrent));

    // 4. Build service instances
    let aegis_service = AegisRuntimeService {
        receipt_emitter: receipt_emitter.clone(),
        semaphore: Arc::clone(&_semaphore),
        fuel_budget: config.execution.fuel_budget,
    };
    let health_service = HealthService;

    // 5. Bind TCP listener BEFORE building tonic server (so we know the actual port)
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind {}: {}", addr, e))?;
    let local_addr = listener.local_addr()?;
    tracing::info!(addr = %local_addr, "starting gRPC server");

    // 6. Build tonic server with TLS if configured (REQ-713)
    let mut server = Server::builder();

    if let Some(ref tls_config) = config.server.tls {
        let tonic_tls_config = tls::build_tonic_tls_config(tls_config)?;
        server = server.tls_config(tonic_tls_config)?;
        tracing::info!("mTLS configured");
    } else {
        tracing::warn!("TLS not configured — connections are unencrypted");
    }

    // 7. Serve via TcpListenerStream (supports TLS accept internally)
    server
        .add_service(AegisRuntimeServer::new(aegis_service))
        .add_service(HealthServer::new(health_service))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {}", e))?;

    tracing::info!("gRPC server stopped");
    Ok(local_addr)
}

/// Wait for SIGTERM or SIGINT signal for graceful shutdown (REQ-806, REQ-807).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        tracing::info!("received SIGINT, shutting down");
    }
}
