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
    pub fuel_budget: Option<u64>,
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
            .map_err(|_| Status::resource_exhausted("max concurrent executions exceeded"))?;

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
        sandbox.store_mut().data_mut().receipt_emitter = Some(self.receipt_emitter.clone());

        // 5b. Test-only transport override injection (REQ-610 E2E). The
        //     `test-utils` feature is enabled exclusively through the
        //     dev-dependency `aegis = { path = ".", features = ["test-utils"] }`,
        //     so this block never compiles into production binaries. Fail-closed:
        //     the override applies ONLY when test mode is active — a stray
        //     `AEGIS_TEST_NETWORK_PORT` in a production environment is ignored.
        #[cfg(feature = "test-utils")]
        if test_mode {
            if let Ok(port_str) = std::env::var("AEGIS_TEST_NETWORK_PORT") {
                let state = sandbox.store_mut().data_mut();
                state.network_test_port = port_str.parse::<u16>().ok();
                if let Ok(ca_path) = std::env::var("AEGIS_TEST_CA_PEM") {
                    state.network_test_ca_pem = std::fs::read(&ca_path).ok();
                }
            }
        }

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
        let result_bytes = self
            .execute_wasm_capability(&mut sandbox, &capabilities, &wasm_module_bytes)
            .await;

        // Capture fuel consumed post-call (works for both success and trap paths).
        // get_fuel() returns remaining fuel; budget_resolved - remaining = consumed.
        let fuel_consumed = sandbox.fuel_consumed();

        tracing::debug!("DEBUG execute: wasm execution result = {:?}", result_bytes);

        match result_bytes {
            Ok(result_bytes) => {
                // Calculate BLAKE3 hash for receipt
                let result_hash = blake3::hash(&result_bytes).to_hex().to_string();

                // Extract path from capability config for receipt (REQ-610, D5).
                // For network.http the host-recorded `FetchRecord` is authoritative:
                // path = the URL actually fetched, result = BLAKE3(body) hex,
                // size = response byte length. When no fetch was performed
                // (`None` — module never fetched, or trapped mid-flight), fall
                // back to the generic values; REQ-610 constrains performed fetches.
                let (capability_path, result_hash, size) = match &matching_cap {
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
                };

                // 8. Emit a success receipt for this execution
                {
                    let mut emitter = self.receipt_emitter.lock().map_err(|e| {
                        Status::internal(format!("receipt emitter lock failed: {}", e))
                    })?;
                    emitter
                        .emit(
                            &capability_name,
                            "execute",
                            &capability_path,
                            size,
                            &result_hash,
                            fuel_consumed,
                        )
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
                    result: result_bytes,
                    receipt: receipt_bytes,
                    error_message: String::new(),
                }))
            }
            Err(status) => {
                // Emit trap receipt on failure (with fuel_consumed captured post-call)
                if let Ok(mut emitter) = self.receipt_emitter.lock() {
                    let _ = emitter.emit(&capability_name, "execute", "", 0, "trap", fuel_consumed);
                }

                // For capability violations (traversal, size exceed) and signing failures,
                // return success=false instead of gRPC error status, per REQ-715 / S-701, S-702
                if status.code() == tonic::Code::FailedPrecondition
                    || status.code() == tonic::Code::Internal
                {
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
