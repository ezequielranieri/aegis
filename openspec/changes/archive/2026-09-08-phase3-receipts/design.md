# Design: Phase 3 — Mature Receipts

## Technical Approach

Replace the `emit_capability_event` tracing stub with cryptographically signed execution receipts that form a BLAKE3 hash chain. Every `aegis_fs_read` invocation produces a verifiable `ExecutionReceipt` signed with Ed25519 (via `ring`). An external `ReceiptChain::verify_chain()` validates chain integrity without the runtime. Key management loads an `Ed25519KeyPair` from a separate key file at sandbox creation (fail-closed), validated for 0600 permissions on Unix — Windows is unsupported and fails immediately per REQ-426.

## Architecture Decisions

### Decision: ExecutionReceipt Structure (REQ-400, REQ-401, REQ-403)

**Choice**: Redefine `ExecutionReceipt` with fixed-size arrays for `prev_hash`/`signature`, nanosecond timestamp, and canonical JSON serialization via `serde_json::to_vec` (struct field declaration order = canonical order).

**Alternatives considered**:
- Keep existing colon-delimited string payload with SHA256 → violates REQ-410 (BLAKE3 required)
- Use CBOR (`serde_cbor`) for canonical serialization → adds dependency, spec mandates JSON (REQ-401)
- Use `ed25519-dalek` instead of `ring` → `ring` already in use, proven, no need to switch

**Rationale**: Spec explicitly requires canonical JSON (REQ-401), BLAKE3 (REQ-410), and fixed-size arrays for hash/signature. Struct field order in Rust is stable and deterministic — no `HashMap`/`BTreeMap` in receipt ensures REQ-403.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub capability_name: String,  // "filesystem.read"
    pub action: String,           // "read" | "write" | "http"
    pub result: String,           // "success" | "trap" | "size_exceeded"
    pub path: String,             // requested path
    pub size: u64,                // file size (0 for trap before stat)
    pub prev_hash: [u8; 32],      // BLAKE3 hash of previous receipt
    pub signature: [u8; 64],      // Ed25519 signature over canonical bytes
    pub timestamp_ns: u64,        // SystemTime::now() as nanos since epoch
}
```

### Decision: Hash Chain Design (REQ-410..412)

**Choice**: `prev_hash = blake3::hash(&[&prev_hash, &canonical_bytes].concat())`. Genesis receipt uses `prev_hash = [0u8; 32]`. Use `blake3` crate (v1.0).

**Alternatives considered**:
- SHA256 via `ring::digest` → violates REQ-410 (BLAKE3 mandatory)
- Store hash as hex String → violates REQ-400 (fixed-size `[u8; 32]`)

**Rationale**: BLAKE3 is fast, parallelizable, and specified in crypto-receipts skill. The hash chain enables tamper-evident audit: any modification breaks the chain.

### Decision: Key Management (REQ-420..426)

**Choice**: Extend `PolicyConfig` with optional `[receipts]` section containing `key_path: PathBuf`. Load `Ed25519KeyPair` from separate key file (TOML with base64-encoded private/public keys). Validate 0600 permissions on Unix using `std::os::unix::fs::PermissionsExt`. On Windows, fail immediately with `ConfigError::Io("unsupported platform")` — no permission check, no warning (REQ-426). No auto-generation; fail-closed if missing/invalid.

**Alternatives considered**:
- Embed key in policy config → violates key isolation requirement
- Auto-generate key if missing → violates REQ-424 (fail-closed)
- Support Windows with different permission model → violates REQ-426 (Windows unsupported)

**Rationale**: Separate key file with 0600 perms follows security best practice. Windows unsupported is explicit in spec — avoids platform-specific permission logic.

```toml
# .aegis/config.toml
[[capabilities]]
name = "filesystem.read"
allowed_root = "/data"
max_read_bytes = 1048576

[receipts]
key_path = "/etc/aegis/signing_key.toml"
```

```toml
# /etc/aegis/signing_key.toml (0600 perms)
[signing_key]
private_key = "base64_encoded_32_bytes"
public_key = "base64_encoded_32_bytes"
```

### Decision: ReceiptEmitter (REQ-430..433)

**Choice**: New `ReceiptEmitter` struct stored in `SandboxState`. Holds `Ed25519KeyPair` + `ReceiptChain`. `emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` creates signed receipt, updates chain state. Signing failure propagates as error (never produces unsigned receipt).

**Failure semantics**: If `emit()` returns `Err`, the calling host function (`aegis_fs_read`) MUST immediately return `Trap` — the original operation result (success or violation) is discarded. No data is returned to the caller, no violation trap is allowed to proceed. This applies uniformly to all 5 scenarios: S-1 (happy path) and S-2/S-3/S-5 (violations). The signed receipt is a mandatory proof of execution; without it, the operation is aborted.

**Alternatives considered**:
- Allow operation to proceed on signing failure, log error only → violates fail-closed principle
- Emit receipt on best effort, trap only if receipt creation succeeds → creates audit gap

**Rationale**: Centralized in `SandboxState` keeps host function signatures clean. `caller.data_mut()` gives exclusive mutable access per host call — single-threaded execution ensures no races. The fail-closed rule on emit failure ensures the receipt chain is complete and verifiable; no operation completes without its corresponding receipt.

```rust
pub struct ReceiptEmitter {
    key_pair: Ed25519KeyPair,
    chain: ReceiptChain,
}

