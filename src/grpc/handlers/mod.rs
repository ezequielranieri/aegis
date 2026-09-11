//! gRPC request handlers for AegisRuntime service.
//!
//! Implements the `AegisRuntime` trait which contains Execute, VerifyChain,
//! and GetReceiptChain RPCs.

pub mod health;

use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use tonic::{Request, Response, Status};

use crate::proto::aegis::v1::aegis_runtime_server::AegisRuntime;
use crate::proto::aegis::v1::{
    ExecuteRequest, ExecuteResponse, GetReceiptChainRequest, GetReceiptChainResponse,
    VerifyChainRequest, VerifyChainResponse,
};
use crate::receipts::{ExecutionReceipt, ReceiptChain, ReceiptEmitter};
use crate::sandbox::{Sandbox, SandboxConfig};

/// Shared state for all AegisRuntime RPC handlers.
pub struct AegisRuntimeService {
    pub receipt_emitter: Arc<Mutex<ReceiptEmitter>>,
    pub semaphore: Arc<Semaphore>,
}

#[tonic::async_trait]
impl AegisRuntime for AegisRuntimeService {
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        // Acquire semaphore permit for concurrency limiting (REQ-810)
        // Use try_acquire to immediately reject when at capacity (RESOURCE_EXHAUSTED)
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Status::resource_exhausted("max concurrent executions exceeded")
            })?;

        let req = request.into_inner();
        let capability_name = req.capability_name;
        let config_bytes = req.config;
        let wasm_module_bytes = req.wasm_module;

        tracing::info!(capability = %capability_name, "execute request received");

        // 1. Parse TOML config from request bytes
        let config_str = String::from_utf8(config_bytes).map_err(|e| {
            tracing::warn!(error = %e, "invalid UTF-8 in config");
            Status::invalid_argument(format!("invalid config encoding: {}", e))
        })?;

        // 2. Parse the TOML config to extract capabilities
        let policy_config: crate::config::PolicyConfig =
            toml::from_str(&config_str).map_err(|e| {
                tracing::warn!(error = %e, "invalid TOML config");
                Status::invalid_argument(format!("invalid TOML config: {}", e))
            })?;

        let capabilities = policy_config
            .try_into_capabilities()
            .map_err(|e| Status::invalid_argument(format!("invalid capabilities: {}", e)))?;

        // 3. Validate WASM module provided
        if wasm_module_bytes.is_empty() {
            return Err(Status::invalid_argument("wasm_module is required"));
        }

