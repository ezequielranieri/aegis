use std::path::PathBuf;

/// Runtime configuration for `aegis-runtime` gRPC server (REQ-809, REQ-810).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeConfig {
    pub server: ServerConfig,
    pub receipts: crate::config::ReceiptsConfig,
    pub execution: ExecutionConfig,
}

pub use crate::config::ReceiptsConfig;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub ca_cert_path: PathBuf,
    /// Expected CN/SAN of client cert (e.g., "agent-gateway").
    pub expected_identity: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExecutionConfig {
    /// Max concurrent Execute RPCs (semaphore limit).
    pub max_concurrent: usize,
    /// Optional per-execution fuel budget. When absent, a generous default is applied.
    #[serde(default)]
    pub fuel_budget: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runtime_config() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 50051

[server.tls]
cert_path = "/etc/aegis/server.crt"
key_path = "/etc/aegis/server.key"
ca_cert_path = "/etc/aegis/ca.crt"
expected_identity = "agent-gateway"

[receipts]
key_path = "/etc/aegis/signing_key.toml"

[execution]
max_concurrent = 8
"#;
        let config: RuntimeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 50051);
        let tls = config.server.tls.as_ref().unwrap();
        assert_eq!(tls.cert_path, PathBuf::from("/etc/aegis/server.crt"));
        assert_eq!(tls.expected_identity, "agent-gateway");
        assert_eq!(
            config.receipts.key_path,
            PathBuf::from("/etc/aegis/signing_key.toml")
        );
        assert_eq!(config.execution.max_concurrent, 8);
    }

    #[test]
    fn parse_config_without_tls() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 50051

[receipts]
key_path = "/tmp/key.toml"

[execution]
max_concurrent = 4
"#;
        let config: RuntimeConfig = toml::from_str(toml_str).unwrap();
        assert!(config.server.tls.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // Phase 8: Fuel Budget Config Tests (WU2)
    // ═══════════════════════════════════════════════════════════════════════════════

    /// E-804: Budget absent — config without fuel_budget parses unchanged,
    /// default constant applied at runtime (not in config).
    #[test]
    fn e804_fuel_budget_absent_parses_unchanged() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 50051

[receipts]
key_path = "/tmp/key.toml"

[execution]
max_concurrent = 4
"#;
        let config: RuntimeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.execution.max_concurrent, 4);
        assert!(
            config.execution.fuel_budget.is_none(),
            "fuel_budget should be None when absent"
        );
    }

    /// E-805: Budget explicit — fuel_budget = n parsed and honored.
    #[test]
    fn e805_fuel_budget_explicit_honored() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 50051

[receipts]
key_path = "/tmp/key.toml"

[execution]
max_concurrent = 4
fuel_budget = 5000000
"#;
        let config: RuntimeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.execution.max_concurrent, 4);
        assert_eq!(config.execution.fuel_budget, Some(5_000_000));
    }
}
