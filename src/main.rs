//! Aegis - Secure WASM Runtime Entry Point

use aegis::initialize;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    initialize().await?;

    // TODO: Implement CLI and runtime logic
    tracing::info!("Aegis runtime starting...");

    Ok(())
}
