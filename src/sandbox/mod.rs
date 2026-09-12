//! Wasmtime-based sandboxing module
//!
//! Provides secure WASM execution with epoch-based CPU interruption
//! and configurable resource limits via `StoreLimitsBuilder`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use ring::signature::Ed25519KeyPair;
use wasmtime::*;

use crate::capabilities::{Capability, NetworkHttpParams};
use crate::receipts::{ExecutionReceipt, ReceiptEmitter};

/// Configuration for sandbox resource limits and epoch interval.
///
/// Controls the four hard resource limits enforced by `StoreLimitsBuilder`
/// and the epoch timer interval for CPU interruption.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum linear memory size in bytes (default: 1 MB = 1 << 20).
    /// Prevents OOM attacks by capping total memory allocation.
    pub memory_size: usize,
    /// Maximum table elements (default: 1024).
    /// Bounds function pointer tables to prevent fork-bomb instantiation.
    pub table_elements: u32,
    /// Maximum instances per store (default: 4).
    /// Prevents fork-bomb instantiation of nested modules.
    pub instances: usize,
    /// Maximum memories per instance (default: 2).
    /// Prevents memory fragmentation abuse.
    pub memories: usize,
    /// Epoch timer interval (default: 100ms).
    /// Controls how frequently `increment_epoch()` is called on the engine.
    pub epoch_interval: Duration,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_size: 1 << 20,
            table_elements: 1024,
            instances: 4,
            memories: 2,
            epoch_interval: Duration::from_millis(100),
        }
    }
}

/// Configuration for a capability grant (filesystem.read, filesystem.write, etc.).
///
/// Captures the allowed root directory and maximum size for a
/// single capability. Built from a `Capability` via
/// `From<&Capability>`.
///
/// **Fail-closed invariant**: all paths outside `allowed_root` or operations
/// exceeding `max_bytes` produce a trap (REQ-107).
#[derive(Debug, Clone)]
pub struct CapabilityConfig {
    /// Capability name (e.g., "filesystem.read", "filesystem.write").
    pub name: String,
    /// Canonicalized absolute path that serves as the root boundary.
    pub allowed_root: PathBuf,
    /// Maximum size in bytes for this capability (default: 1 MB).
    pub max_bytes: u64,
}

impl From<&Capability> for CapabilityConfig {
    fn from(cap: &Capability) -> Self {
        match cap {
            Capability::FilesystemRead(params) => Self {
                name: "filesystem.read".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_bytes: params.max_read_bytes,
            },
            Capability::FilesystemWrite(params) => Self {
                name: "filesystem.write".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_bytes: params.max_write_bytes,
            },
            Capability::NetworkHttp(params) => Self {
                name: "network.http".to_string(),
                allowed_root: PathBuf::new(),
                max_bytes: params.max_requests_per_second,
            },
        }
    }
}

/// Manages the epoch timer thread lifecycle.
///
/// Spawned by `Sandbox::new_with_limits()`. The thread calls
/// `engine.increment_epoch()` at the configured interval using
/// `park_timeout` for responsive shutdown.
///
/// **Fail-closed invariant**: all resource exhaustion paths produce
/// a Wasmtime `Trap`, never graceful errors.
pub struct EpochInterrupter {
    thread: std::thread::Thread,
    handle: Option<JoinHandle<()>>,
    stop: std::sync::Arc<AtomicBool>,
}

impl EpochInterrupter {
    /// Spawns a new epoch timer thread that calls `engine.increment_epoch()`
    /// every `interval`. Uses `park_timeout` for responsive shutdown and
    /// `Arc<AtomicBool>` stop flag for graceful termination.
    ///
    /// NOTE: Uses `mpsc::channel` to relay the spawned thread's own `Thread`
    /// handle back to the creator. `std::thread::current()` called inside
    /// `new()` returns the CALLER's thread, not the spawned thread — calling
    /// `unpark()` on that would deadlock. The spawned thread must send its
    /// own handle via channel. See DECISIONS.md AD-003.
    pub fn new(engine: Engine, interval: Duration) -> Self {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            // Send our Thread handle back so the creator can unpark us
            let current = std::thread::current();
            let _ = tx.send(current);

            loop {
                std::thread::park_timeout(interval);
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }
                engine.increment_epoch();
            }
        });

        // Block until the spawned thread sends its handle
        let thread = rx
            .recv()
            .expect("EpochInterrupter: failed to receive thread handle");

        EpochInterrupter {
            thread,
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for EpochInterrupter {
    fn drop(&mut self) {
        // Signal the thread to stop
        self.stop.store(true, Ordering::Relaxed);
        // Wake the thread immediately from park_timeout
        self.thread.unpark();
        // Wait for the thread to exit
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Internal state stored in the Wasmtime `Store`.
///
/// Holds the `StoreLimits` which implements `ResourceLimiter` and
/// enforces the four hard resource limits configured via `SandboxConfig`.
/// Also holds capability configurations for host function closures.
#[derive(Default)]
pub struct SandboxState {
    limits: StoreLimits,
    /// Capability configs captured for host function access during execution.
    capabilities: Vec<CapabilityConfig>,
    /// Shared receipt emitter for signed execution receipts (Phase 5, REQ-430).
    /// `Arc<Mutex<ReceiptEmitter>>` enables a single hash chain across concurrent gRPC RPCs.
    pub receipt_emitter: Option<Arc<Mutex<ReceiptEmitter>>>,
    /// Network HTTP policy carrier (D2) — `None` for sandboxes without the
    /// capability (zero cost), keeping `CapabilityConfig` untouched.
    pub network_http: Option<NetworkHttpParams>,
    /// Per-execution token bucket (D3) — fresh per Execute, refill-on-demand,
    /// no `Mutex` (single-threaded store).
    pub network_bucket: Option<TokenBucket>,
    /// Cached ureq agent (connect 2s / total 5s); built lazily on first fetch.
    network_agent: Option<ureq::Agent>,
    /// Host-authoritative fetch state for the Execute-level receipt (D5, REQ-610).
    /// Set only on success; traps leave it `None` and the handler falls back to `""`.
    pub network_fetch: Option<FetchRecord>,
    /// Test-only: connect-port override for hermetic `https://localhost` tests.
    /// URL-policy semantics are unchanged — the override applies to the transport
    /// only, after validation.
    #[cfg(feature = "test-utils")]
    pub network_test_port: Option<u16>,
    /// Test-only: CA certificate (PEM) injected as the TLS trust root.
    #[cfg(feature = "test-utils")]
    pub network_test_ca_pem: Option<Vec<u8>>,
}

/// Per-execution token bucket rate limiter (D3, REQ-606, S-606).
///
/// Refill-on-demand: tokens accrue from `last_refill` only when `try_take` is
/// called — no timer thread. Capacity equals `rate`, so a burst of `rate`
/// requests is allowed up front, then the bucket refills at `rate`/s (E-605).
#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    rate: f64,
    capacity: f64,
}

impl TokenBucket {
    /// Fresh bucket, full to capacity (`max_requests_per_second`).
    fn new(requests_per_second: u64) -> Self {
        let capacity = requests_per_second as f64;
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
            rate: capacity,
            capacity,
        }
    }

    /// Refill on demand, then consume one token if available.
    ///
    /// Returns `false` (empty bucket) when fewer than one token is available.
    fn try_take(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// `min(cap, tokens + elapsed * rate)` — tokens never exceed capacity.
    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
            self.last_refill = Instant::now();
        }
    }
}

