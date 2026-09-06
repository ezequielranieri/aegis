//! OpenTelemetry observability

use anyhow::Result;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{self, TracerProvider};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialize tracing and OpenTelemetry
pub fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,aegis=debug"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    let tracer_provider = init_tracer_provider()?;
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer("aegis"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    Ok(())
}

/// Initialize OpenTelemetry tracer provider
fn init_tracer_provider() -> Result<TracerProvider> {
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint("http://localhost:4317");

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::new(vec![
                KeyValue::new("service.name", "aegis"),
                KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            ])
        )
        .build();

    global::set_tracer_provider(provider.clone());
    Ok(provider)
}

/// Shutdown tracer provider
pub fn shutdown_tracing() {
    opentelemetry::global::shutdown_tracer_provider();
}