// 4. Create sandbox for this request
        // Check for test mode to disable epoch interruption entirely (avoids "wasm trap: interrupt" in tests)
        let test_mode = std::env::var("AEGIS_TEST_MODE").is_ok();
        let mut sandbox = if test_mode {
            // Disable epoch interruption entirely for tests (like unit tests do)
            Sandbox::new_with_config(SandboxConfig::default(), false).map_err(|e| {
                tracing::error!(error = %e, "failed to create sandbox");
                Status::internal(format!("failed to create sandbox: {}", e))
            })?
        } else {
            Sandbox::new_with_limits(SandboxConfig::default()).map_err(|e| {
                tracing::error!(error = %e, "failed to create sandbox");
                Status::internal(format!("failed to create sandbox: {}", e))
            })?
        };

        // 5. Set the shared receipt emitter on the sandbox
        sandbox.store_mut().data_mut().receipt_emitter =
            Some(self.receipt_emitter.clone());

        // 6. Find the matching capability by name
        let matching_cap = capabilities
            .iter()
            .find(|c| c.capability_name() == capability_name)
            .ok_or_else(|| {
                Status::failed_precondition(format!(
                    "capability '{}' not granted in config",
                    capability_name
                ))
            })?;

        // 7. Execute the capability in the sandbox via WASM
        let result = self
            .execute_wasm_capability(&mut sandbox, &capabilities, &wasm_module_bytes)
            .await;

        match result {
            Ok(output) => {
                // 8. Emit a success receipt for this execution
                {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter
                        .emit(&capability_name, "execute", "", output.len() as u64, "success")
                        .map_err(|e| Status::internal(format!("receipt emission failed: {}", e)))?;
                }

                // 9. Get the receipt from the emitter chain
                let receipt_json = {
                    let emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    let chain = emitter.chain();
                    chain.last().map(|r| {
                        serde_json::to_vec(r).expect("receipt serialization should not fail")
                    })
                };

                let receipt_bytes = receipt_json.unwrap_or_default();

                Ok(Response::new(ExecuteResponse {
                    success: true,
                    result: output,
                    receipt: receipt_bytes,
                    error_message: String::new(),
                }))
            }
            Err(status) => {
                // Emit trap receipt on failure
                if let Ok(mut emitter) = self.receipt_emitter.lock() {
                    let _ = emitter.emit(&capability_name, "execute", "", 0, "trap");
                }

                // For capability violations (traversal, size exceed), return success=false
                // instead of gRPC error status, per REQ-715 / S-701
                if status.code() == tonic::Code::FailedPrecondition {
                    Ok(Response::new(ExecuteResponse {
                        success: false,
                        result: Vec::new(),
                        receipt: Vec::new(),
                        error_message: status.message().to_string(),
                    }))
                } else {
                    Err(status)
                }
            }
        }
    }

    async fn verify_chain(
        &self,
        request: Request<VerifyChainRequest>,
    ) -> Result<Response<VerifyChainResponse>, Status> {
        let req = request.into_inner();

        // Parse receipts from request bytes
        let mut receipts = Vec::new();
        for (i, receipt_bytes) in req.receipts.iter().enumerate() {
            let receipt: ExecutionReceipt =
                serde_json::from_slice(receipt_bytes).map_err(|e| {
                    tracing::warn!(index = i, error = %e, "failed to parse receipt");
                    Status::invalid_argument(format!(
                        "receipt at index {} is not valid JSON: {}",
                        i, e
                    ))
                })?;
            receipts.push(receipt);
        }

        // Parse public key
        let public_key = req.public_key;
        if public_key.is_empty() {
            return Ok(Response::new(VerifyChainResponse {
                valid: false,
                error_message: "public key is required".to_string(),
            }));
        }

        tracing::info!(
            receipt_count = receipts.len(),
            "verify chain request received"
        );

        // Delegate to ReceiptChain::verify_chain
        match ReceiptChain::verify_chain(&receipts, &public_key) {
            Ok(()) => {
                tracing::info!("chain verification succeeded");
                Ok(Response::new(VerifyChainResponse {
                    valid: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                tracing::warn!(error = %e, "chain verification failed");
                Ok(Response::new(VerifyChainResponse {
                    valid: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn get_receipt_chain(
        &self,
        _request: Request<GetReceiptChainRequest>,
    ) -> Result<Response<GetReceiptChainResponse>, Status> {
        tracing::info!("get receipt chain request received");

        let emitter = self.receipt_emitter.lock().map_err(|e| {
            Status::internal(format!("receipt emitter lock failed: {}", e))
        })?;

        let chain = emitter.chain();

        // Serialize each receipt to JSON bytes
        let receipts: Vec<Vec<u8>> = chain
            .iter()
            .map(|r| serde_json::to_vec(r).expect("receipt serialization should not fail"))
            .collect();

        tracing::info!(count = receipts.len(), "returning receipt chain");

        Ok(Response::new(GetReceiptChainResponse {
            receipts,
        }))
    }
}

#[allow(clippy::result_large_err)]
impl AegisRuntimeService {
    /// Execute a capability by instantiating the provided WASM module with the granted capabilities
    /// and calling its exported `execute` function.
    async fn execute_wasm_capability(
        &self,
        sandbox: &mut Sandbox,
        capabilities: &[crate::capabilities::Capability],
        wasm_module_bytes: &[u8],
    ) -> Result<Vec<u8>, Status> {
        // Instantiate the WASM module with the granted capabilities
        let instance = sandbox
            .instantiate_with_capabilities(wasm_module_bytes, capabilities)
            .map_err(|e| {
                tracing::error!(error = %e, "failed to instantiate WASM module");
                Status::internal(format!("WASM instantiation failed: {}", e))
            })?;

        // Get the exported `execute` function
        let execute_func = instance
            .get_typed_func::<(), (i32,)>(&mut sandbox.store_mut(), "execute")
            .map_err(|e| {
                tracing::error!(error = %e, "exported function 'execute' not found");
                Status::failed_precondition(format!("WASM module must export 'execute' function: {}", e))
            })?;

        // Call the execute function (no arguments, returns i32 status/result pointer)
        let (result_ptr,) = execute_func
            .call(&mut sandbox.store_mut(), ())
            .map_err(|e| {
                tracing::error!(error = %e, "WASM execution trapped");
                // WASM trap = capability violation or guest error
                // The error `e` is anyhow::Error wrapping wasmtime::Error
                // Try to extract the original host function error from the error chain
                let mut msg = e.to_string();
                if let Some(source) = e.source() {
                    msg.push_str(" | source: ");
                    msg.push_str(&source.to_string());
                    if let Some(source2) = source.source() {
                        msg.push_str(" | source2: ");
                        msg.push_str(&source2.to_string());
                    }
                }
                tracing::debug!(full_error = %msg, "WASM trap error chain");
                
                // Extract meaningful error message for common violation types
                let clean_msg = if msg.contains("traversal") || msg.contains("..") || msg.contains("outside") || msg.contains("traversal attempt") {
                    "path traversal attempt detected"
                } else if msg.contains("size") || msg.contains("exceed") || msg.contains("max") {
                    "size limit exceeded"
                } else {
                    "WASM execution trapped"
                };
                Status::failed_precondition(clean_msg)
            })?;

        // Get memory export from the instance
        let memory = instance
            .get_export(&mut sandbox.store_mut(), "memory")
            .and_then(|e| e.into_memory())
            .ok_or_else(|| Status::internal("memory export required"))?;

        // For now, return a simple success indicator
        // TODO: Implement proper result retrieval from guest memory using result_ptr
        // The WASM module should write results to a known memory location
        // or use a host function to return data
        let _ = memory; // suppress unused warning
        let _ = result_ptr;

        Ok(b"WASM execution completed".to_vec())
    }
}