/// Host-authoritative record of the last performed fetch (D5, REQ-610).
///
/// Set by `aegis_http_fetch` on success only; the guest never reports what it
/// fetched. The Execute handler reads this for the execute-level receipt path,
/// size, and result (Phase 3 wiring).
#[derive(Debug, Clone)]
pub struct FetchRecord {
    /// The exact URL that was fetched (`https://host/path`).
    pub url: String,
    /// `blake3(body)` as hex.
    pub body_blake3: String,
    /// Response body length in bytes.
    pub body_len: u64,
}

/// Load an Ed25519 key pair from a TOML key file (REQ-420, REQ-423, REQ-424, REQ-426).
///
/// Key file format:
/// ```toml
/// [signing_key]
/// private_key = "base64-encoded-pkcs8"
/// public_key = "base64-encoded-public-key"
/// ```
///
/// Enforces:
/// - File must exist (REQ-424: fail-closed)
/// - File must have 0600 permissions on Unix (REQ-423)
/// - Keys must be valid base64 and valid Ed25519 (REQ-424)
/// - Non-Unix platforms: fail immediately (REQ-426)
pub fn load_receipt_keypair(key_path: &std::path::Path) -> Result<Ed25519KeyPair> {
    // REQ-426: Platform check — non-Unix unsupported
    #[cfg(not(unix))]
    {
        return Err(anyhow::anyhow!(
            "unsupported platform: receipts key management requires Unix"
        ));
    }

    // REQ-423: Validate 0600 permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(key_path)
            .map_err(|e| anyhow::anyhow!("key file not found: {}: {}", key_path.display(), e))?;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            bail!(
                "key file {} has insecure permissions {:o} (required: 0600)",
                key_path.display(),
                mode & 0o777
            );
        }
    }

    // Read and parse key file
    let content = std::fs::read_to_string(key_path)
        .map_err(|e| anyhow::anyhow!("failed to read key file: {}", e))?;
    let parsed: toml::Value =
        toml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid key file TOML: {}", e))?;

    let signing_key = parsed
        .get("signing_key")
        .ok_or_else(|| anyhow::anyhow!("key file missing [signing_key] section"))?;

    let private_key_b64 = signing_key
        .get("private_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("key file missing signing_key.private_key"))?;

    // Decode base64 PKCS8
    let pkcs8_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, private_key_b64)
            .map_err(|e| anyhow::anyhow!("invalid base64 in private_key: {}", e))?;

    // Create key pair from PKCS8
    let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
        .map_err(|e| anyhow::anyhow!("invalid Ed25519 key: {}", e))?;

    Ok(key_pair)
}

/// Secure WASM sandbox with epoch interruption and resource limits.
///
/// Creates an `Engine` with `epoch_interruption(true)`, builds a `Store`
/// with `StoreLimitsBuilder` limits, and optionally spawns an `EpochInterrupter`
/// thread for CPU time-slicing.
///
/// **Fail-closed invariant**: all resource exhaustion paths produce
/// a Wasmtime `Trap`, never graceful errors.
pub struct Sandbox {
    engine: Engine,
    store: Store<SandboxState>,
    _epoch_interrupter: Option<EpochInterrupter>,
}

impl Sandbox {
    /// Creates a new sandbox with the given configuration.
    ///
    /// Configures `Engine` with `epoch_interruption(true)`, builds `StoreLimits`
    /// via `StoreLimitsBuilder`, and spawns the epoch timer thread.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the engine or store cannot be created.
    pub fn new_with_limits(config: SandboxConfig) -> Result<Self> {
        Self::new_with_config(config, true)
    }

    /// Creates a new sandbox with the given configuration, optionally enabling epoch interruption.
    ///
    /// When `enable_epoch` is `false`, the engine is created without epoch interruption.
    /// Useful for tests that only verify memory/table/instance limits without CPU time-slicing.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the engine or store cannot be created.
    pub fn new_with_config(config: SandboxConfig, enable_epoch: bool) -> Result<Self> {
        let mut engine_config = Config::new();
        if enable_epoch {
            engine_config.epoch_interruption(true);
            // Epoch-only interruption (no fuel metering).
            // Limitation: tight loops without host calls may not yield for full epoch interval.
            // See DECISIONS.md AD-002. Phase 1 host functions provide natural epoch check points.
        }
        let engine = Engine::new(&engine_config)?;

        // Trap on grow failure: memory.grow beyond limit raises Trap,
        // not -1 return. This is the fail-closed invariant (REQ-003).
        // See DECISIONS.md AD-001 for non-spec rationale.
        let limits = StoreLimitsBuilder::new()
            .memory_size(config.memory_size)
            .table_elements(config.table_elements)
            .instances(config.instances)
            .memories(config.memories)
            .trap_on_grow_failure(true)
            .build();

        let mut store = Store::new(
            &engine,
            SandboxState {
                limits,
                capabilities: Vec::new(),
                receipt_emitter: None,
                ..Default::default()
            },
        );
        store.limiter(|state| &mut state.limits);

        let epoch_interrupter = if enable_epoch {
            Some(EpochInterrupter::new(engine.clone(), config.epoch_interval))
        } else {
            None
        };

        Ok(Self {
            engine,
            store,
            _epoch_interrupter: epoch_interrupter,
        })
    }

    /// Returns a reference to the receipt chain for verification/testing.
    pub fn get_receipt_chain(&self) -> Vec<ExecutionReceipt> {
        self.store
            .data()
            .receipt_emitter
            .as_ref()
            .map(|e| e.lock().unwrap().chain().to_vec())
            .unwrap_or_default()
    }

