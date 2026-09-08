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
pub mod observability;
pub mod policy;
pub mod receipts;
pub mod sandbox;

pub use config::{CapabilityDef, ConfigError, PolicyConfig};
pub use sandbox::{CapabilityConfig, EpochInterrupter, Sandbox, SandboxConfig};

/// Initialize the Aegis runtime
pub async fn initialize() -> anyhow::Result<()> {
    observability::init_tracing()?;
    tracing::info!("Aegis runtime initialized");
    Ok(())
}
