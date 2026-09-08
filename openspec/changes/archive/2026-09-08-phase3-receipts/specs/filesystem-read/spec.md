# Delta: filesystem-read — Phase 3 Mature Receipts

## MODIFIED Requirements

### REQ-108 (Modified from Phase 1 traceability)
Receipt stub logs per call — replaced by signed receipt emission. `aegis_fs_read` now emits cryptographically signed `ExecutionReceipt` for every invocation via `ReceiptEmitter::emit()` accessed through `caller.data_mut()`.

### REQ-440: Receipt Emission via caller.data_mut()
System SHALL access `ReceiptEmitter` via `caller.data_mut()` instead of `emit_capability_event`. The `aegis_fs_read` host function calls `caller.data_mut().receipt_emitter.as_mut()?.emit(capability, action, path, size, result)` at all validation points.

### REQ-441: Receipt Emission for All 5 Scenarios
Receipt emission SHALL occur for all 5 scenarios: S-1 (happy), S-2 (denied), S-3 (traversal), S-4 (fd leak — N/A, no receipt), S-5 (size exceeded). S-4 excluded — module fails at linking before host function runs.

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

### REQ-433: Fail-Closed Signing Failure
Signing failure SHALL propagate as error. If `emit()` returns `Err`, `aegis_fs_read` MUST immediately return `Trap` with the error, discarding original operation result. Uniform across all 5 scenarios.

## ADDED Requirements

### REQ-430: ReceiptEmitter in SandboxState
System SHALL store `ReceiptEmitter` in `SandboxState`.

### REQ-431: ReceiptEmitter Holds Key Pair + Chain
`ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain`.

### REQ-432: emit() Creates Signed Receipt
`emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` SHALL create signed receipt.

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
Platform scope: `aegis` currently targets Unix-like platforms only. Key file `0600` permission check uses `std::os::unix::fs::PermissionsExt`. On non-Unix platforms (e.g., Windows), `Sandbox::new` SHALL fail immediately with `ConfigError::Io` ("unsupported platform").

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-400 | Valid receipt creation | Ed25519 key loaded, chain initialized | `emit()` called with valid inputs | Receipt returned with valid signature, correct `prev_hash`, monotonic timestamp |
| S-401 | Hash chain integrity | Two receipts emitted sequentially | Verifier checks chain | Second receipt's `prev_hash` matches `blake3` of first receipt; chain validates |
| S-402 | Key file 0600 enforced | Key file at `key_path` with `0600` perms | Sandbox created | Sandbox creation succeeds, `ReceiptEmitter` initialized |
| S-403 | Key file 0644 rejected | Key file at `key_path` with `0644` perms | Sandbox created | Sandbox creation fails with `ConfigError::Io` |
| S-404 | Key file missing | No file at `key_path` | Sandbox created | Sandbox creation fails with `ConfigError::MissingField` or `ConfigError::Io` |
| S-405 | Invalid key format | Key file present but malformed base64 | Sandbox created | Sandbox creation fails with `ConfigError` |
| S-415 | Windows/Unix platform scope | Non-Unix platform (Windows) | Sandbox created | Sandbox creation fails with `ConfigError::Io` ("unsupported platform") |
| S-406 | Receipt for S-1 | File within `allowed_root`, size ≤ limit | Module calls `aegis::fs_read` | Receipt emitted with `result = "success"`, file contents returned |
| S-407 | Receipt for S-2 | Path resolves outside `allowed_root` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-408 | Receipt for S-3 | Path contains `../../etc/passwd` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-409 | Receipt for S-5 | File size > `max_read_bytes` | Module calls `aegis::fs_read` | Receipt emitted with `result = "trap"`, then trap raised |
| S-410 | Verifier detects tampered payload | Valid chain with one modified receipt | `verify_chain()` called | Verification fails: signature mismatch |
| S-411 | Verifier detects broken chain | Valid chain with one `prev_hash` altered | `verify_chain()` called | Verification fails: hash chain mismatch |
| S-412 | Verifier detects expired timestamp | Receipt with `timestamp_ns` > 5s old | `verify_chain()` called | Verification fails: timestamp outside window |
| S-413 | Verifier detects wrong public key | Valid chain signed with key A | `verify_chain()` called with key B | Verification fails: signature mismatch |
| S-414 | Deterministic serialization | Same `ExecutionReceipt` serialized twice | Bytes compared | Both serializations produce identical byte sequences |
| S-416 | Signing failure on happy path | Force signing error (corrupt key) on S-1 call | `aegis_fs_read` called | `aegis_fs_read` returns `Trap`, no data returned to caller |

## RENAMED Requirements

No requirements renamed.

## REMOVED Requirements

No requirements removed.