    /// Instantiates a WASM module in the sandbox.
    ///
    /// Compiles the raw WASM bytes into a `Module` and creates an `Instance`.
    /// The instance runs within the resource limits configured on this sandbox.
    ///
    /// # Errors
    ///
    /// Returns `Err` if compilation or instantiation fails.
    pub fn instantiate(&mut self, wasm_bytes: &[u8]) -> Result<Instance> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let instance = Instance::new(&mut self.store, &module, &[])?;
        Ok(instance)
    }

    /// Instantiates a WASM module with capability-driven host functions.
    ///
    /// For each `filesystem.read` capability, registers `aegis::fs_read`
    /// via `Linker::func_wrap` with the capability's `allowed_root` and
    /// `max_bytes` captured in the closure.
    ///
    /// **Fail-closed invariant**: all violation paths (path escape,
    /// traversal, size exceed, fd leak) produce a `Trap`, never a
    /// graceful error (REQ-107).
    ///
    /// # Errors
    ///
    /// Returns `Err` if compilation or instantiation fails. WASI imports
    /// in the module produce a linking error at instantiation (S-4).
    pub fn instantiate_with_capabilities(
        &mut self,
        wasm_bytes: &[u8],
        capabilities: &[Capability],
    ) -> Result<Instance> {
        // Convert capabilities to config for closure capture
        let capability_configs: Vec<CapabilityConfig> =
            capabilities.iter().map(CapabilityConfig::from).collect();

        // Store in SandboxState for host function access
        self.store.data_mut().capabilities = capability_configs.clone();

        // Wire the network HTTP policy carrier + per-execution bucket (D2/D3).
        self.wire_network_state(capabilities);

        // Build linker with host functions per capability variant
        let mut linker = Linker::new(&self.engine);

        for cap in capabilities {
            match cap {
                Capability::FilesystemRead(_params) => {
                    linker.func_wrap(
                        "aegis",
                        "fs_read",
                        move |caller: Caller<'_, SandboxState>,
                              path_ptr: i32,
                              path_len: i32,
                              out_ptr: i32,
                              out_len: i32|
                              -> Result<i32> {
                            aegis_fs_read(caller, path_ptr, path_len, out_ptr, out_len)
                        },
                    )?;
                }
                Capability::FilesystemWrite(_) => {
                    linker.func_wrap(
                        "aegis",
                        "fs_write",
                        move |caller: Caller<'_, SandboxState>,
                              path_ptr: i32,
                              path_len: i32,
                              data_ptr: i32,
                              data_len: i32|
                              -> Result<i32> {
                            aegis_fs_write(caller, path_ptr, path_len, data_ptr, data_len)
                        },
                    )?;
                }
                Capability::NetworkHttp(_) => {
                    linker.func_wrap(
                        "aegis",
                        "http_fetch",
                        move |caller: Caller<'_, SandboxState>,
                              method_ptr: i32,
                              method_len: i32,
                              url_ptr: i32,
                              url_len: i32,
                              out_ptr: i32,
                              out_len: i32|
                              -> Result<i32> {
                            aegis_http_fetch(
                                caller, method_ptr, method_len, url_ptr, url_len, out_ptr, out_len,
                            )
                        },
                    )?;
                }
            }
        }

        let module = Module::new(&self.engine, wasm_bytes)?;
        let instance = linker.instantiate(&mut self.store, &module)?;
        Ok(instance)
    }

    /// Returns a mutable reference to the underlying `Store`.
    ///
    /// Required for calling functions on instantiated modules.
    pub fn store_mut(&mut self) -> &mut Store<SandboxState> {
        &mut self.store
    }

    /// Wire `network.http` policy into the store (D2/D3).
    ///
    /// Sets the policy carrier (`network_http`) and a fresh per-execution token
    /// bucket whenever the capability is granted. A `Sandbox` is created fresh
    /// per Execute RPC, so the bucket resets between executions (REQ-606, E-605).
    /// No-op for sandboxes without the capability — zero cost (`Option`).
    fn wire_network_state(&mut self, capabilities: &[Capability]) {
        if let Some(params) = capabilities.iter().find_map(|cap| match cap {
            Capability::NetworkHttp(params) => Some(params.clone()),
            _ => None,
        }) {
            let state = self.store.data_mut();
            state.network_http = Some(params.clone());
            state.network_bucket = Some(TokenBucket::new(params.max_requests_per_second));
            state.network_agent = None;
            state.network_fetch = None;
        }
    }

    /// Create a sandbox from a TOML config file (REQ-304).
    ///
    /// Loads `PolicyConfig`, converts capabilities, validates, and creates
    /// the sandbox with registered host functions. If `[receipts]` section
    /// is present with a valid key file, initializes `ReceiptEmitter`.
    /// Fail-closed on any error.
    pub fn from_config(config_path: &str) -> Result<Self> {
        let policy_config =
            crate::config::PolicyConfig::load(config_path).map_err(|e| anyhow::anyhow!(e))?;

        // Extract receipts config before policy_config is consumed
        let receipts_config = policy_config.receipts.clone();

        let capabilities = policy_config
            .try_into_capabilities()
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut sandbox = Self::new_with_limits(SandboxConfig::default())?;

        // Convert to CapabilityConfig and store
        let capability_configs: Vec<CapabilityConfig> =
            capabilities.iter().map(CapabilityConfig::from).collect();
        sandbox.store.data_mut().capabilities = capability_configs;
        sandbox.wire_network_state(&capabilities);

        // Load receipt key pair if receipts config is present (REQ-420, REQ-421)
        if let Some(ref receipts_config) = receipts_config {
            let key_pair = load_receipt_keypair(&receipts_config.key_path)
                .map_err(|e| anyhow::anyhow!("receipts key load failed: {}", e))?;
            let emitter = ReceiptEmitter::new(key_pair);
            sandbox.store.data_mut().receipt_emitter = Some(Arc::new(Mutex::new(emitter)));
        }

        Ok(sandbox)
    }

    /// Access the receipt chain for verification and CLI export.
    ///
    /// Returns a reference to the shared `Arc<Mutex<ReceiptEmitter>>`.
    pub fn get_receipt_emitter(&self) -> Option<&Arc<Mutex<ReceiptEmitter>>> {
        self.store.data().receipt_emitter.as_ref()
    }
}

