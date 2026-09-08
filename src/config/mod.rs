//! Declarative policy configuration from TOML files
//!
//! Provides `PolicyConfig` for loading and validating TOML config files
//! with capability definitions. Fail-closed: no default capabilities when
//! config is missing or invalid.

use std::path::PathBuf;

use crate::capabilities::{Capability, FilesystemReadParams};
use serde::de::Error as SerdeError;

/// Configuration parsed from a TOML config file (REQ-302)
#[derive(Debug, serde::Deserialize)]
pub struct PolicyConfig {
    pub capabilities: Vec<CapabilityDef>,
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
                    max_requests_per_second,
                } => {
                    if allowed_hosts.is_empty() {
                        return Err(ConfigError::MissingField {
                            capability: "network.http".to_string(),
                            field: "allowed_hosts".to_string(),
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
            .map(|c| c.try_into())
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
    fn try_into(self) -> Result<Capability, ConfigError> {
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
                max_requests_per_second,
            } => Ok(Capability::NetworkHttp(
                crate::capabilities::NetworkHttpParams {
                    allowed_hosts,
                    max_requests_per_second,
                },
            )),
        }
    }
}

/// Resolve the fallback config path: `~/.config/aegis/config.toml`
fn default_config_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/.config/aegis/config.toml", home)
}
