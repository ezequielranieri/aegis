//! Wasmtime-based sandboxing module
//!
//! Provides secure WASM execution with epoch-based CPU interruption
//! and configurable resource limits via `StoreLimitsBuilder`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Result};
use ring::signature::Ed25519KeyPair;
use wasmtime::*;

use crate::capabilities::Capability;
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
                    // Phase 3: register network host functions
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "read", "", 0, "trap") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "read", &path, 0, "trap") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "read", &path, 0, "trap") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "read", &path, file_size, "trap") {
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
        if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "read", &path, file_size, "success") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", "", 0, "trap") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", &path, 0, "trap") {
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
                            let _ = emitter.lock().unwrap().emit(&cap_name, "write", &path, 0, "trap");
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
            let _ = emitter.lock().unwrap().emit(&cap_name, "write", &path, 0, "trap");
        }
        bail!("path contains '..' after resolution — traversal attempt");
    }

    if !canonical_path.starts_with(&canonical_root) {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", &path, 0, "trap") {
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
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", &path, 0, "trap") {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("data_ptr/data_len out of bounds")
    })?;

    // 7. REQ-504: Size enforcement (S-5-W)
    let data_size = write_data.len() as u64;
    if data_size > cap.max_bytes {
        if let Some(ref emitter) = caller.data().receipt_emitter {
            if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", &path, data_size, "trap") {
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
            let _ = emitter.lock().unwrap().emit(&cap_name, "write", &path, data_size, "trap");
        }
        bail!("atomic rename failed: {}", e);
    }

    // 10. Rename succeeded — file is now visible at target path.
    // NOW emit success receipt. This is the critical asymmetry vs aegis_fs_read:
    // In read, we could trap and not return data. In write, the file is ALREADY
    // written to the host filesystem and visible to other processes. We cannot
    // "un-write" it. If emit fails HERE, we have a real write without a receipt.
    if let Some(ref emitter) = caller.data().receipt_emitter {
        if let Err(e) = emitter.lock().unwrap().emit(&cap_name, "write", &path, data_size, "success") {
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