/// Host function: read-only filesystem access within an allowed root.
///
/// Registered via `Linker::func_wrap` when `filesystem.read` capability
/// is present. Validates path against `allowed_root`, rejects `..`
/// traversal, enforces `max_bytes` via `stat()`, reads via
/// `std::fs::read`, and traps on ALL violations (fail-closed, REQ-107).
///
/// Phase 3: Each validation point emits a signed execution receipt via
/// `ReceiptEmitter`. If receipt emission fails, the function immediately
/// returns a Trap — no data is returned to the caller (fail-closed, REQ-433).
fn aegis_fs_read(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> Result<i32> {
    // 1. Get capability config FIRST — before any validation that could fail.
    let cap = caller
        .data()
        .capabilities
        .iter()
        .find(|c| c.name == "filesystem.read")
        .ok_or_else(|| anyhow::anyhow!("filesystem.read capability not granted"))?
        .clone();
    let cap_name = cap.name.clone();

    // 2. Validate guest memory bounds — fail-closed on OOB (T-6)
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("memory export required"))?;

    // 3. Read path string from guest memory
    // Drop the memory borrow before accessing data_mut for receipt emission
    let path_result = {
        let data = memory.data(&caller);
        data.get(path_ptr as usize..(path_ptr + path_len) as usize)
            .map(|bytes| bytes.to_vec())
    };

    let path_bytes = path_result.ok_or_else(|| {
        // Guest memory OOB (S-5): emit receipt, then trap
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "read", "", 0, "trap")
            {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("path_ptr/path_len out of bounds")
    })?;
    let path = std::str::from_utf8(&path_bytes)
        .map_err(|_| anyhow::anyhow!("invalid UTF-8 in path"))?
        .trim_end_matches('\0')
        .to_string();

    // 4. REQ-103: Reject .. segments BEFORE canonicalize (T-1, S-3)
    if path.contains("..") {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "read", &path, 0, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path traversal attempt: '..' segment detected");
    }

    // 5. REQ-102: Canonicalize and verify within allowed_root (S-2)
    let full_path = cap.allowed_root.join(&path);
    let canonical_path = std::fs::canonicalize(&full_path)
        .map_err(|_| anyhow::anyhow!("path canonicalization failed"))?;
    let canonical_root = std::fs::canonicalize(&cap.allowed_root)
        .map_err(|_| anyhow::anyhow!("allowed_root canonicalization failed"))?;
    if !canonical_path.starts_with(&canonical_root) {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "read", &path, 0, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path outside allowed root");
    }

    // 6. REQ-104: stat() size check (S-5)
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|_| anyhow::anyhow!("file metadata read failed"))?;
    let file_size = metadata.len();
    if file_size > cap.max_bytes {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "read", &path, file_size, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!(
            "file size {} exceeds capability limit {}",
            file_size,
            cap.max_bytes
        );
    }

    // 7. Emit receipt for happy path (S-1) BEFORE read
    if let Some(ref emitter) = caller.data().receipt_emitter {
        if let Err(e) = emitter
            .lock()
            .unwrap()
            .emit(&cap_name, "read", &path, file_size, "success")
        {
            return Err(anyhow::anyhow!("receipt emission failed: {}", e));
        }
    }

    // 8. Read and copy to guest memory
    let contents =
        std::fs::read(&canonical_path).map_err(|_| anyhow::anyhow!("file read failed"))?;

    // Bounds check output buffer
    if contents.len() > out_len as usize {
        bail!("output buffer too small");
    }
    memory.data_mut(&mut caller)[out_ptr as usize..(out_ptr + contents.len() as i32) as usize]
        .copy_from_slice(&contents);

    Ok(contents.len() as i32) // return bytes read
}