impl ReceiptEmitter {
    pub fn emit(
        &mut self,
        capability: &str,
        action: &str,
        path: &str,
        size: u64,
        result: &str,
    ) -> Result<ExecutionReceipt> { ... }
}
```

### Decision: emit_capability_event Replacement (REQ-440..441)

**Choice**: Replace `emit_capability_event` signature to access `ReceiptEmitter` via `caller.data_mut().receipt_emitter`. Called from `aegis_fs_read` at all 5 validation points (S-1 success, S-2 denied, S-3 traversal, S-5 size exceeded, S-5 guest OOB). S-4 (fd leak) excluded — module fails at instantiation before any host function runs.

**Execution order at each call site (uniform for all 5 scenarios)**:
1. `ReceiptEmitter::emit(capability, action, path, size, result)` is called
2. If `emit()` returns `Ok(receipt)`: receipt is added to chain, host function proceeds (returns data for S-1, or raises trap for S-2/S-3/S-5)
3. If `emit()` returns `Err(e)`: `aegis_fs_read` immediately returns `Trap` with `e` — original operation result (success data or violation trap) is discarded; no data returned to caller

**Rationale**: The existing `aegis_fs_read` already calls `emit_capability_event` at the right points (after capability config loaded, before each violation check). Just swap the implementation to produce real receipts. The fail-closed rule on emit failure ensures the receipt chain is complete; no operation completes without its corresponding signed receipt.

### Decision: Sandbox Integration

**Choice**: Add `receipt_emitter: Option<ReceiptEmitter>` to `SandboxState`. `Sandbox::new_with_limits()` and `Sandbox::from_config()` both load key from config and create `ReceiptEmitter`. Add `Sandbox::get_receipt_chain()` returning `&ReceiptChain` for CLI/test access.

**Rationale**: Both sandbox creation paths must initialize receipts. Optional in state allows tests without receipts (though Phase 3 mandates it).

### Decision: Verification (REQ-450..454)

**Choice**: `ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` as library function. Validates: (1) Ed25519 signature over canonical bytes, (2) hash chain integrity (`prev_hash == blake3(prev_hash || canonical_bytes)`), (3) timestamp within ±5s of `SystemTime::now()`. Optional CLI binary `src/bin/aegis-verify.rs` wraps this.

**Rationale**: Stateless verifier enables external audit without runtime. ±5s clock skew tolerance per crypto-receipts skill. Library + CLI covers both in-process and external use.

## Data Flow

```
aegis_fs_read (host function)
       │
       ▼
caller.data_mut().receipt_emitter.as_mut()?.emit(
    capability, "read", path, size, result
)
       │
       ▼
ReceiptEmitter.emit() ─────────────────┐
    │                                  │
    ▼                                  │
ExecutionReceipt::new()                │
    │                                  │
    ▼                                  │
serde_json::to_vec() (canonical bytes) │
    │                                  │
    ▼                                  │
blake3::sign() with Ed25519KeyPair     │
    │                                  │
    ▼                                  │
ReceiptChain.update(prev_hash) ◄───────┘
    │
    ▼
Returns ExecutionReceipt (stored in chain)

Verification (external):
ReceiptChain::verify_chain(receipts, public_key)
    │
    ├─► For each receipt: verify Ed25519 signature
    ├─► For each receipt: verify prev_hash == blake3(prev || canonical)
    └─► Check timestamp_ns within ±5s of now()
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `src/receipts/mod.rs` | Modify | Replace `ExecutionReceipt` struct, add `ReceiptEmitter`, `blake3` hash chain, `verify_chain()` |
| `src/sandbox/mod.rs` | Modify | Add `receipt_emitter` to `SandboxState`, load key in `new_with_limits()` and `from_config()`, add `get_receipt_chain()` |
| `src/config/mod.rs` | Modify | Extend `PolicyConfig` with optional `ReceiptsConfig { key_path: PathBuf }` |
| `Cargo.toml` | Modify | Add `blake3 = "1.0"` dependency |
| `tests/sandbox.rs` | Modify | Update capability event capture for new receipt format; add receipt verification tests |
| `src/bin/aegis-verify.rs` | Create | Optional CLI verifier binary |
| `openspec/config/aegis-keys.toml` | Create | Example key config file (template) |

