# Filesystem Read-Only Specification (Phase 1)

## Purpose

Custom host function `aegis::fs_read` enabling WASM modules to read files from a single fixed root directory. Runtime decides access — not WASI. Any violation (path escape, traversal, size exceed, fd leak) = trap, never graceful error.

## Delta from Phase 0

`sandbox-init` spec is unchanged. Phase 1 adds new `filesystem-read` domain. `Sandbox::instantiate_with_capabilities()` gains `&[Capability]` typed enum parameter. Phase 2 adds typed enum `Capability`, `src/config/` module with `PolicyConfig`, `CapabilityDef`, `ConfigError`, and `Sandbox::from_config()`. No WASI.

## Requirements

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-101 | System SHALL provide `aegis::fs_read(path_ptr, path_len, out_ptr, out_len) -> i32` host function via `Linker::func_wrap` when `filesystem.read` capability is present | MUST |
| REQ-102 | System SHALL validate path against `allowed_root` (canonicalized, absolute) and TRAP on any path outside root | MUST |
| REQ-103 | System SHALL reject paths containing `..` segments — TRAP on traversal attempt | MUST |
| REQ-104 | System SHALL enforce `max_read_bytes` via `stat()` before read — TRAP if file size exceeds limit | MUST |
| REQ-105 | System SHALL NOT maintain an fd table — all reads go through `aegis::fs_read`; any raw fd usage = trap | MUST |
| REQ-106 | System SHALL accept `&[Capability]` (typed enum) in `Sandbox::instantiate_with_capabilities()` and register host functions per variant using `capability_name()` | MUST |
| REQ-107 | All violation paths (REQ-102–105) SHALL trap with descriptive message, never return error values | MUST |
| REQ-201 | System SHALL define `Capability` as a Rust enum with variants `FilesystemRead(FilesystemReadParams)`, `FilesystemWrite(FilesystemWriteParams)`, `NetworkHttp(NetworkHttpParams)` | MUST |
| REQ-202 | System SHALL define `FilesystemReadParams`, `FilesystemWriteParams`, `NetworkHttpParams` structs with typed fields | MUST |
| REQ-203 | System SHALL provide `capability_name(&self) -> &'static str` method on `Capability` returning the variant name | MUST |
| REQ-204 | System SHALL update `CapabilityConfig::from(&Capability)` to `match` on enum variants instead of parsing `HashMap<String, Value>` | MUST |
| REQ-205 | System SHALL update `Sandbox::instantiate_with_capabilities(&[Capability])` to iterate enum variants and register host functions per variant using `capability_name()` | MUST |
| REQ-206 | **REGRESSION GATE**: All 18 Phase 1 tests MUST pass after the typed enum refactor — zero test failures permitted | MUST |

### REQ-101: Host Function Signature

The host function SHALL be registered via `Linker::func_wrap::<SandboxState, (i32, i32, i32, i32), i32>`:

| Param | Type | Purpose |
|-------|------|---------|
| `path_ptr` | i32 | Guest memory pointer to path string |
| `path_len` | i32 | Length of path string |
| `out_ptr` | i32 | Guest memory pointer for output buffer |
| `out_len` | i32 | Capacity of output buffer |
| Return | i32 | 0 = success, trap on error |

### REQ-102: Path Validation

System SHALL canonicalize both the guest-provided path and `allowed_root`, then verify the resolved path starts with the resolved `allowed_root`.

### REQ-103: Traversal Rejection

System SHALL reject any path containing `..` segments BEFORE canonicalization. Canonicalize must not resolve `..` out of the root.

### REQ-104: Size Enforcement

System SHALL call `std::fs::metadata(path)` before reading. If `len > max_read_bytes`: trap. Default `max_read_bytes` = 1,048,576 (1 MB).

### REQ-105: No FD Table

System SHALL NOT expose file descriptors to WASM modules. The host function reads files internally. There is no fd namespace, no fd passing, no fd inheritance. Attempting to use a raw fd number traps.

### REQ-106: Capability-Driven Registration (Updated in Phase 2)