/// Host function: atomic filesystem write within an allowed root.
///
/// Registered via `Linker::func_wrap` when `filesystem.write` capability
/// is present. Validates path against `allowed_root`, rejects `..`
/// traversal, enforces `max_bytes` on data size, writes to a temp file
/// (`.aegis_tmp`), atomically renames to target, and emits a signed
/// receipt AFTER rename succeeds (design §5.2).
///
/// **Critical invariant**: rename BEFORE success receipt emission.
/// If emit fails after rename (S-416-W-after-rename), the file IS
/// written on the host filesystem and cannot be undone. We log
/// "FAIL-CLOSED VIOLATION" and return Trap at the API level.
///
/// **Fail-closed invariant**: all violation paths produce a Trap,
/// never graceful errors (REQ-107, REQ-510).
fn aegis_fs_write(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    data_ptr: i32,
    data_len: i32,
) -> Result<i32> {
    // 1. Get capability config FIRST
    let cap = caller
        .data()
        .capabilities
        .iter()
        .find(|c| c.name == "filesystem.write")
        .ok_or_else(|| anyhow::anyhow!("filesystem.write capability not granted"))?
        .clone();
    let cap_name = cap.name.clone();

    // 2. Validate guest memory bounds for PATH (T-6-W)
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("memory export required"))?;

    // 3. Read path string from guest memory
    let path_result = {
        let data = memory.data(&caller);
        data.get(path_ptr as usize..(path_ptr + path_len) as usize)
            .map(|bytes| bytes.to_vec())
    };

    let path_bytes = path_result.ok_or_else(|| {
        // Guest memory OOB on path — emit trap receipt, then trap
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", "", 0, "trap")
            {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("path_ptr/path_len out of bounds")
    })?;
    let path = std::str::from_utf8(&path_bytes)
        .map_err(|_| anyhow::anyhow!("invalid UTF-8 in path"))?
        .trim_end_matches('\0')
        .to_string();

    // 4. REQ-503: Reject .. segments BEFORE canonicalize (S-3-W)
    if path.contains("..") {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, 0, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path traversal attempt: '..' segment detected");
    }

    // 5. REQ-502: Canonicalize and verify within allowed_root (S-2-W, Symlink-W)
    // For write, the target file may not exist yet. We check for symlinks
    // explicitly using symlink_metadata (which does NOT follow symlinks),
    // then resolve via canonicalize if the target exists, or read_link if not.
    let full_path = cap.allowed_root.join(&path);
    let canonical_path = {
        // Check if the path is a symlink — if so, resolve the target
        let symlink_meta = std::fs::symlink_metadata(&full_path);
        let is_symlink = symlink_meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        if is_symlink {
            // Symlink exists — try canonicalize (resolves to target path)
            match std::fs::canonicalize(&full_path) {
                Ok(p) => p,
                Err(_) => {
                    // Target doesn't exist — read the raw symlink target
                    let target = std::fs::read_link(&full_path)
                        .map_err(|_| anyhow::anyhow!("path canonicalization failed"))?;

                    // REQ-503: Reject symlink targets containing ".." segments — same fail-closed
                    // traversal check as guest-provided paths. A symlink pointing outside the
                    // allowed_root via ".." is a traversal attempt regardless of whether the
                    // target exists yet.
                    if target.components().any(|c| c.as_os_str() == "..") {
                        if let Some(ref emitter) = caller.data().receipt_emitter {
                            let _ = emitter
                                .lock()
                                .unwrap()
                                .emit(&cap_name, "write", &path, 0, "trap");
                        }
                        bail!("symlink target contains '..' traversal segment");
                    }

                    // Resolve relative symlink targets against the parent directory
                    if target.is_relative() {
                        let parent = full_path
                            .parent()
                            .ok_or_else(|| anyhow::anyhow!("path has no parent directory"))?;
                        parent.join(target)
                    } else {
                        target
                    }
                }
            }
        } else {
            // Not a symlink — file may not exist yet, canonicalize parent
            let parent = full_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("path has no parent directory"))?;
            let file_name = full_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("path has no file name"))?;
            let canonical_parent = std::fs::canonicalize(parent)
                .map_err(|_| anyhow::anyhow!("path canonicalization failed"))?;
            canonical_parent.join(file_name)
        }
    };
    let canonical_root = std::fs::canonicalize(&cap.allowed_root)
        .map_err(|_| anyhow::anyhow!("allowed_root canonicalization failed"))?;

    // REQ-503 / REQ-512: Defense-in-depth — reject any canonical_path that still
    // contains ".." components (ParentDir). This catches symlink targets with
    // unresolved ".." that escaped the earlier checks, and any other path
    // construction that might leave ".." unnormalized.
    if canonical_path.components().any(|c| c.as_os_str() == "..") {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            let _ = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, 0, "trap");
        }
        bail!("path contains '..' after resolution — traversal attempt");
    }

    if !canonical_path.starts_with(&canonical_root) {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, 0, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path outside allowed root");
    }

    // 6. Validate guest memory bounds for DATA (T-6-W)
    let data_result = {
        let data = memory.data(&caller);
        data.get(data_ptr as usize..(data_ptr + data_len) as usize)
            .map(|bytes| bytes.to_vec())
    };

    let write_data = data_result.ok_or_else(|| {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, 0, "trap")
            {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("data_ptr/data_len out of bounds")
    })?;

    // 7. REQ-504: Size enforcement (S-5-W)
    let data_size = write_data.len() as u64;
    if data_size > cap.max_bytes {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, data_size, "trap")
            {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!(
            "data size {} exceeds capability limit {}",
            data_size,
            cap.max_bytes
        );
    }

    // 8. Create temp file: target.aegis_tmp in allowed_root
    let temp_path = canonical_path.with_extension(
        canonical_path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
            + ".aegis_tmp",
    );
    // If no extension, use "filename.aegis_tmp"
    let temp_path = if temp_path == canonical_path {
        canonical_path.with_file_name(
            canonical_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
                + ".aegis_tmp",
        )
    } else {
        temp_path
    };

    // Write data to temp file
    std::fs::write(&temp_path, &write_data)
        .map_err(|_| anyhow::anyhow!("temp file write failed"))?;

    // 9. Atomic rename: temp → target (REQ-505, REQ-506: overwrite allowed)
    // CRITICAL: Rename BEFORE success receipt emission.
    if let Err(e) = std::fs::rename(&temp_path, &canonical_path) {
        // Rename failed — cleanup temp file, emit trap receipt, then trap
        let _ = std::fs::remove_file(&temp_path);
        if let Some(ref emitter) = caller.data().receipt_emitter {
            let _ = emitter
                .lock()
                .unwrap()
                .emit(&cap_name, "write", &path, data_size, "trap");
        }
        bail!("atomic rename failed: {}", e);
    }

    // 10. Rename succeeded — file is now visible at target path.
    // NOW emit success receipt. This is the critical asymmetry vs aegis_fs_read:
    // In read, we could trap and not return data. In write, the file is ALREADY
    // written to the host filesystem and visible to other processes. We cannot
    // "un-write" it. If emit fails HERE, we have a real write without a receipt.
    if let Some(ref emitter) = caller.data().receipt_emitter {
        if let Err(e) = emitter
            .lock()
            .unwrap()
            .emit(&cap_name, "write", &path, data_size, "success")
        {
            // S-416-W-after-rename: emit failed AFTER successful rename
            // File is already written at target — we CANNOT undo it.
            tracing::error!(
                "FAIL-CLOSED VIOLATION: filesystem.write succeeded (file at {}) but receipt emission failed: {}. \
                Host has a write with no signed receipt in chain.",
                canonical_path.display(), e
            );
            // Still return Trap to maintain fail-closed at API level
            return Err(anyhow::anyhow!(
                "receipt emission failed after successful write: {}",
                e
            ));
        }
    }

    // 11. Success — file written AND receipt emitted
    Ok(0)
}

/// Maximum response body size in bytes (REQ-608, S-605) — exactly 1 MiB.
const MAX_NETWORK_RESPONSE_BYTES: usize = 1 << 20;

