//! Capability-based security via host functions

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Capability granted to a WASM module
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub params: HashMap<String, serde_json::Value>,
}

/// Capability provider trait
pub trait CapabilityProvider: Send + Sync {
    fn provide(&self, capability: &Capability) -> Result<serde_json::Value>;
}

/// Built-in capabilities
pub mod builtin {
    use super::*;
    
    pub const FILESYSTEM_READ: &str = "filesystem.read";
    pub const FILESYSTEM_WRITE: &str = "filesystem.write";
    pub const NETWORK_HTTP: &str = "network.http";
    pub const CRYPTO_SIGN: &str = "crypto.sign";
    pub const CRYPTO_VERIFY: &str = "crypto.verify";
}