//! Declarative policy configuration from TOML files
//!
//! Provides `PolicyConfig` for loading and validating TOML config files
//! with capability definitions. Fail-closed: no default capabilities when
//! config is missing or invalid.

pub mod runtime;

use std::path::PathBuf;

use crate::capabilities::{default_allowed_methods, Capability, FilesystemReadParams};
use serde::de::Error as SerdeError;

/// Configuration parsed from a TOML config file (REQ-302)
#[derive(Debug, serde::Deserialize)]
pub struct PolicyConfig {
    pub capabilities: Vec<CapabilityDef>,
    #[serde(default)]
    pub receipts: Option<ReceiptsConfig>,
}

/// Receipt signing configuration (REQ-421)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReceiptsConfig {
    pub key_path: PathBuf,
}

/// Capability definition from TOML — uses `serde(tag = "name")` for
/// internally-tagged enum deserialization (REQ-302, REQ-308).
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "name")]
pub enum CapabilityDef {
    #[serde(rename = "filesystem.read")]
    FilesystemRead {
        allowed_root: String,
        max_read_bytes: u64,
    },
    #[serde(rename = "filesystem.write")]
    FilesystemWrite {
        allowed_root: String,
        max_write_bytes: u64,
    },
    #[serde(rename = "network.http")]
    NetworkHttp {
        allowed_hosts: Vec<String>,
        #[serde(default = "default_allowed_methods")]
        allowed_methods: Vec<String>,
        max_requests_per_second: u64,
    },
}