## Interfaces / Contracts

### ExecutionReceipt (canonical JSON via struct field order)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub capability_name: String,
    pub action: String,
    pub result: String,
    pub path: String,
    pub size: u64,
    pub prev_hash: [u8; 32],
    pub signature: [u8; 64],
    pub timestamp_ns: u64,
}
```

Canonical bytes = `serde_json::to_vec(&receipt).unwrap()` — field order in struct declaration is the canonical order.

### ReceiptEmitter

```rust
impl ReceiptEmitter {
    pub fn new(key_pair: Ed25519KeyPair) -> Self { ... }
    pub fn emit(
        &mut self,
        capability: &str,
        action: &str,
        path: &str,
        size: u64,
        result: &str,
    ) -> Result<ExecutionReceipt> { ... }
}
```

### ReceiptChain

```rust
impl ReceiptChain {
    pub fn verify_chain(
        receipts: &[ExecutionReceipt],
        public_key: &[u8],
    ) -> Result<()> { ... }
}
```

### PolicyConfig Extension

```rust
#[derive(Debug, Deserialize)]
pub struct ReceiptsConfig {
    pub key_path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct PolicyConfig {
    pub capabilities: Vec<CapabilityDef>,
    #[serde(default)]
    pub receipts: Option<ReceiptsConfig>,
}
```

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Unit | `ExecutionReceipt` deterministic serialization (S-414) | Serialize same receipt twice, compare bytes |
| Unit | Hash chain integrity (S-401) | Emit 2 receipts, verify `prev_hash` linkage |
| Unit | `ReceiptChain::verify_chain` success | Valid chain + correct public key → Ok |
| Unit | Verifier: tampered payload (S-410) | Modify receipt field, verify fails |
| Unit | Verifier: broken chain (S-411) | Modify `prev_hash`, verify fails |
| Unit | Verifier: expired timestamp (S-412) | Old timestamp, verify fails |
| Unit | Verifier: wrong public key (S-413) | Valid chain, wrong key, verify fails |
| Integration | Key file 0600 enforced (S-402/S-403) | Create key file 0600 → success; 0644 → `ConfigError::Io` |
| Integration | Key file missing (S-404) | No key file → `ConfigError::MissingField`/`Io` |
| Integration | Invalid key format (S-405) | Malformed base64 → `ConfigError` |
| Integration | Windows unsupported (S-415) | Simulate Windows → `ConfigError::Io("unsupported platform")` |
| Integration | S-1 receipt emitted (S-406) | Happy path → receipt with `result="success"` |
| Integration | S-2 receipt emitted (S-407) | Path outside root → `result="trap"` |
| Integration | S-3 receipt emitted (S-408) | Traversal `..` → `result="trap"` |
| Integration | S-5 receipt emitted (S-409) | Size exceeded → `result="trap"` |
| Integration | Signing failure on happy path → operation traps (S-416) | Force signing error (corrupt key) on S-1 call → `aegis_fs_read` returns `Trap`, no data returned |
| Regression | 5 Phase 1 receipt tests (REQ-464) | All existing `emit_capability_event_s*` tests pass |
| E2E | CLI verifier (S-454) | `aegis-verify` reads chain file, validates |

## Threat Matrix

| Threat | Applicable | Expected Safe Behavior | RED Test |
|--------|------------|------------------------|----------|
| Path traversal via `..` in receipt path field | N/A — receipt path is logged, not used for access | Receipt records attempted path; no filesystem access | S-407, S-408 |
| Hash chain tampering | Applicable — receipt chain is security boundary | Verifier detects any `prev_hash` or payload modification | S-410, S-411 |
| Key file permission bypass | Applicable — 0600 enforced on Unix | Sandbox creation fails with `ConfigError::Io` if not 0600 | S-403 |
| Windows platform | N/A — explicit unsupported (REQ-426) | Immediate `ConfigError::Io("unsupported platform")` | S-415 |
| Timestamp manipulation | Applicable — ±5s window | Verifier rejects receipts outside window | S-412 |
| Unsigned receipt production | N/A — signing failure propagates | `emit()` returns `Err`, no receipt created | Signing error path |

## Migration / Rollout

No migration required — Phase 3 is a new capability replacing a stub. Rollback plan (from proposal):
1. Revert `Cargo.toml` (remove `blake3`)
2. Revert `src/receipts/mod.rs` to stub implementation
3. Remove `ReceiptEmitter` from `SandboxState`
4. Restore `emit_capability_event` with original signature
5. Delete `src/bin/aegis-verify.rs` and test additions
6. Delete key config file

## Open Questions

- [ ] None — all design decisions resolved per spec requirements

## Next Step

Ready for tasks (sdd-tasks).