/// Host function: HTTPS fetch against the configured allowlist.
///
/// Registered via `Linker::func_wrap` when the `network.http` capability is
/// present. Guest passes method + URL as `(ptr, len)` pairs. Validation order
/// (REQ-603): endpoint (S-601) → method (S-602) → rate bucket (S-606) → fetch
/// (S-603/S-604) → size cap (S-605). Every violation traps fail-closed (REQ-107)
/// with a `network.http`/`fetch` receipt (REQ-609).
///
/// **Trap-path semantics** (REQ-609): guest-memory OOB → `path=""` (fs
/// precedent); S-601..S-606 → `path=<URL>`. On success the host records
/// `FetchRecord { url, body_blake3, body_len }` (D5) — the guest is never
/// trusted to report what it fetched (REQ-610).
fn aegis_http_fetch(
    mut caller: Caller<'_, SandboxState>,
    method_ptr: i32,
    method_len: i32,
    url_ptr: i32,
    url_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> Result<i32> {
    // 1. Policy carrier (D2) — the fn is registered only when the capability
    //    is granted, so this is a defensive fail-closed guard (fs precedent).
    let params = caller
        .data()
        .network_http
        .clone()
        .ok_or_else(|| anyhow::anyhow!("network.http capability not granted"))?;

    // 2. Validate guest memory bounds for method + URL (T-6 class, no panics).
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("memory export required"))?;

    let (method_bytes, url_bytes) = {
        let data = memory.data(&caller);
        match (
            checked_guest_slice(data, method_ptr, method_len),
            checked_guest_slice(data, url_ptr, url_len),
        ) {
            (Some(method), Some(url)) => (method.to_vec(), url.to_vec()),
            // Guest memory OOB — receipt with path "" (fs precedent), then trap.
            _ => {
                emit_network_receipt(&caller, "", 0, "trap")?;
                bail!("method_ptr/method_len or url_ptr/url_len out of bounds");
            }
        }
    };
    let method = String::from_utf8_lossy(&method_bytes)
        .trim_end_matches('\0')
        .to_string();
    let url_str = String::from_utf8_lossy(&url_bytes)
        .trim_end_matches('\0')
        .to_string();

    // 3. REQ-604 / S-601: endpoint allowlist — https-only, no userinfo, domain
    //    host (no IPs/wildcards), port 443 implied, exact case-insensitive
    //    hostname in `allowed_hosts`. Parsed via `ureq::http::Uri` (no `url`
    //    dep; semantics equivalent to the `url::Url` design note).
    let uri: ureq::http::Uri = match url_str.parse() {
        Ok(uri) => uri,
        Err(_) => {
            emit_network_receipt(&caller, &url_str, 0, "trap")?;
            bail!("network endpoint not allowed: {}", url_str);
        }
    };
    if !endpoint_allowed(&uri, &params.allowed_hosts) {
        emit_network_receipt(&caller, &url_str, 0, "trap")?;
        bail!("network endpoint not allowed: {}", url_str);
    }

    // 4. REQ-605 / S-602: method allowlist (case-insensitive, normalized to
    //    uppercase for the HTTP token). An allowed-but-unparseable method token
    //    also traps S-602 (nothing sendable, fail-closed).
    if !params
        .allowed_methods
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&method))
    {
        emit_network_receipt(&caller, &url_str, 0, "trap")?;
        bail!("network method not allowed: {}", method);
    }
    let method_upper = method.to_ascii_uppercase();
    let http_method = match ureq::http::Method::from_bytes(method_upper.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            emit_network_receipt(&caller, &url_str, 0, "trap")?;
            bail!("network method not allowed: {}", method);
        }
    };

    // 5. REQ-606 / S-606: consume one token (refill-on-demand). Empty bucket
    //    traps BEFORE any network I/O.
    let bucket_empty = {
        let state = caller.data_mut();
        let bucket = state
            .network_bucket
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("network.http bucket not initialized"))?;
        !bucket.try_take()
    };
    if bucket_empty {
        emit_network_receipt(&caller, &url_str, 0, "trap")?;
        bail!("network rate limit exceeded: bucket empty");
    }

    // 6. Agent — cached per execution (D2), built lazily: TLS-only, connect ≤ 2s,
    //    total ≤ 5s (REQ-607), no redirects (a redirect could escape the host
    //    allowlist — fail-closed), 4xx/5xx bodies delivered like any response.
    if caller.data().network_agent.is_none() {
        let agent = build_network_agent(caller.data())?;
        caller.data_mut().network_agent = Some(agent);
    }
    let agent = caller
        .data()
        .network_agent
        .as_ref()
        .expect("network agent set above")
        .clone();

    // Test-only transport override (task 2.7): validation ran against the
    // original URL; only the socket destination may change.
    let fetch_url = network_transport_url(caller.data(), &url_str);

    let request = ureq::http::Request::builder()
        .method(http_method)
        .uri(fetch_url.as_str())
        .body(Vec::<u8>::new())
        .map_err(|e| anyhow::anyhow!("request construction failed: {}", e))?;

    let response = match agent.run(request) {
        Ok(response) => response,
        Err(ureq::Error::Timeout(timeout)) => {
            // S-604 (catalog `network timeout: <stage> > <n>s`): connect vs
            // total are distinguishable via ureq's timeout stage.
            let (stage, seconds) = match timeout {
                ureq::Timeout::Connect => ("connect", 2),
                _ => ("total", 5),
            };
            emit_network_receipt(&caller, &url_str, 0, "trap")?;
            bail!("network timeout: {} > {}s", stage, seconds);
        }
        Err(err) => {
            // S-603: DNS / TLS / connect failures (fail-closed).
            emit_network_receipt(&caller, &url_str, 0, "trap")?;
            bail!("network connection failed: {}", err);
        }
    };

    // 7. REQ-608 / S-605: read the body with a 1 MiB + 1 bound so oversize
    //    responses are detected (E-604: exactly 1 MiB succeeds). `take` bounds
    //    allocation — no unbounded growth from a hostile server.
    let mut body = Vec::new();
    if let Err(err) = response
        .into_body()
        .into_reader()
        .take(MAX_NETWORK_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
    {
        emit_network_receipt(&caller, &url_str, 0, "trap")?;
        bail!("network connection failed: {}", err);
    }
    if body.len() > MAX_NETWORK_RESPONSE_BYTES {
        // S-605 — receipt carries the captured (oversized) byte count.
        emit_network_receipt(&caller, &url_str, body.len() as u64, "trap")?;
        bail!(
            "network response size {} exceeds limit {}",
            body.len(),
            MAX_NETWORK_RESPONSE_BYTES
        );
    }

    // 8. Guest output buffer must fit the body (copy ≤ out_len; all arithmetic
    //    in i64 — never panic on guest-provided bounds). Checked BEFORE the
    //    success receipt so the chain never records success for an attempt
    //    that cannot deliver data (fs precedent: plain trap, no receipt).
    let out_start = i64::from(out_ptr);
    let out_end = out_start + i64::from(out_len);
    let mem_len = memory.data(&caller).len() as i64;
    if out_start < 0 || out_end < out_start || out_end > mem_len {
        bail!("output buffer too small");
    }
    if body.len() as i64 > out_end - out_start {
        bail!("output buffer too small");
    }

    // 9. REQ-610: receipt `result` = BLAKE3(body) hex. Emit BEFORE the data
    //    reaches the guest buffer — emit failure traps with no data delivered
    //    (fail-closed; the remote GET is already observed — AD-005 class).
    let body_blake3 = hex::encode(blake3::hash(&body).as_bytes());
    if let Err(e) = emit_network_receipt(&caller, &url_str, body.len() as u64, &body_blake3) {
        tracing::error!(
            "network.http fetch of {} was observed remotely but receipt emission failed: {}. \
             Host performed a fetch with no signed receipt in chain.",
            url_str,
            e
        );
        return Err(e);
    }

    // 10. Copy ≤ out_len into guest memory, then record host-authoritative
    //     fetch state (D5) — read later by the Execute handler (REQ-610).
    memory.data_mut(&mut caller)[out_start as usize..out_end as usize].copy_from_slice(&body);
    caller.data_mut().network_fetch = Some(FetchRecord {
        url: url_str,
        body_blake3,
        body_len: body.len() as u64,
    });

    Ok(body.len() as i32) // return bytes copied to guest memory
}

/// Bounds-checked guest memory slice read (no panics on guest-provided ptr/len:
/// all arithmetic happens in `i64` before casting to `usize`).
fn checked_guest_slice(data: &[u8], ptr: i32, len: i32) -> Option<&[u8]> {
    let start = i64::from(ptr);
    let end = start + i64::from(len);
    if start < 0 || end < start || end > data.len() as i64 {
        return None;
    }
    data.get(start as usize..end as usize)
}

