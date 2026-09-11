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

        // 3. Create sandbox for this request
        let mut sandbox = Sandbox::new_with_limits(SandboxConfig::default()).map_err(|e| {
            tracing::error!(error = %e, "failed to create sandbox");
            Status::internal(format!("failed to create sandbox: {}", e))
        })?;

        // 4. Set the shared receipt emitter on the sandbox
        sandbox.store_mut().data_mut().receipt_emitter =
            Some(self.receipt_emitter.clone());

        // 5. Find the matching capability by name
        let matching_cap = capabilities
            .iter()
            .find(|c| c.capability_name() == capability_name)
            .ok_or_else(|| {
                Status::failed_precondition(format!(
                    "capability '{}' not granted in config",
                    capability_name
                ))
            })?;

        // 6. Execute the capability in the sandbox
        let result = match capability_name.as_str() {
            "filesystem.read" => {
                self.execute_filesystem_read(&mut sandbox, matching_cap, &config_str)
                    .await
            }
            _ => Err(Status::unimplemented(format!(
                "capability '{}' not yet implemented",
                capability_name
            ))),
        };

        match result {
            Ok(output) => {
                // 7. Emit a success receipt for this execution
                {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter
                        .emit(&capability_name, "execute", "", output.len() as u64, "success")
                        .map_err(|e| Status::internal(format!("receipt emission failed: {}", e)))?;
                }

                // 8. Get the receipt from the emitter chain
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

                Err(status)
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
    async fn execute_filesystem_read(
        &self,
        _sandbox: &mut Sandbox,
        cap: &crate::capabilities::Capability,
        config_str: &str,
    ) -> Result<Vec<u8>, Status> {
        // Parse the config to get the path to read
        let _policy_config: crate::config::PolicyConfig = toml::from_str(config_str)
            .map_err(|e| Status::invalid_argument(format!("config parse error: {}", e)))?;

        // For now, we return a simple success indicator
        // In a full implementation, this would load a WASM module and execute it
        // with the filesystem.read capability registered via Linker
        let result = format!(
            "filesystem.read executed for capability: {}",
            cap.capability_name()
        );

        Ok(result.into_bytes())
    }
}