/// Errors during config loading and validation (REQ-308)
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Config file not found: {0}")]
    NotFound(String),

    #[error("Failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Missing required field '{field}' for capability '{capability}'")]
    MissingField { capability: String, field: String },

    #[error("Unknown capability type '{name}'. Must be one of: {allowed:?}")]
    UnknownCapability { name: String, allowed: Vec<String> },

    #[error("Duplicate capability '{name}' in config")]
    DuplicateCapability { name: String },

    #[error("Invalid path '{path}' for capability '{capability}'")]
    InvalidPath { capability: String, path: String },

    #[error("Invalid host '{host}' for capability '{capability}'")]
    InvalidHost { capability: String, host: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl PolicyConfig {
    /// Load and validate a policy config from the given path (REQ-303).
    ///
    /// Tries `path` first, then falls back to `~/.config/aegis/config.toml`.
    /// Returns `ConfigError::NotFound` if neither exists — fail-closed.
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        // 1. Try the provided path first
        let content = match std::fs::read_to_string(path) {
            Ok(c) => Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 2. Try fallback path
                let fallback = default_config_path();
                std::fs::read_to_string(&fallback)
                    .map_err(|_| ConfigError::NotFound(path.to_string()))
            }
            Err(e) => Err(ConfigError::Io(e)),
        }?;

        // 3. Pre-validate capability names before typed deserialization (REQ-306).
        // This ensures UnknownCapability is reachable for better error messages.
        Self::validate_capability_names(&content)?;

        // 4. Parse TOML to typed config
        let config: PolicyConfig = toml::from_str(&content)?;

        // 5. Validate required fields, duplicates, and paths
        config.validate()?;

        Ok(config)
    }

    /// Validate all capability names in the TOML are known (REQ-306).
    fn validate_capability_names(content: &str) -> Result<(), ConfigError> {
        // Parse as generic TOML value to extract capability names
        let raw: toml::Value = toml::from_str(content)?;

        // Extract capabilities array
        let capabilities = match raw.get("capabilities") {
            Some(toml::Value::Array(arr)) => arr,
            Some(_) => {
                return Err(ConfigError::ParseError(SerdeError::custom(
                    "'capabilities' must be an array",
                )));
            }
            None => return Ok(()), // no capabilities defined
        };

        let allowed = Self::allowed_capability_names();
        for (idx, cap) in capabilities.iter().enumerate() {
            // Try to extract name field
            let name = match cap.get("name") {
                Some(toml::Value::String(s)) => s.clone(),
                Some(_) => {
                    return Err(ConfigError::ParseError(SerdeError::custom(format!(
                        "capability at index {}: 'name' must be a string",
                        idx
                    ))));
                }
                None => {
                    return Err(ConfigError::ParseError(SerdeError::custom(format!(
                        "capability at index {}: missing required 'name' field",
                        idx
                    ))));
                }
            };

            // Check against allowed capabilities
            if !allowed.contains(&name) {
                return Err(ConfigError::UnknownCapability {
                    name: name.clone(),
                    allowed: allowed.clone(),
                });
            }
        }

        Ok(())
    }

    /// List of all allowed capability names
    fn allowed_capability_names() -> Vec<String> {
        vec![
            "filesystem.read".to_string(),
            "filesystem.write".to_string(),
            "network.http".to_string(),
        ]
    }

    /// Validate required fields per capability type and check for duplicates (REQ-305, REQ-309, REQ-310).
    fn validate(&self) -> Result<(), ConfigError> {
        // Check duplicates
        let mut seen = std::collections::HashSet::new();
        for cap in &self.capabilities {
            let name = cap.name();
            if !seen.insert(name.to_string()) {
                return Err(ConfigError::DuplicateCapability {
                    name: name.to_string(),
                });
            }
        }

        // Validate required fields and paths per variant
        for cap in &self.capabilities {
            match cap {
                CapabilityDef::FilesystemRead {
                    allowed_root,
                    max_read_bytes,
                } => {
                    if allowed_root.is_empty() {
                        return Err(ConfigError::MissingField {
                            capability: "filesystem.read".to_string(),
                            field: "allowed_root".to_string(),
                        });
                    }
                    if *max_read_bytes == 0 {
                        return Err(ConfigError::MissingField {
                            capability: "filesystem.read".to_string(),
                            field: "max_read_bytes (> 0)".to_string(),
                        });
                    }
                    // Validate path is absolute
                    let root = PathBuf::from(allowed_root);
                    if !root.is_absolute() {
                        return Err(ConfigError::InvalidPath {
                            capability: "filesystem.read".to_string(),
                            path: allowed_root.clone(),
                        });
                    }
                    // Verify canonicalizable
                    std::fs::canonicalize(&root).map_err(|_| ConfigError::InvalidPath {
                        capability: "filesystem.read".to_string(),
                        path: allowed_root.clone(),
                    })?;
                }
                CapabilityDef::FilesystemWrite {
                    allowed_root,
                    max_write_bytes,
                } => {
                    if allowed_root.is_empty() {
                        return Err(ConfigError::MissingField {
                            capability: "filesystem.write".to_string(),
                            field: "allowed_root".to_string(),
                        });
                    }
                    if *max_write_bytes == 0 {
                        return Err(ConfigError::MissingField {
                            capability: "filesystem.write".to_string(),
                            field: "max_write_bytes (> 0)".to_string(),
                        });
                    }
                    let root = PathBuf::from(allowed_root);
                    if !root.is_absolute() {
                        return Err(ConfigError::InvalidPath {
                            capability: "filesystem.write".to_string(),
                            path: allowed_root.clone(),
                        });
                    }
                    std::fs::canonicalize(&root).map_err(|_| ConfigError::InvalidPath {
                        capability: "filesystem.write".to_string(),
                        path: allowed_root.clone(),
                    })?;
                }
                CapabilityDef::NetworkHttp {
                    allowed_hosts,
                    allowed_methods,
                    max_requests_per_second,
                } => {
                    if allowed_hosts.is_empty() {
                        return Err(ConfigError::MissingField {
                            capability: "network.http".to_string(),
                            field: "allowed_hosts".to_string(),
                        });
                    }
                    // Fail-closed: exact hostname only — no IP literals, no wildcards (REQ-602).
                    for host in allowed_hosts {
                        if is_invalid_host(host) {
                            return Err(ConfigError::InvalidHost {
                                capability: "network.http".to_string(),
                                host: host.clone(),
                            });
                        }
                    }
                    if allowed_methods.is_empty() {
                        return Err(ConfigError::MissingField {
                            capability: "network.http".to_string(),
                            field: "allowed_methods".to_string(),
                        });
                    }
                    if *max_requests_per_second == 0 {
                        return Err(ConfigError::MissingField {
                            capability: "network.http".to_string(),
                            field: "max_requests_per_second (> 0)".to_string(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Convert all capability definitions to typed `Capability` enums (REQ-306).
    pub fn try_into_capabilities(self) -> Result<Vec<Capability>, ConfigError> {
        self.capabilities
            .into_iter()
            .map(|c| c.into_capability())
            .collect()
    }
}

impl CapabilityDef {
    fn name(&self) -> &'static str {
        match self {
            CapabilityDef::FilesystemRead { .. } => "filesystem.read",
            CapabilityDef::FilesystemWrite { .. } => "filesystem.write",
            CapabilityDef::NetworkHttp { .. } => "network.http",
        }
    }

    /// Convert a capability definition to a typed `Capability` enum (REQ-306).
    pub fn into_capability(self) -> Result<Capability, ConfigError> {
        match self {
            CapabilityDef::FilesystemRead {
                allowed_root,
                max_read_bytes,
            } => {
                let root = PathBuf::from(&allowed_root);
                // Re-validate: absolute and canonicalizable
                if !root.is_absolute() {
                    return Err(ConfigError::InvalidPath {
                        capability: "filesystem.read".to_string(),
                        path: allowed_root,
                    });
                }
                let canonical =
                    std::fs::canonicalize(&root).map_err(|_| ConfigError::InvalidPath {
                        capability: "filesystem.read".to_string(),
                        path: allowed_root,
                    })?;
                Ok(Capability::FilesystemRead(FilesystemReadParams {
                    allowed_root: canonical,
                    max_read_bytes,
                }))
            }
            CapabilityDef::FilesystemWrite {
                allowed_root,
                max_write_bytes,
            } => {
                let root = PathBuf::from(&allowed_root);
                if !root.is_absolute() {
                    return Err(ConfigError::InvalidPath {
                        capability: "filesystem.write".to_string(),
                        path: allowed_root,
                    });
                }
                let canonical =
                    std::fs::canonicalize(&root).map_err(|_| ConfigError::InvalidPath {
                        capability: "filesystem.write".to_string(),
                        path: allowed_root,
                    })?;
                Ok(Capability::FilesystemWrite(
                    crate::capabilities::FilesystemWriteParams {
                        allowed_root: canonical,
                        max_write_bytes,
                    },
                ))
            }
            CapabilityDef::NetworkHttp {
                allowed_hosts,
                allowed_methods,
                max_requests_per_second,
            } => Ok(Capability::NetworkHttp(
                crate::capabilities::NetworkHttpParams {
                    allowed_hosts: allowed_hosts
                        .into_iter()
                        .map(|h| h.to_lowercase())
                        .collect(),
                    allowed_methods: allowed_methods
                        .into_iter()
                        .map(|m| m.to_uppercase())
                        .collect(),
                    max_requests_per_second,
                },
            )),
        }
    }
}

/// Fail-closed host check (REQ-602): `allowed_hosts` entries must be DNS
/// hostnames matched exactly — IP literals (IPv4/IPv6, bare or bracketed) and
/// wildcard entries are rejected.
fn is_invalid_host(host: &str) -> bool {
    // No wildcards — exact hostname matching only
    if host.contains('*') {
        return true;
    }
    // Bare IPv4/IPv6 literals, e.g. "1.2.3.4", "::1"
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    // Bracketed IPv6 literal from URL notation, e.g. "[::1]"
    if host.starts_with('[') && host.ends_with(']') {
        if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            return inner.parse::<std::net::IpAddr>().is_ok();
        }
    }
    false
}

/// Resolve the fallback config path: `~/.config/aegis/config.toml`
fn default_config_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/.config/aegis/config.toml", home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One network.http capability with the given allowed_hosts.
    fn policy_with_hosts(hosts: Vec<&str>) -> PolicyConfig {
        PolicyConfig {
            capabilities: vec![CapabilityDef::NetworkHttp {
                allowed_hosts: hosts.into_iter().map(str::to_string).collect(),
                allowed_methods: vec!["GET".to_string()],
                max_requests_per_second: 10,
            }],
            receipts: None,
        }
    }

    /// One network.http capability with the given allowed_methods.
    fn policy_with_methods(methods: Vec<&str>) -> PolicyConfig {
        PolicyConfig {
            capabilities: vec![CapabilityDef::NetworkHttp {
                allowed_hosts: vec!["example.com".to_string()],
                allowed_methods: methods.into_iter().map(str::to_string).collect(),
                max_requests_per_second: 10,
            }],
            receipts: None,
        }
    }

    /// One network.http capability with the given rate.
    fn policy_with_rate(rate: u64) -> PolicyConfig {
        PolicyConfig {
            capabilities: vec![CapabilityDef::NetworkHttp {
                allowed_hosts: vec!["example.com".to_string()],
                allowed_methods: vec!["GET".to_string()],
                max_requests_per_second: rate,
            }],
            receipts: None,
        }
    }

    // ═══ E-601: validation rejects empty / non-hostname allowlist entries ═══

    #[test]
    fn network_http_empty_allowed_hosts_rejected_e601() {
        let err = policy_with_hosts(vec![])
            .validate()
            .expect_err("empty allowed_hosts must be rejected (E-601)");
        assert!(
            matches!(err, ConfigError::MissingField { ref capability, ref field }
                if capability == "network.http" && field == "allowed_hosts"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_empty_allowed_methods_rejected_e601() {
        let err = policy_with_methods(vec![])
            .validate()
            .expect_err("empty allowed_methods must be rejected (E-601)");
        assert!(
            matches!(err, ConfigError::MissingField { ref capability, ref field }
                if capability == "network.http" && field == "allowed_methods"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_zero_rate_rejected_e601() {
        let err = policy_with_rate(0)
            .validate()
            .expect_err("zero rate must be rejected (E-601)");
        assert!(
            matches!(err, ConfigError::MissingField { ref capability, ref field }
                if capability == "network.http" && field == "max_requests_per_second (> 0)"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_ipv4_literal_rejected_e601() {
        let err = policy_with_hosts(vec!["127.0.0.1"])
            .validate()
            .expect_err("IPv4 literal must be rejected (E-601, REQ-602)");
        assert!(
            matches!(err, ConfigError::InvalidHost { ref capability, ref host }
                if capability == "network.http" && host == "127.0.0.1"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_ipv6_literal_rejected_e601() {
        let err = policy_with_hosts(vec!["::1"])
            .validate()
            .expect_err("IPv6 literal must be rejected (E-601, REQ-602)");
        assert!(
            matches!(err, ConfigError::InvalidHost { ref capability, ref host }
                if capability == "network.http" && host == "::1"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_bracketed_ipv6_rejected_e601() {
        let err = policy_with_hosts(vec!["[::1]"])
            .validate()
            .expect_err("bracketed IPv6 literal must be rejected (E-601, REQ-602)");
        assert!(
            matches!(err, ConfigError::InvalidHost { ref capability, ref host }
                if capability == "network.http" && host == "[::1]"),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn network_http_wildcard_host_rejected_e601() {
        let err = policy_with_hosts(vec!["*.example.com"])
            .validate()
            .expect_err("wildcard host must be rejected (E-601, REQ-602)");
        assert!(
            matches!(err, ConfigError::InvalidHost { ref capability, ref host }
                if capability == "network.http" && host == "*.example.com"),
            "got: {:?}",
            err
        );
    }

    // ═══ Normalization: hosts → lowercase, methods → uppercase (task 1.5) ═══

    #[test]
    fn network_http_normalizes_hosts_and_methods() {
        let cfg = PolicyConfig {
            capabilities: vec![CapabilityDef::NetworkHttp {
                allowed_hosts: vec!["EXAMPLE.com".to_string(), "Sub.Example.Org".to_string()],
                allowed_methods: vec!["get".to_string(), "POST".to_string()],
                max_requests_per_second: 10,
            }],
            receipts: None,
        }
        .try_into_capabilities()
        .expect("valid network.http must convert");

        match &cfg[0] {
            Capability::NetworkHttp(params) => {
                assert_eq!(params.allowed_hosts, vec!["example.com", "sub.example.org"]);
                assert_eq!(params.allowed_methods, vec!["GET", "POST"]);
            }
            other => panic!("expected NetworkHttp, got: {:?}", other),
        }
    }

    // ═══ REQ-601: `allowed_methods` defaults to ["GET"] when absent ═══

    #[test]
    fn network_http_methods_default_to_get_in_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("net_http_default_methods.toml");
        std::fs::write(
            &path,
            r#"
[[capabilities]]
name = "network.http"
allowed_hosts = ["example.com"]
max_requests_per_second = 10
"#,
        )
        .expect("write config");

        let caps = PolicyConfig::load(path.to_str().expect("path"))
            .expect("defaults must load")
            .try_into_capabilities()
            .expect("convert");
        match &caps[0] {
            Capability::NetworkHttp(params) => {
                assert_eq!(params.allowed_methods, vec!["GET"], "REQ-601 default");
            }
            other => panic!("expected NetworkHttp, got: {:?}", other),
        }
    }
}
