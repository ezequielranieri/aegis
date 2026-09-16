//! gRPC request handlers for AegisRuntime service.
//!
//! Implements the `AegisRuntime` trait which contains Execute, VerifyChain,
//! and GetReceiptChain RPCs.

pub mod health;

use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use tonic::{Request, Response, Status};

use crate::config::PolicyConfig;
use crate::proto::aegis::v1::aegis_runtime_server::AegisRuntime;
use crate::proto::aegis::v1::{
    ExecuteAbortRequest, ExecuteAbortResponse, ExecuteCommitRequest, ExecuteCommitResponse,
    ExecutePrepareRequest, ExecutePrepareResponse, ExecuteRequest, ExecuteResponse,
    GetReceiptChainRequest, GetReceiptChainResponse, VerifyChainRequest, VerifyChainResponse,
};
use crate::receipts::{ExecutionReceipt, PrepareHandle, ReceiptChain, ReceiptEmitter};
use crate::sandbox::Sandbox;

/// Shared state for all AegisRuntime RPC handlers.
pub struct AegisRuntimeService {
    pub receipt_emitter: Arc<Mutex<ReceiptEmitter>>,
    pub semaphore: Arc<Semaphore>,
    pub fuel_budget: Option<u64>,
}

#[tonic::async_trait]
impl AegisRuntime for AegisRuntimeService {
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        // Legacy Execute RPC: internally uses Prepare+Commit two-phase flow.
        // On Commit signing failure -> Abort with error="commit_signing_failed" (AD-016).
        // Does NOT require TwoPhaseReceipts capability (legacy compatibility).

