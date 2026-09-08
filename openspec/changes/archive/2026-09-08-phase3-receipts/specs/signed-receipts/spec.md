# Delta: signed-receipts — Phase 3 Mature Receipts

## ADDED Requirements

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

### REQ-430: ReceiptEmitter in SandboxState
System SHALL store `ReceiptEmitter` in `SandboxState`.

### REQ-431: ReceiptEmitter Holds Key Pair + Chain
`ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain`.

### REQ-432: emit() Creates Signed Receipt
`emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` SHALL create signed receipt.

### REQ-433: Fail-Closed Signing Failure
Signing failure SHALL propagate as error, never produce unsigned receipt. If `emit()` returns `Err`, the calling host function MUST immediately return `Trap`.

### REQ-450: ReceiptChain::verify_chain Library Function
`ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` SHALL be a library function.

### REQ-451: Ed25519 Signature Validation
Verifier SHALL validate Ed25519 signature over canonical bytes.

### REQ-452: Hash Chain Integrity Validation
Verifier SHALL validate hash chain integrity (`prev_hash` linkage).

### REQ-453: Timestamp Window Validation
Verifier SHALL reject timestamps outside ±5s window from current time.

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
Platform scope: `aegis` currently targets Unix-like platforms only. On non-Unix platforms (e.g., Windows), `Sandbox::new` SHALL fail immediately with `ConfigError::Io` ("unsupported platform").

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-400 | Valid receipt creation | Ed25519 key loaded, chain initialized | `emit()` called with valid inputs | Receipt returned with valid signature, correct `prev_hash`, monotonic timestamp |
| S-401 | Hash chain integrity | Two receipts emitted sequentially | Verifier checks chain | Second receipt's `prev_hash` matches `blake3` of first receipt; chain validates |
| S-402 | Key file 0600 enforced | Key file at `key_path` with `0600` perms | Sandbox created | Sandbox creation succeeds, `ReceiptEmitter` initialized |
| S-403 | Key file 0644 rejected | Key file at `key_path` with `0644` perms | Sandbox created | Sandbox creation fails with `ConfigError::Io` |
| S-404 | Key file missing | No file at `key_path` | Sandbox created | Sandbox creation fails with `ConfigError::MissingField` or `ConfigError::Io` |
| S-405 | Invalid key format | Key file present but malformed base64 | Sandbox created | Sandbox creation fails with `ConfigError` |
| S-414 | Deterministic serialization | Same `ExecutionReceipt` serialized twice | Bytes compared | Both serializations produce identical byte sequences |
| S-415 | Windows/Unix platform scope | Non-Unix platform (Windows) | Sandbox created | Sandbox creation fails with `ConfigError::Io` ("unsupported platform") |
| S-416 | Signing failure on happy path | Force signing error (corrupt key) on S-1 call | `aegis_fs_read` called | `aegis_fs_read` returns `Trap`, no data returned to caller |

## RENAMED Requirements

No requirements renamed.

## REMOVED Requirements

No requirements removed.