`Sandbox::instantiate_with_capabilities(wasm_bytes, capabilities)` SHALL iterate `&[Capability]` typed enum. For each `Capability::FilesystemRead(params)`, register `aegis::fs_read` using `capability_name()` and `params.allowed_root`/`params.max_read_bytes`. Phase 2 changed `Capability` from `{ name: String, params: HashMap<String, Value> }` to a typed enum.

### REQ-107: Fail-Closed Violations

Every access control and resource violation MUST trap with `Trap::new(message)`. The system SHALL NOT return error codes, errno values, or graceful failure indicators.

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-1 | Allowed read | File within `allowed_root`, size ≤ `max_read_bytes` | Module calls `aegis::fs_read` | Returns file contents, no trap |
| S-2 | Denied read | Path resolves outside `allowed_root` | Module calls `aegis::fs_read` | Trap raised |
| S-3 | Path traversal | Guest path contains `../../etc/passwd` | Module calls `aegis::fs_read` | Trap raised (before canonicalize) |
| S-4 | FD leak | Module attempts raw fd read (no fd table exists) | Module calls raw fd op | Trap raised |
| S-5 | Size exceeded | File size > `max_read_bytes` | Module calls `aegis::fs_read` | Trap raised |

### Phase 2 Scenarios (SA-xxx)

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| SA-201 | Enum construction | Code constructs `Capability::FilesystemRead(params)` | Variant is created | `capability_name()` returns `"filesystem.read"` |
| SA-202 | CapabilityConfig from enum | `Capability::FilesystemRead(FilesystemReadParams { allowed_root: "/data", max_read_bytes: 1024 })` | `CapabilityConfig::from(&cap)` called | Config has correct `name`, `allowed_root`, `max_read_bytes` |
| SA-203 | Sandbox instantiation with enum | `instantiate_with_capabilities(wasm, &[cap])` | Sandbox created | Host function `aegis::fs_read` registered, 18 Phase 1 tests pass |

## Public API

| Type | Signature | Purpose |
|------|-----------|---------|
| `Sandbox` | `instantiate_with_capabilities(wasm_bytes: &[u8], capabilities: &[Capability]) -> Result<Instance>` | Register host functions per enum variant using `capability_name()`, compile + instantiate |
| `Sandbox` | `from_config(config_path: &str) -> Result<Self>` | Load TOML config, validate, create sandbox — fail-closed |
| `Capability` | `enum Capability { FilesystemRead(FilesystemReadParams), FilesystemWrite(FilesystemWriteParams), NetworkHttp(NetworkHttpParams) }` | Typed enum — Phase 2 refactor from `{ name: String, params: HashMap<String, Value> }` |

## Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Security | Fail-closed: all violations trap, never graceful |
| TOCTOU | Race between `stat()` and `read()` documented as known limitation — single-threaded Phase 1 scope; Phase 2/3 addresses with `openat` + fd passing |
| Performance | `stat()` before read adds one syscall overhead — acceptable for Phase 1 |
| Determinism | Same path + same limits = same result, every run |
| Cleanup | No leaked fds or threads after `fs_read` returns |

## Traceability

| Requirement | Proposal Acceptance Criteria |
|-------------|------------------------------|
| REQ-101, REQ-106 | AC-1: Allowed read succeeds |
| REQ-102, REQ-107 | AC-2: Denied read traps |
| REQ-103, REQ-107 | AC-3: Path traversal traps |
| REQ-105, REQ-107 | AC-4: FD leak traps |
| REQ-104, REQ-107 | AC-5: Size exceeded traps |
| REQ-108 | Receipt stub logs per call — moved to Phase 3 |

## Phase 3: Mature Receipts — Delta