        // Acquire semaphore permit for concurrency limiting (REQ-810)
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("max concurrent executions exceeded"))?;

        let req = request.into_inner();
        let capability_name = req.capability_name;
        let config_bytes = req.config;
        let wasm_module_bytes = req.wasm_module;

        tracing::info!(capability = %capability_name, "execute (legacy wrapper) request received");

        // 1. Parse TOML config from request bytes
        let config_str = String::from_utf8(config_bytes).map_err(|e| {
            tracing::warn!(error = %e, "invalid UTF-8 in config");
            Status::invalid_argument(format!("invalid config encoding: {}", e))
        })?;

        // 2. Parse the TOML config to extract capabilities
        let policy_config: PolicyConfig = toml::from_str(&config_str).map_err(|e| {
            tracing::warn!(error = %e, "invalid TOML config");
            Status::invalid_argument(format!("invalid TOML config: {}", e))
        })?;

        let capabilities = policy_config
            .clone()
            .try_into_capabilities()
            .map_err(|e| Status::invalid_argument(format!("invalid capabilities: {}", e)))?;

        // 3. Validate WASM module provided
        if wasm_module_bytes.is_empty() {
            return Err(Status::invalid_argument("wasm_module is required"));
        }

        // 4. Find the matching capability by name (the actual capability to execute)
        // Legacy Execute: does NOT require TwoPhaseReceipts capability
        let _matching_cap = capabilities
            .iter()
            .find(|c| c.capability_name() == capability_name)
            .ok_or_else(|| {
                Status::failed_precondition(format!(
                    "capability '{}' not granted in config",
                    capability_name
                ))
            })?;

        // 5. Check if this is a TwoPhaseReceipts capability (should not be executable via legacy)
        if capability_name == "two_phase_receipts" {
            return Err(Status::failed_precondition(
                "two_phase_receipts is not an executable capability; use ExecutePrepare",
            ));
        }

        // 6. Phase 1: Prepare - create sandbox and sign prepare receipt
        let (_prepare_receipt, prepare_handle) = {
            let mut emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .prepare(
                    &capability_name,
                    policy_config.clone(),
                    &wasm_module_bytes,
                    self.fuel_budget,
                )
                .map_err(|e| Status::internal(format!("prepare failed: {}", e)))?
        };

        // 7. Get the sandbox handle from the pending entry
        let sandbox_handle = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .get_pending_sandbox(&prepare_handle)
                .ok_or_else(|| Status::internal("sandbox handle not found after prepare"))?
        };

        // 8. Get the config for receipt emission
        let config = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .get_pending_config(&prepare_handle)
                .ok_or_else(|| Status::internal("config not found after prepare"))?
        };

        // 9. Get the WASM module
        let wasm_module = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .get_pending_wasm_module(&prepare_handle)
                .ok_or_else(|| Status::internal("wasm module not found after prepare"))?
        };

        // 10. Execute WASM in the prepared sandbox
        let mut sandbox = sandbox_handle.lock().await;
        sandbox.store_mut().data_mut().receipt_emitter = Some(self.receipt_emitter.clone());

        // Test-only transport override injection
        #[cfg(feature = "test-utils")]
        if std::env::var("AEGIS_TEST_MODE").is_ok() {
            if let Ok(port_str) = std::env::var("AEGIS_TEST_NETWORK_PORT") {
                let state = sandbox.store_mut().data_mut();
                state.network_test_port = port_str.parse::<u16>().ok();
                if let Ok(ca_path) = std::env::var("AEGIS_TEST_CA_PEM") {
                    state.network_test_ca_pem = std::fs::read(&ca_path).ok();
                }
            }
        }

        // Execute the capability
        let result_bytes = self
            .execute_wasm_capability(
                &mut sandbox,
                &config
                    .clone()
                    .try_into_capabilities()
                    .map_err(|e| Status::internal(e.to_string()))?,
                &wasm_module,
            )
            .await;

        // Capture fuel consumed
        let fuel_consumed = sandbox.fuel_consumed();

        // 11. Handle result: success -> commit with hash, failure -> abort with trap
        match result_bytes {
            Ok(result_bytes) => {
                // Success: compute hash and commit
                let result_hash = blake3::hash(&result_bytes).to_hex().to_string();
                let cap = config
                    .try_into_capabilities()
                    .map_err(|e| Status::internal(e.to_string()))?
                    .into_iter()
                    .find(|c| c.capability_name() == capability_name)
                    .ok_or_else(|| Status::internal("capability not found in config"))?;

                let (capability_path, size) = match cap {
                    crate::capabilities::Capability::FilesystemRead(params) => (
                        params.allowed_root.to_string_lossy().to_string(),
                        result_bytes.len() as u64,
                    ),
                    crate::capabilities::Capability::FilesystemWrite(params) => (
                        params.allowed_root.to_string_lossy().to_string(),
                        result_bytes.len() as u64,
                    ),
                    crate::capabilities::Capability::NetworkHttp(_) => {
                        match sandbox.store_mut().data_mut().network_fetch.take() {
                            Some(fetch) => (fetch.url, fetch.body_len),
                            None => (String::new(), result_bytes.len() as u64),
                        }
                    }
                    crate::capabilities::Capability::TwoPhaseReceipts => {
                        return Err(Status::internal(
                            "two_phase_receipts should not be executable",
                        ));
                    }
                };

                // 12. Phase 2: Commit - sign commit receipt with BLAKE3 hash
                // Pass the BLAKE3 hash (result_hash) as the result for backward compatibility
                // with legacy execute receipt format (result field = BLAKE3 hash hex)
                let commit_result = {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter.commit(
                        prepare_handle,
                        result_hash.as_bytes(), // BLAKE3 hash hex string as bytes
                        &capability_path,
                        size,
                        fuel_consumed,
                    )
                };

                match commit_result {
                    Ok(commit_receipt) => {
                        // Success: return commit receipt
                        let receipt_bytes = serde_json::to_vec(&commit_receipt)
                            .expect("commit receipt serialization should not fail");

                        Ok(Response::new(ExecuteResponse {
                            success: true,
                            result: result_bytes,
                            receipt: receipt_bytes,
                            error_message: String::new(),
                        }))
                    }
                    Err(e)
                        if e.to_string().contains("signing")
                            || e.to_string().contains("receipt") =>
                    {
                        // Commit signing failure (AD-016): emit abort with error="commit_signing_failed"
                        tracing::warn!(error = %e, "commit signing failed, emitting abort with commit_signing_failed");

                        let abort_receipt = {
                            let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                                Status::internal(format!("receipt emitter lock failed: {}", e))
                            })?;
                            emitter
                                .abort(prepare_handle, Some("commit_signing_failed"))
                                .map_err(|e| Status::internal(format!("abort failed: {}", e)))?
                        };

                        let abort_receipt_bytes = serde_json::to_vec(&abort_receipt)
                            .expect("abort receipt serialization should not fail");

                        // Return success=false with the abort receipt (signing failure is a capability violation class)
                        Ok(Response::new(ExecuteResponse {
                            success: false,
                            result: Vec::new(),
                            receipt: abort_receipt_bytes,
                            error_message: "commit signing failed".to_string(),
                        }))
                    }
                    Err(e) => {
                        // Other commit errors (e.g., prepare not found)
                        let abort_receipt = {
                            let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                                Status::internal(format!("receipt emitter lock failed: {}", e))
                            })?;
                            emitter
                                .abort(prepare_handle, Some("commit_failed"))
                                .map_err(|e| Status::internal(format!("abort failed: {}", e)))?
                        };

                        let abort_receipt_bytes = serde_json::to_vec(&abort_receipt)
                            .expect("abort receipt serialization should not fail");

                        Ok(Response::new(ExecuteResponse {
                            success: false,
                            result: Vec::new(),
                            receipt: abort_receipt_bytes,
                            error_message: e.to_string(),
                        }))
                    }
                }
            }
            Err(status) => {
                // WASM execution trapped: create legacy trap receipt for backward compatibility
                // and also abort the two-phase flow for chain integrity.
                tracing::warn!(error = %status, "WASM execution trapped, emitting legacy trap receipt");

                // Capture fuel consumed before aborting
                let fuel_consumed = sandbox.fuel_consumed();

                // 1. Abort the two-phase flow for chain integrity (internal)
                let _ = {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter.abort(prepare_handle, Some("trap"))
                };

                // 2. Create legacy trap receipt for client response (backward compatibility)
                // Format: phase="", result="trap", with correct fuel_consumed
                // Uses emit_legacy which does NOT add to chain or update chain hash
                let trap_receipt = {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter
                        .emit_legacy(&capability_name, "execute", "", 0, "trap", fuel_consumed)
                        .map_err(|e| Status::internal(format!("receipt emission failed: {}", e)))?
                };

                let trap_receipt_bytes = serde_json::to_vec(&trap_receipt)
                    .expect("trap receipt serialization should not fail");

                // For capability violations (traversal, size exceed) and signing failures,
                // return success=false instead of gRPC error status, per REQ-715 / S-701, S-702
                if status.code() == tonic::Code::FailedPrecondition
                    || status.code() == tonic::Code::Internal
                {
                    Ok(Response::new(ExecuteResponse {
                        success: false,
                        result: Vec::new(),
                        receipt: trap_receipt_bytes,
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
            let receipt: ExecutionReceipt = serde_json::from_slice(receipt_bytes).map_err(|e| {
                tracing::warn!(index = i, error = %e, "failed to parse receipt");
                Status::invalid_argument(format!("receipt at index {} is not valid JSON: {}", i, e))
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

        let emitter = self
            .receipt_emitter
            .lock()
            .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;

        let chain = emitter.chain();

        // Serialize each receipt to JSON bytes
        let receipts: Vec<Vec<u8>> = chain
            .iter()
            .map(|r| serde_json::to_vec(r).expect("receipt serialization should not fail"))
            .collect();

        tracing::info!(count = receipts.len(), "returning receipt chain");

        Ok(Response::new(GetReceiptChainResponse { receipts }))
    }

    async fn execute_prepare(
        &self,
        request: Request<ExecutePrepareRequest>,
    ) -> Result<Response<ExecutePrepareResponse>, Status> {
        // Acquire semaphore permit for concurrency limiting (REQ-810)
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("max concurrent executions exceeded"))?;

        let req = request.into_inner();
        let capability_name = req.capability_name;
        let config_bytes = req.config;
        let wasm_module_bytes = req.wasm_module;

        tracing::info!(capability = %capability_name, "execute_prepare request received");

        // 1. Parse TOML config from request bytes
        let config_str = String::from_utf8(config_bytes).map_err(|e| {
            tracing::warn!(error = %e, "invalid UTF-8 in config");
            Status::invalid_argument(format!("invalid config encoding: {}", e))
        })?;

        // 2. Parse the TOML config to extract capabilities
        let policy_config: PolicyConfig = toml::from_str(&config_str).map_err(|e| {
            tracing::warn!(error = %e, "invalid TOML config");
            Status::invalid_argument(format!("invalid TOML config: {}", e))
        })?;

        // Clone policy_config before moving it into try_into_capabilities
        let policy_config_for_prepare = policy_config.clone();
        let capabilities = policy_config
            .try_into_capabilities()
            .map_err(|e| Status::invalid_argument(format!("invalid capabilities: {}", e)))?;

        // 3. Validate WASM module provided
        if wasm_module_bytes.is_empty() {
            return Err(Status::invalid_argument("wasm_module is required"));
        }

        // 4. Check TwoPhaseReceipts capability is granted (REQ-821)
        let has_two_phase = capabilities
            .iter()
            .any(|c| c.capability_name() == "two_phase_receipts");
        if !has_two_phase {
            return Ok(Response::new(ExecutePrepareResponse {
                success: false,
                receipt: Vec::new(),
                prepare_hash: String::new(),
                error_message: "two_phase_receipts capability not granted".to_string(),
            }));
        }

        // 5. Find the matching capability by name (the actual capability to execute)
        let _matching_cap = capabilities
            .iter()
            .find(|c| c.capability_name() == capability_name)
            .ok_or_else(|| {
                Status::failed_precondition(format!(
                    "capability '{}' not granted in config",
                    capability_name
                ))
            })?;

        // 6. Call ReceiptEmitter::prepare() to create sandbox and sign prepare receipt
        let (prepare_receipt, prepare_handle) = {
            let mut emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .prepare(
                    &capability_name,
                    policy_config_for_prepare, // use the cloned config
                    &wasm_module_bytes,
                    self.fuel_budget,
                )
                .map_err(|e| Status::internal(format!("prepare failed: {}", e)))?
        };

        // 7. Store the sandbox handle in the emitter for later commit/abort
        // The pending map in ReceiptEmitter already holds the SandboxHandle
        // No additional storage needed here

        // 8. Return the prepare receipt and prepare_hash
        let prepare_hash = hex::encode(prepare_handle.pending_hash());
        let receipt_bytes = serde_json::to_vec(&prepare_receipt)
            .expect("prepare receipt serialization should not fail");

        Ok(Response::new(ExecutePrepareResponse {
            success: true,
            receipt: receipt_bytes,
            prepare_hash,
            error_message: String::new(),
        }))
    }

    async fn execute_commit(
        &self,
        request: Request<ExecuteCommitRequest>,
    ) -> Result<Response<ExecuteCommitResponse>, Status> {
        // Acquire semaphore permit for concurrency limiting
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("max concurrent executions exceeded"))?;

        let req = request.into_inner();
        let prepare_hash_hex = req.prepare_hash;
        let _result_bytes = req.result; // not used directly, passed to commit

        tracing::info!(prepare_hash = %prepare_hash_hex, "execute_commit request received");

        // Parse prepare_hash from hex
        let prepare_hash_bytes = hex::decode(&prepare_hash_hex).map_err(|e| {
            tracing::warn!(error = %e, "invalid prepare_hash hex");
            Status::invalid_argument(format!("invalid prepare_hash: {}", e))
        })?;
        let mut prepare_hash = [0u8; 32];
        if prepare_hash_bytes.len() != 32 {
            return Err(Status::invalid_argument("prepare_hash must be 32 bytes"));
        }
        prepare_hash.copy_from_slice(&prepare_hash_bytes);

        let handle = PrepareHandle::new(prepare_hash);

        // 1. Get the sandbox handle from the pending entry
        let sandbox_handle = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter.get_pending_sandbox(&handle).ok_or_else(|| {
                Status::not_found("prepare_hash not found or already committed/aborted")
            })?
        };

        // 2. Get the capability name and config for receipt emission
        let (capability_name, config) = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            let cap_name = emitter
                .get_pending_capability_name(&handle)
                .ok_or_else(|| Status::not_found("prepare_hash not found"))?;
            let cfg = emitter
                .get_pending_config(&handle)
                .ok_or_else(|| Status::not_found("prepare_hash not found"))?;
            (cap_name, cfg)
        };

        // 3. Get the WASM module from the pending entry
        let wasm_module = {
            let emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .get_pending_wasm_module(&handle)
                .ok_or_else(|| Status::not_found("prepare_hash not found"))?
        };

        // 4. Execute WASM in the prepared sandbox
        let mut sandbox = sandbox_handle.lock().await;
        sandbox.store_mut().data_mut().receipt_emitter = Some(self.receipt_emitter.clone());

        // Test-only transport override injection
        #[cfg(feature = "test-utils")]
        if std::env::var("AEGIS_TEST_MODE").is_ok() {
            if let Ok(port_str) = std::env::var("AEGIS_TEST_NETWORK_PORT") {
                let state = sandbox.store_mut().data_mut();
                state.network_test_port = port_str.parse::<u16>().ok();
                if let Ok(ca_path) = std::env::var("AEGIS_TEST_CA_PEM") {
                    state.network_test_ca_pem = std::fs::read(&ca_path).ok();
                }
            }
        }

        // Execute the capability
        let result_bytes = self
            .execute_wasm_capability(
                &mut sandbox,
                &config
                    .clone()
                    .try_into_capabilities()
                    .map_err(|e| Status::internal(e.to_string()))?,
                &wasm_module,
            )
            .await;

        // Capture fuel consumed
        let fuel_consumed = sandbox.fuel_consumed();

        // 5. Determine path/result_hash/size for receipt based on capability
        let (capability_path, _result_hash, size) = match &result_bytes {
            Ok(result_bytes) => {
                let result_hash = blake3::hash(result_bytes).to_hex().to_string();
                let cap = config
                    .try_into_capabilities()
                    .map_err(|e| Status::internal(e.to_string()))?
                    .into_iter()
                    .find(|c| c.capability_name() == capability_name)
                    .ok_or_else(|| Status::internal("capability not found in config"))?;

                match cap {
                    crate::capabilities::Capability::FilesystemRead(params) => (
                        params.allowed_root.to_string_lossy().to_string(),
                        result_hash,
                        result_bytes.len() as u64,
                    ),
                    crate::capabilities::Capability::FilesystemWrite(params) => (
                        params.allowed_root.to_string_lossy().to_string(),
                        result_hash,
                        result_bytes.len() as u64,
                    ),
                    crate::capabilities::Capability::NetworkHttp(_) => {
                        match sandbox.store_mut().data_mut().network_fetch.take() {
                            Some(fetch) => (fetch.url, fetch.body_blake3, fetch.body_len),
                            None => (String::new(), result_hash, result_bytes.len() as u64),
                        }
                    }
                    crate::capabilities::Capability::TwoPhaseReceipts => {
                        return Err(Status::internal(
                            "two_phase_receipts should not be executable",
                        ));
                    }
                }
            }
            Err(_) => (String::new(), String::new(), 0),
        };

        // 6. Call ReceiptEmitter::commit() with the results (pure emitter)
        let commit_receipt = {
            let mut emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .commit(
                    handle,
                    &result_bytes.unwrap_or_default(),
                    &capability_path,
                    size,
                    fuel_consumed,
                )
                .map_err(|e| Status::internal(format!("commit failed: {}", e)))?
        };

        let receipt_bytes = serde_json::to_vec(&commit_receipt)
            .expect("commit receipt serialization should not fail");

        Ok(Response::new(ExecuteCommitResponse {
            success: true,
            receipt: receipt_bytes,
            error_message: String::new(),
        }))
    }

    async fn execute_abort(
        &self,
        request: Request<ExecuteAbortRequest>,
    ) -> Result<Response<ExecuteAbortResponse>, Status> {
        let req = request.into_inner();
        let prepare_hash_hex = req.prepare_hash;

        tracing::info!(prepare_hash = %prepare_hash_hex, "execute_abort request received");

        // Parse prepare_hash from hex
        let prepare_hash_bytes = hex::decode(&prepare_hash_hex).map_err(|e| {
            tracing::warn!(error = %e, "invalid prepare_hash hex");
            Status::invalid_argument(format!("invalid prepare_hash: {}", e))
        })?;
        let mut prepare_hash = [0u8; 32];
        if prepare_hash_bytes.len() != 32 {
            return Err(Status::invalid_argument("prepare_hash must be 32 bytes"));
        }
        prepare_hash.copy_from_slice(&prepare_hash_bytes);

        let handle = PrepareHandle::new(prepare_hash);

        // Call ReceiptEmitter::abort()
        let abort_receipt = {
            let mut emitter = self
                .receipt_emitter
                .lock()
                .map_err(|e| Status::internal(format!("receipt emitter lock failed: {}", e)))?;
            emitter
                .abort(handle, None)
                .map_err(|e| Status::internal(format!("abort failed: {}", e)))?
        };

        let receipt_bytes = serde_json::to_vec(&abort_receipt)
            .expect("abort receipt serialization should not fail");

        Ok(Response::new(ExecuteAbortResponse {
            success: true,
            receipt: receipt_bytes,
            error_message: String::new(),
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
                // Print full error chain for debugging
                let mut msg = e.to_string();
                if let Some(source) = e.source() {
                    msg.push_str(" | source: ");
                    msg.push_str(&source.to_string());
                    if let Some(source2) = source.source() {
                        msg.push_str(" | source2: ");
                        msg.push_str(&source2.to_string());
                    }
                }
                tracing::error!(error = %e, chain = %msg, "failed to instantiate WASM module");
                Status::internal(format!("WASM instantiation failed: {}", e))
            })?;

        // Get the exported `execute` function - now returns (ptr, len)
        let execute_func = instance
            .get_typed_func::<(), (i32, i32)>(&mut sandbox.store_mut(), "execute")
            .map_err(|e| {
                tracing::error!(error = %e, "exported function 'execute' not found");
                Status::failed_precondition(format!(
                    "WASM module must export 'execute' function returning (ptr, len): {}",
                    e
                ))
            })?;

        // Call the execute function (no arguments, returns ptr and len)
        let (ptr, len) = execute_func.call(sandbox.store_mut(), ()).map_err(|e| {
            tracing::error!(error = %e, "WASM execution trapped");
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

            // D4: the "network " guard MUST be the first branch of this cascade.
            // wasmtime 24 wraps host-fn traps in a backtrace frame — the catalog
            // message lives in the error chain, not in the top-level Display —
            // so walk the chain and structurally isolate all network messages
            // from the fs branches below. An S-601 message embeds the
            // guest-controlled URL (may contain `..` / `size` / `max`) and must
            // never be evaluated against them. Inside the guard, dispatch by the
            // catalog message's second word (endpoint/method/connection/timeout/
            // response-size/rate).
            let network_msg =
                std::iter::successors(e.source(), |src| src.source()).find_map(|src| {
                    let frame = src.to_string();
                    frame.starts_with("network ").then_some(frame)
                });

            let clean_msg = if let Some(network_msg) = network_msg {
                if network_msg.starts_with("network endpoint") {
                    "network endpoint not allowed"
                } else if network_msg.starts_with("network method") {
                    "network method not allowed"
                } else if network_msg.starts_with("network connection") {
                    "network connection failed"
                } else if network_msg.starts_with("network timeout") {
                    "network timeout exceeded"
                } else if network_msg.starts_with("network response size") {
                    "network response size limit exceeded"
                } else if network_msg.starts_with("network rate limit") {
                    "network rate limit exceeded"
                } else {
                    "WASM execution trapped"
                }
            // D4 fuel arm (Phase 8): evaluated after network guard, before fs cascade.
            // Matches deterministic wasmtime literal "all fuel consumed" from trap_encoding.rs:142.
            } else if std::iter::successors(e.source(), |s| s.source())
                .map(|s| s.to_string())
                .any(|f| f.contains("all fuel consumed"))
            {
                "fuel budget exceeded"
            } else if msg.contains("traversal")
                || msg.contains("..")
                || msg.contains("outside")
                || msg.contains("traversal attempt")
            {
                "path traversal attempt detected"
            } else if msg.contains("size") || msg.contains("exceed") || msg.contains("max") {
                "size limit exceeded"
            } else if msg.contains("signing")
                || msg.contains("receipt")
                || msg.contains("test-forced")
            {
                "receipt signing failure"
            } else {
                "WASM execution trapped"
            };
            Status::failed_precondition(clean_msg)
        })?;

        tracing::debug!("DEBUG execute_wasm: ptr={}, len={}", ptr, len);

        // Validate ptr/len
        if len < 0 || ptr < 0 {
            return Err(Status::internal("invalid ptr/len from WASM module"));
        }

        // Get memory export from the instance (after call, store is free)
        let memory = instance
            .get_memory(sandbox.store_mut(), "memory")
            .ok_or_else(|| Status::internal("memory export required"))?;

        // Read result bytes from guest memory
        let ptr = ptr as usize;
        let len = len as usize;
        // Get store reference once to avoid temporary borrow issue
        let store = sandbox.store_mut();
        let data = memory.data(store);
        if ptr + len > data.len() {
            return Err(Status::internal(
                "WASM result ptr/len exceeds memory bounds",
            ));
        }
        let result_bytes = data[ptr..ptr + len].to_vec();

        Ok(result_bytes)
    }
}
