//! Capability-based security via host functions

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Capability granted to a WASM module (typed enum — REQ-201, REQ-202)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "name")]
pub enum Capability {
    #[serde(rename = "filesystem.read")]
    FilesystemRead(FilesystemReadParams),
    #[serde(rename = "filesystem.write")]
    FilesystemWrite(FilesystemWriteParams),
    #[serde(rename = "network.http")]
    NetworkHttp(NetworkHttpParams),
}

/// Parameters for filesystem.read capability (REQ-202)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemReadParams {
    pub allowed_root: PathBuf,
    pub max_read_bytes: u64,
}

/// Parameters for filesystem.write capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemWriteParams {
    pub allowed_root: PathBuf,
    pub max_write_bytes: u64,
}

/// Parameters for network.http capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkHttpParams {
    pub allowed_hosts: Vec<String>,
    #[serde(default = "default_allowed_methods")]
    pub allowed_methods: Vec<String>,
    pub max_requests_per_second: u64,
}

/// Serde default for `allowed_methods` — GET-only unless configured (REQ-601).
pub fn default_allowed_methods() -> Vec<String> {
    vec!["GET".to_string()]
}

impl Capability {
    /// Returns the capability variant name as a static string (REQ-203, SA-201).
    pub fn capability_name(&self) -> &'static str {
        match self {
            Capability::FilesystemRead(_) => "filesystem.read",
            Capability::FilesystemWrite(_) => "filesystem.write",
            Capability::NetworkHttp(_) => "network.http",
        }
    }
}

/// Capability provider trait
pub trait CapabilityProvider: Send + Sync {
    fn provide(&self, capability: &Capability) -> Result<serde_json::Value>;
}

/// Built-in capabilities
pub mod builtin {
    pub const FILESYSTEM_READ: &str = "filesystem.read";
    pub const FILESYSTEM_WRITE: &str = "filesystem.write";
    pub const NETWORK_HTTP: &str = "network.http";
    pub const CRYPTO_SIGN: &str = "crypto.sign";
    pub const CRYPTO_VERIFY: &str = "crypto.verify";
}
