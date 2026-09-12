//! Aegis - Secure WASM Runtime with Capability-Based Security
//!
//! Features:
//! - Wasmtime-based sandboxing
//! - Capability-based security via host functions
//! - Signed execution receipts with hash-chaining
//! - Declarative policy engine
//! - OpenTelemetry observability

pub mod capabilities;
pub mod config;
pub mod grpc;
pub mod observability;
pub mod policy;
pub mod receipts;
pub mod sandbox;

/// Generated protobuf code from `proto/aegis/v1/aegis.proto`
#[allow(clippy::result_large_err)]
pub mod proto {
    pub mod aegis {
        pub mod v1 {
            tonic::include_proto!("aegis.v1");
        }
    }
}

pub use config::runtime::{ExecutionConfig, RuntimeConfig, ServerConfig, TlsConfig};
pub use config::{CapabilityDef, ConfigError, PolicyConfig, ReceiptsConfig};
pub use sandbox::{
    load_receipt_keypair, CapabilityConfig, EpochInterrupter, Sandbox, SandboxConfig,
};

/// Initialize the Aegis runtime
pub async fn initialize() -> anyhow::Result<()> {
    observability::init_tracing()?;
    tracing::info!("Aegis runtime initialized");
    Ok(())
}