### REQ-400: ExecutionReceipt Structure
System SHALL define `ExecutionReceipt` with fields: `capability_name`, `action`, `result`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`.

### REQ-401: Canonical JSON Serialization
System SHALL serialize receipts via canonical JSON (`serde_json`) using struct field declaration order — NO `serde_cbor`.

### REQ-402: Reject Missing Fields
System SHALL reject any receipt missing required fields.

### REQ-403: Deterministic Serialization
System SHALL produce identical byte output when serializing the same `ExecutionReceipt` twice.

### REQ-410: BLAKE3 Hash Chain
System SHALL use BLAKE3 (`blake3` crate) for content hashing — no SHA-256.

### REQ-411: prev_hash Linkage
Each receipt's `prev_hash` SHALL equal `blake3(prev_hash ‖ canonical_bytes)`.

### REQ-412: Genesis prev_hash
Genesis receipt SHALL have `prev_hash = [0u8; 32]`.

### REQ-420: Load Ed25519KeyPair from key_path
System SHALL load `Ed25519KeyPair` from `PolicyConfig.receipts.key_path` at sandbox creation.

### REQ-421: ReceiptsConfig in PolicyConfig
`PolicyConfig` SHALL include optional `[receipts]` section with `key_path` field.

### REQ-422: Reuse Phase 2 Fallback Chain
System SHALL reuse Phase 2 fallback chain + `ConfigError` fail-closed for key loading.

### REQ-423: Key File 0600 Permissions
Key file MUST have `0600` permissions — sandbox creation SHALL fail with `ConfigError::Io` otherwise.

### REQ-424: No Auto-Generation
System SHALL NOT auto-generate keys — fail-closed if key file missing or invalid.

### REQ-425: Key Persistence
Key SHALL persist across restarts for verifiable runtime identity.

### REQ-426: Windows Unsupported
Platform scope: `aegis` currently targets Unix-like platforms only. Key file `0600` permission check uses `std::os::unix::fs::PermissionsExt`. On non-Unix platforms (e.g., Windows), `Sandbox::new` SHALL fail immediately with `ConfigError::Io` ("unsupported platform") — no permission check is attempted, no warning log is emitted.

### REQ-430: ReceiptEmitter in SandboxState
System SHALL store `ReceiptEmitter` in `SandboxState`.

### REQ-431: ReceiptEmitter Holds Key Pair + Chain
`ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain`.

### REQ-432: emit() Creates Signed Receipt
`emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` SHALL create signed receipt.

### REQ-433: Fail-Closed Signing Failure
Signing failure SHALL propagate as error, never produce unsigned receipt. If `emit()` returns `Err`, the calling host function MUST immediately return `Trap` with the error, discarding original operation result. Uniform across all 5 scenarios.

### REQ-440: Receipt Emission via caller.data_mut()
System SHALL access `ReceiptEmitter` via `caller.data_mut()` instead of `emit_capability_event`. The `aegis_fs_read` host function calls `caller.data_mut().receipt_emitter.as_mut()?.emit(capability, action, path, size, result)` at all validation points.

### REQ-441: Receipt Emission for All 5 Scenarios
Receipt emission SHALL occur for all 5 scenarios: S-1 (happy), S-2 (denied), S-3 (traversal), S-4 (fd leak — N/A, no receipt), S-5 (size exceeded). S-4 excluded — module fails at linking before host function runs.

### REQ-450: ReceiptChain::verify_chain Library Function
`ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` SHALL be a library function.

### REQ-451: Ed25519 Signature Validation
Verifier SHALL validate Ed25519 signature over canonical bytes.

### REQ-452: Hash Chain Integrity Validation
Verifier SHALL validate hash chain integrity (`prev_hash` linkage).

### REQ-453: Timestamp Window Validation
Verifier SHALL reject timestamps outside ±5s window from current time.

### REQ-454: Optional CLI Verifier
Optional CLI verifier (`aegis-verify` binary) MAY wrap `verify_chain`.

### REQ-460: S-1 Happy Path Receipt
`aegis_fs_read` SHALL emit mature receipt for S-1 (happy path) with `result = "success"`.

### REQ-461: S-2 Path Traversal Receipt
`aegis_fs_read` SHALL emit mature receipt for S-2 (path traversal) with `result = "trap"`.

### REQ-462: S-3 Size Exceeded Receipt
`aegis_fs_read` SHALL emit mature receipt for S-3 (size exceeded) with `result = "trap"`.

### REQ-463: S-5 Guest OOB Receipt
`aegis_fs_read` SHALL emit mature receipt for S-5 (WASI unknown import) with `result = "trap"`.

### REQ-464: Regression Gate — 5 Phase 1 Receipt Tests
All 5 Phase 1 receipt tests (S-1 through S-5) MUST pass after replacement. Tests verify `ExecutionReceipt` fields (capability_name, action, result, path, size, prev_hash, signature, timestamp_ns) with Ed25519 signature verification. S-4 is a pure regression (0 receipts).

## Phase 3 Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-400 | Valid receipt creation | Ed25519 key loaded, chain initialized | `emit()` called with valid inputs | Receipt returned with valid signature, correct `prev_hash`, monotonic timestamp |
| S-401 | Hash chain integrity | Two receipts emitted sequentially | Verifier checks chain | Second receipt's `prev_hash` matches `blake3` of first receipt; chain validates |
| S-402 | Key file 0600 enforced | Key file at `key_path` with `0600` perms | Sandbox created | Sandbox creation succeeds, `ReceiptEmitter` initialized |
| S-403 | Key file 0644 rejected | Key file at `key_path` with `0644` perms | Sandbox created | Sandbox creation fails with `ConfigError::Io` |
| S-404 | Key file missing | No file at `key_path` | Sandbox created | Sandbox creation fails with `ConfigError::MissingField` or `ConfigError::Io` |
| S-405 | Invalid key format | Key file present but malformed base64 | Sandbox created | Sandbox creation fails with `ConfigError` |
| S-406 | Receipt for S-1 | File within `allowed_root`, size ≤ limit | Module calls `aegis::fs_read` | Receipt emitted with `result = "success"`, file contents returned |
| S-407 | Receipt for S-2 | Path resolves outside `allowed_root` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-408 | Receipt for S-3 | Path contains `../../etc/passwd` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-409 | Receipt for S-5 | File size > `max_read_bytes` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-410 | Verifier detects tampered payload | Valid chain with one modified receipt | `verify_chain()` called | Verification fails: signature mismatch |
| S-411 | Verifier detects broken chain | Valid chain with one `prev_hash` altered | `verify_chain()` called | Verification fails: hash chain mismatch |
| S-412 | Verifier detects expired timestamp | Receipt with `timestamp_ns` > 5s old | `verify_chain()` called | Verification fails: timestamp outside window |
| S-413 | Verifier detects wrong public key | Valid chain signed with key A | `verify_chain()` called with key B | Verification fails: signature mismatch |
| S-414 | Deterministic serialization | Same `ExecutionReceipt` serialized twice | Bytes compared | Both serializations produce identical byte sequences |
| S-415 | Windows/Unix platform scope | Non-Unix platform (Windows) | Sandbox created | Sandbox creation fails with `ConfigError::Io` ("unsupported platform") |
| S-416 | Signing failure on happy path | Force signing error (corrupt key) on S-1 call | `aegis_fs_read` called | `aegis_fs_read` returns `Trap`, no data returned to caller |

## Phase 3 Public API

| Type | Signature | Purpose |
|------|-----------|---------|
| `ExecutionReceipt` | `ExecutionReceipt::new(capability, action, path, size, result, prev_hash, key_pair) -> Result<Self>` | Create signed receipt |
| `ReceiptEmitter` | `ReceiptEmitter::new(key_pair) -> Self` | Create emitter with key pair and chain |
| `ReceiptEmitter` | `emit(&mut self, capability, action, path, size, result) -> Result<ExecutionReceipt>` | Emit signed receipt |
| `ReceiptChain` | `verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` | Verify chain integrity |
| `Sandbox` | `get_receipt_chain() -> &ReceiptChain` | Access receipt chain for verification |
| `PolicyConfig` | `receipts: Option<ReceiptsConfig>` | Extended config with key_path |

## Phase 3 Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Determinism | Receipt is deterministic given a fixed timestamp; nanosecond timestamp is the only variation source |
| Security | No secrets in receipt payload; capabilities are identifiers only |
| Audit | Hash chain enables tamper-evident audit trail for all sandbox executions |
| Testing | Reproducibility achieved by freezing clock in tests |
| Fail-Closed | Signing failure = Trap; missing/invalid key = ConfigError; no auto-generation |

## Phase 3 Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-400–403 | S-400, S-414 |
| REQ-410–412 | S-401 |
| REQ-420–426 | S-402, S-403, S-404, S-405, S-415 |
| REQ-430–433 | S-400, S-416 |
| REQ-440–441 | S-406, S-407, S-408, S-409 |
| REQ-450–454 | S-410, S-411, S-412, S-413 |
| REQ-460–464 | S-406, S-407, S-408, S-409, S-416 |
