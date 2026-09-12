use std::path::PathBuf;

/// Runtime configuration for `aegis-runtime` gRPC server (REQ-809, REQ-810).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeConfig {
    pub server: ServerConfig,
    pub receipts: ReceiptsConfig,
    pub execution: ExecutionConfig,
}

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
pub struct ReceiptsConfig {
    /// Ed25519 key file path (0600 perms enforced).
    pub key_path: PathBuf,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExecutionConfig {
    /// Max concurrent Execute RPCs (semaphore limit).
    pub max_concurrent: usize,
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
}
