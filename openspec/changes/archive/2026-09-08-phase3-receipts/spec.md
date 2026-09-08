# Phase 3 — Mature Receipts Specification

## Purpose

Replace the `emit_capability_event` tracing stub with cryptographically signed execution receipts. Each capability invocation produces a BLAKE3-hash-chained, Ed25519-signed receipt. An external verifier validates chain integrity without the runtime.

## Requirements

### Receipt Structure

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-400 | System SHALL define `ExecutionReceipt` with fields: `capability_name`, `action`, `result`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64` | MUST |
| REQ-401 | System SHALL serialize receipts via canonical JSON (`serde_json`) using struct field declaration order — NO `serde_cbor` | MUST |
| REQ-402 | System SHALL reject any receipt missing required fields | MUST |
| REQ-403 | System SHALL produce identical byte output when serializing the same `ExecutionReceipt` twice (deterministic canonical JSON) | MUST |

### Hash Chain

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-410 | System SHALL use BLAKE3 (`blake3` crate) for content hashing — no SHA-256 | MUST |
| REQ-411 | Each receipt's `prev_hash` SHALL equal `blake3(prev_hash ‖ canonical_bytes)` | MUST |
| REQ-412 | Genesis receipt SHALL have `prev_hash = [0u8; 32]` | MUST |

### Key Management

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-420 | System SHALL load `Ed25519KeyPair` from `PolicyConfig.receipts.key_path` at sandbox creation | MUST |
| REQ-421 | `PolicyConfig` SHALL include optional `[receipts]` section with `key_path` field | MUST |
| REQ-422 | System SHALL reuse Phase 2 fallback chain + `ConfigError` fail-closed for key loading | MUST |
| REQ-423 | Key file MUST have `0600` permissions — sandbox creation SHALL fail with `ConfigError::Io` otherwise | MUST |
| REQ-424 | System SHALL NOT auto-generate keys — fail-closed if key file missing or invalid | MUST |
| REQ-425 | Key SHALL persist across restarts for verifiable runtime identity | MUST |
| REQ-426 | **Platform scope**: `aegis` currently targets Unix-like platforms only (Linux, macOS). Key file `0600` permission check uses `std::os::unix::fs::PermissionsExt`. On non-Unix platforms (e.g., Windows), `Sandbox::new` SHALL fail immediately with `ConfigError::Io` ("unsupported platform") — no permission check is attempted, no warning log is emitted. | MUST |

### ReceiptEmitter

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-430 | System SHALL store `ReceiptEmitter` in `SandboxState` | MUST |
| REQ-431 | `ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain` | MUST |
| REQ-432 | `emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` SHALL create signed receipt | MUST |
| REQ-433 | Signing failure SHALL propagate as error, never produce unsigned receipt | MUST |

### emit_capability_event Replacement

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-440 | `aegis_fs_read` SHALL access `ReceiptEmitter` via `caller.data_mut()` instead of `emit_capability_event` | MUST |
| REQ-441 | Receipt emission SHALL occur for all 5 scenarios: S-1 (happy), S-2 (denied), S-3 (traversal), S-4 (fd leak — N/A, no receipt), S-5 (size exceeded) | MUST |

**Note on S-4**: The fd leak scenario (REQ-105) traps before any host function executes, so no receipt is emitted. S-4 is excluded from receipt emission.

### Verification

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-450 | `ReceiptChain::verify_chain(receipts, public_key) -> Result<()>` SHALL be a library function | MUST |
| REQ-451 | Verifier SHALL validate Ed25519 signature over canonical bytes | MUST |
| REQ-452 | Verifier SHALL validate hash chain integrity (`prev_hash` linkage) | MUST |
| REQ-453 | Verifier SHALL reject timestamps outside ±5s window from current time | MUST |
| REQ-454 | Optional CLI verifier (`aegis-verify` binary) MAY wrap `verify_chain` | MAY |

### Integration (filesystem-read modified)

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-460 | `aegis_fs_read` SHALL emit mature receipt for S-1 (happy path) with `result = "success"` | MUST |
| REQ-461 | `aegis_fs_read` SHALL emit mature receipt for S-2 (path traversal) with `result = "trap"` | MUST |
| REQ-462 | `aegis_fs_read` SHALL emit mature receipt for S-3 (size exceeded) with `result = "trap"` | MUST |
| REQ-463 | `aegis_fs_read` SHALL emit mature receipt for S-5 (WASI unknown import) with `result = "trap"` | MUST |
| REQ-464 | **REGRESSION GATE**: All 5 Phase 1 receipt tests (S-1 through S-5) MUST pass after replacement | MUST |

## Scenarios

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

## Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Determinism | Receipt is deterministic given a fixed timestamp; nanosecond timestamp is the only variation source |
| Security | No secrets in receipt payload; capabilities are identifiers only |
| Audit | Hash chain enables tamper-evident audit trail for all sandbox executions |
| Testing | Reproducibility achieved by freezing clock in tests |

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-400–403 | S-400, S-414 |
| REQ-410–412 | S-401 |
| REQ-420–426 | S-402, S-403, S-404, S-405, S-415 |
| REQ-430–433 | S-400 |
| REQ-440–441 | S-406, S-407, S-408, S-409 |
| REQ-450–454 | S-410, S-411, S-412, S-413 |
| REQ-460–464 | S-406, S-407, S-408, S-409 |
