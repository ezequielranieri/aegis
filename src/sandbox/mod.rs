//! Wasmtime-based sandboxing module

use anyhow::Result;
use wasmtime::*;

/// Secure WASM sandbox
pub struct Sandbox {
    engine: Engine,
    store: Store<()>,
}

impl Sandbox {
    pub fn new() -> Result<Self> {
        let engine = Engine::default();
        let store = Store::new(&engine, ());
        Ok(Self { engine, store })
    }

    pub async fn instantiate(&mut self, wasm_bytes: &[u8]) -> Result<Instance> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let instance = Instance::new(&mut self.store, &module, &[])?;
        Ok(instance)
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new().expect("Failed to create sandbox")
    }
}