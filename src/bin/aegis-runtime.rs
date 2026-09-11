use aegis::config::runtime::RuntimeConfig;
use aegis::grpc::server;
use aegis::observability;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Aegis Runtime — gRPC server for capability-based WASM execution
#[derive(Parser)]
#[command(name = "aegis-runtime", about = "Aegis gRPC runtime server")]
struct Cli {
    /// Path to TOML runtime configuration file
    #[arg(short, long)]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing before anything else (REQ-808)
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,aegis=debug"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    tracing::info!("aegis-runtime starting");

    // Parse CLI arguments
    let cli = Cli::parse();

    // Load and validate config — fail-closed on error (REQ-803)
    let config_content = std::fs::read_to_string(&cli.config)
        .map_err(|e| anyhow::anyhow!("failed to read config {}: {}", cli.config, e))?;
    let config: RuntimeConfig = toml::from_str(&config_content)
        .map_err(|e| anyhow::anyhow!("failed to parse config: {}", e))?;

    tracing::info!(config = %cli.config, "config loaded successfully");

    // Start the gRPC server (fail-closed on any error, REQ-803)
    server::start_server(&config).await?;

    // Shutdown tracing
    observability::shutdown_tracing();

    tracing::info!("aegis-runtime shutdown complete");
    Ok(())
}