/// REQ-604 / S-601: endpoint policy check.
///
/// Accepts only `https://`, no userinfo, a domain host (no IP literals, no
/// wildcards), port absent or 443, and an exact case-insensitive hostname
/// present in `allowed_hosts`.
fn endpoint_allowed(uri: &ureq::http::Uri, allowed_hosts: &[String]) -> bool {
    if uri.scheme_str().map(|s| s.eq_ignore_ascii_case("https")) != Some(true) {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    if authority.as_str().contains('@') {
        return false; // userinfo rejected
    }
    let host = authority.host();
    if host.is_empty() || host.contains('*') {
        return false;
    }
    if !matches!(authority.port_u16(), None | Some(443)) {
        return false;
    }
    // Defense-in-depth: config validation (E-601) already rejects IP-literal
    // hosts; reject them here too for directly-constructed params.
    let ip_candidate = host.trim_start_matches('[').trim_end_matches(']');
    if ip_candidate.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// Emit a `network.http`/`fetch` receipt (REQ-609).
///
/// Emit failures propagate so the caller traps fail-closed — no capability
/// outcome is ever left without a signed receipt in the chain.
fn emit_network_receipt(
    caller: &Caller<'_, SandboxState>,
    path: &str,
    size: u64,
    result: &str,
) -> Result<()> {
    if let Some(ref emitter) = caller.data().receipt_emitter {
        emitter
            .lock()
            .unwrap()
            .emit("network.http", "fetch", path, size, result)
            .map_err(|e| anyhow::anyhow!("receipt emission failed: {}", e))?;
    }
    Ok(())
}

/// Build the ureq agent for `network.http` (D1): TLS-only, connect ≤ 2s,
/// total ≤ 5s (REQ-607), no redirects (host-allowlist escape), no
/// status-code errors. `test-utils` additionally injects a CA PEM as the
/// TLS trust root for hermetic `https://localhost` tests.
fn build_network_agent(_state: &SandboxState) -> Result<ureq::Agent> {
    #[cfg(not(feature = "test-utils"))]
    let tls_config = network_tls_config(_state);
    #[cfg(feature = "test-utils")]
    let tls_config = network_tls_config(_state)?;

    let builder = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .max_redirects(0)
        .https_only(true)
        .tls_config(tls_config);
    Ok(ureq::Agent::new_with_config(builder.build()))
}

/// Default TLS config — platform roots via rustls.
#[cfg(not(feature = "test-utils"))]
fn network_tls_config(_state: &SandboxState) -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder().build()
}

/// Test TLS config — injects the CA PEM as the only trust root.
#[cfg(feature = "test-utils")]
fn network_tls_config(state: &SandboxState) -> Result<ureq::tls::TlsConfig> {
    let mut builder = ureq::tls::TlsConfig::builder();
    if let Some(ca_pem) = state.network_test_ca_pem.as_deref() {
        let cert = ureq::tls::Certificate::from_pem(ca_pem)
            .map_err(|e| anyhow::anyhow!("invalid test CA PEM: {}", e))?;
        builder = builder.root_certs(ureq::tls::RootCerts::new_with_certs(&[cert]));
    }
    Ok(builder.build())
}

/// Transport URL for the actual fetch (task 2.7).
///
/// Policy validation always runs against the original URL. Under `test-utils`,
/// an active port override redirects the socket while preserving the host and
/// path — URL-policy semantics are unchanged.
fn network_transport_url(_state: &SandboxState, url: &str) -> String {
    #[cfg(feature = "test-utils")]
    {
        if let Some(port) = _state.network_test_port {
            let uri: ureq::http::Uri = url.parse().expect("validated URL parses");
            let host = uri.authority().map(|a| a.host()).unwrap_or_default();
            let path_and_query = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
            return format!("https://{}:{}{}", host, port, path_and_query);
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    use wat::parse_str;

    /// Build an HTTP-fetch WAT module calling `aegis::http_fetch` once.
    ///
    /// Method data lives at 1024, URL at 2048, output buffer at 4096.
    fn http_fetch_wat(method: &str, url: &str) -> String {
        format!(
            r#"(module
  (import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{method}")
  (data (i32.const 2048) "{url}")
  (func (export "_start")
    (call $fetch (i32.const 1024) (i32.const {mlen}) (i32.const 2048) (i32.const {ulen}) (i32.const 4096) (i32.const 1024))
    drop
  )
)"#,
            method = method,
            url = url,
            mlen = method.len(),
            ulen = url.len(),
        )
    }

    /// Sandbox with a receipt emitter wired, ready for one network capability.
    fn network_sandbox(
        hosts: &[&str],
        methods: &[&str],
        rate: u64,
    ) -> (Sandbox, Capability, Ed25519KeyPair) {
        use ring::signature::Ed25519KeyPair;

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        // Re-derive from the same PKCS8 bytes so the emitter signs with the
        // exact key we verify against (ring's Ed25519KeyPair is not Clone).
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();

        let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
            .expect("Failed to create sandbox");
        let emitter = crate::receipts::ReceiptEmitter::new(
            Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
        );
        sandbox.store_mut().data_mut().receipt_emitter = Some(Arc::new(Mutex::new(emitter)));

        let cap = Capability::NetworkHttp(NetworkHttpParams {
            allowed_hosts: hosts.iter().map(|s| s.to_string()).collect(),
            allowed_methods: methods.iter().map(|s| s.to_string()).collect(),
            max_requests_per_second: rate,
        });

        (sandbox, cap, key_pair)
    }

    /// Instantiate `_start` and return the typed function.
    fn instantiate_http_start(
        sandbox: &mut Sandbox,
        cap: &Capability,
        method: &str,
        url: &str,
    ) -> wasmtime::TypedFunc<(), ()> {
        let wasm = parse_str(http_fetch_wat(method, url)).expect("WAT parse failed");
        let instance = sandbox
            .instantiate_with_capabilities(&wasm, std::slice::from_ref(cap))
            .expect("Module should instantiate");
        instance
            .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
            .expect("Function not found")
    }

    fn assert_network_trap_receipt(
        chain: &[ExecutionReceipt],
        path: &str,
        size: u64,
        pub_key: &[u8],
    ) {
        assert_eq!(chain.len(), 1, "expected exactly 1 receipt");
        let receipt = &chain[0];
        assert_eq!(receipt.capability_name, "network.http");
        assert_eq!(receipt.action, "fetch");
        assert_eq!(receipt.path, path);
        assert_eq!(receipt.size, size);
        assert_eq!(receipt.result, "trap");
        let valid = receipt
            .verify_signature(pub_key)
            .expect("signature verification must not error");
        assert!(valid, "receipt signature must verify");
    }

    /// wasmtime 24 wraps host errors in a backtrace error; the custom message
    /// lives in the "Caused by:" chain, not in `to_string()`.
    fn error_chain_contains(err: &wasmtime::Error, needle: &str) -> bool {
        if err.to_string().contains(needle) {
            return true;
        }
        let mut source: Option<&dyn std::error::Error> = err.source();
        while let Some(s) = source {
            if s.to_string().contains(needle) {
                return true;
            }
            source = s.source();
        }
        format!("{:?}", err).contains(needle)
    }

    fn assert_network_trap(err: &wasmtime::Error, needle: &str) {
        assert!(
            error_chain_contains(err, needle),
            "trap message must contain {:?}, got: {:?}",
            needle,
            err
        );
    }

    // ═══ TokenBucket (D3, REQ-606) ═══

    #[test]
    fn token_bucket_allows_burst_then_exhausts() {
        let mut bucket = TokenBucket::new(2);
        assert!(bucket.try_take());
        assert!(bucket.try_take());
        assert!(
            !bucket.try_take(),
            "third take with empty bucket must fail (E-605)"
        );
    }

    #[test]
    fn token_bucket_refills_on_demand() {
        let mut bucket = TokenBucket::new(2);
        assert!(bucket.try_take());
        assert!(bucket.try_take());
        assert!(!bucket.try_take());

        // Simulate 1s passing: refill restores capacity, capped at 2.
        bucket.last_refill = Instant::now() - Duration::from_secs(1);
        assert!(bucket.try_take());
        assert!(bucket.try_take());
        assert!(!bucket.try_take(), "refill must not accrue beyond capacity");
    }

    #[test]
    fn token_bucket_caps_at_capacity() {
        let mut bucket = TokenBucket::new(2);
        bucket.last_refill = Instant::now() - Duration::from_secs(60);
        bucket.refill();
        assert_eq!(bucket.tokens, 2.0, "refill must cap at capacity");
    }

    #[test]
    fn token_bucket_zero_rate_is_fail_closed() {
        let mut bucket = TokenBucket::new(0);
        assert!(
            !bucket.try_take(),
            "zero-rate bucket must never grant a token"
        );
    }

    // ═══ Validation stage (2.3) — S-601 / S-602 / S-606, no network ═══

    #[test]
    fn fetch_endpoint_not_allowed_traps_s601() {
        let (mut sandbox, cap, key_pair) = network_sandbox(&["example.com"], &["GET"], 10);
        let url = "https://evil.com/x";
        let pub_key = key_pair.public_key().as_ref().to_vec();

        let func = instantiate_http_start(&mut sandbox, &cap, "GET", url);
        let err = func
            .call(sandbox.store_mut(), ())
            .expect_err("S-601 must trap");
        assert_network_trap(&err, &format!("network endpoint not allowed: {}", url));

        assert_network_trap_receipt(&sandbox.get_receipt_chain(), url, 0, &pub_key);
    }

    #[test]
    fn fetch_hostname_case_insensitive_passes_endpoint_validation() {
        // E-602 semantics: `https://LOCALHOST/x` must PASS endpoint validation
        // against allowlist `localhost`; the fetch then fails at the transport
        // (nothing listens on :443) — S-603/S-604, NOT S-601.
        let (mut sandbox, cap, _key_pair) = network_sandbox(&["localhost"], &["GET"], 1);

        let func = instantiate_http_start(&mut sandbox, &cap, "GET", "https://LOCALHOST/x");
        let err = func
            .call(sandbox.store_mut(), ())
            .expect_err("transport stage must trap");
        assert!(
            !error_chain_contains(&err, "network endpoint not allowed"),
            "endpoint validation must accept case-insensitive host: {:?}",
            err
        );
        assert!(
            !error_chain_contains(&err, "network method not allowed"),
            "method validation must pass: {:?}",
            err
        );
    }

    #[test]
    fn fetch_method_not_allowed_traps_s602() {
        let (mut sandbox, cap, key_pair) = network_sandbox(&["example.com"], &["GET"], 10);
        let url = "https://example.com/x";
        let pub_key = key_pair.public_key().as_ref().to_vec();

        let func = instantiate_http_start(&mut sandbox, &cap, "POST", url);
        let err = func
            .call(sandbox.store_mut(), ())
            .expect_err("S-602 must trap");
        assert_network_trap(&err, "network method not allowed: POST");

        assert_network_trap_receipt(&sandbox.get_receipt_chain(), url, 0, &pub_key);
    }

    #[test]
    fn fetch_rate_limit_exhausted_traps_s606() {
        // Bucket = 1 (rate 1). First call passes validation and drains the
        // token, then fails at the transport (S-603 — nothing listens on
        // localhost:443). Second call traps S-606 BEFORE any network I/O.
        let (mut sandbox, cap, key_pair) = network_sandbox(&["localhost"], &["GET"], 1);
        let url = "https://localhost/x";
        let pub_key = key_pair.public_key().as_ref().to_vec();

        let func = instantiate_http_start(&mut sandbox, &cap, "GET", url);

        let r1_err = func
            .call(sandbox.store_mut(), ())
            .expect_err("first call must trap");
        assert!(
            !error_chain_contains(&r1_err, "network rate limit exceeded"),
            "first call must fail at the transport stage, got: {:?}",
            r1_err
        );

        let r2_err = func
            .call(sandbox.store_mut(), ())
            .expect_err("S-606 must trap");
        assert_network_trap(&r2_err, "network rate limit exceeded: bucket empty");

        let chain = sandbox.get_receipt_chain();
        assert_eq!(chain.len(), 2, "one trap receipt per attempt (REQ-609)");
        assert_eq!(chain[1].capability_name, "network.http");
        assert_eq!(chain[1].action, "fetch");
        assert_eq!(chain[1].path, url);
        assert_eq!(chain[1].result, "trap");
        let valid = chain[1]
            .verify_signature(&pub_key)
            .expect("signature verification must not error");
        assert!(valid, "receipt signature must verify");
    }

    #[test]
    fn fetch_guest_memory_oob_emits_empty_path_receipt() {
        // URL ptr beyond the 64 KiB memory → OOB → receipt path "" then trap.
        let (mut sandbox, cap, key_pair) = network_sandbox(&["example.com"], &["GET"], 10);
        let pub_key = key_pair.public_key().as_ref().to_vec();

        let wasm = parse_str(
            r#"(module
  (import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "GET")
  (func (export "_start")
    (call $fetch (i32.const 1024) (i32.const 3) (i32.const 100000) (i32.const 18) (i32.const 4096) (i32.const 1024))
    drop
  )
)"#,
        )
        .expect("WAT parse failed");
        let instance = sandbox
            .instantiate_with_capabilities(&wasm, &[cap])
            .expect("Module should instantiate");
        let func = instance
            .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
            .expect("Function not found");

        let err = func
            .call(sandbox.store_mut(), ())
            .expect_err("OOB must trap");
        assert_network_trap(&err, "out of bounds");

        // Trap-path semantics: OOB → path "" (fs precedent).
        assert_network_trap_receipt(&sandbox.get_receipt_chain(), "", 0, &pub_key);
    }
}
