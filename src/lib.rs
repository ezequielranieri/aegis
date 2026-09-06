//! Aegis - Secure WASM Runtime with Capability-Based Security
//!
//! Features:
//! - Wasmtime-based sandboxing
//! - Capability-based security via host functions
//! - Signed execution receipts with hash-chaining
//! - Declarative policy engine
//! - OpenTelemetry observability

pub mod sandbox;
pub mod capabilities;
pub mod receipts;
pub mod policy;
pub mod observability;

/// Initialize the Aegis runtime
pub async fn initialize() -> anyhow::Result<()> {
    observability::init_tracing()?;
    tracing::info!("Aegis runtime initialized");
    Ok(())
}