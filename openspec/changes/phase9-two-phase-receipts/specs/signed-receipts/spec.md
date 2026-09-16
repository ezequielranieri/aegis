# Delta: signed-receipts — Phase 9 Two-Phase Receipts

## MODIFIED Requirements

### REQ-430: ReceiptEmitter in SandboxState (MODIFIED)

System SHALL store `ReceiptEmitter` in `SandboxState`. When `aegis-runtime` serves via gRPC, `ReceiptEmitter` SHALL be owned by the runtime process and shared across concurrent sandbox instances via `Arc<Mutex<ReceiptEmitter>>`.

**CHANGE**: `ReceiptEmitter` gains a `pending: HashMap<[u8; 32], PendingReceipt>` map for two-phase receipt state. The pending map is protected by the same mutex and is cleaned up by a background TTL sweep.

### REQ-431: ReceiptEmitter Holds Key Pair + Chain (MODIFIED)

`ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain`. The Ed25519 private key SHALL remain in-process and MUST NOT be serialized, exported, or transmitted over gRPC. Only the corresponding public key MAY be returned to callers (e.g., via `GetReceiptChain`).

**CHANGE**: Unchanged for two-phase; the key pair still signs all three receipt phases.

### REQ-433: Fail-Closed Signing Failure (MODIFIED)

Signing failure SHALL propagate as error, never produce unsigned receipt. If `emit()` returns `Err`, the calling host function MUST immediately return `Trap`. For gRPC Execute RPC, signing failure SHALL result in `ExecuteResponse.success=false`.

**CHANGE**: Extended to cover two-phase signing failures:
- `ExecutePrepare` signing failure → returns `success=false`, no pending entry created
- `ExecuteCommit` signing failure → emits `abort` receipt with `phase="abort"`, `error="commit_signing_failed"`, removes pending entry
- `ExecuteAbort` signing failure → returns `success=false`, pending entry remains (caller can retry Abort)

### REQ-723: fuel_consumed on Execute Receipts (MODIFIED)

Execute receipts SHALL carry `fuel_consumed: u64` reporting the fuel consumed by the execution, measured as `effective_budget - Store::get_fuel()` after the call returns. The field SHALL be serialized with `skip_serializing_if = "is_zero"` and SHALL be declared last, after `timestamp_ns`, so receipts with `fuel_consumed == 0` serialize byte-identically to the pre-change schema and existing chains keep verifying with zero verifier changes. In v1 the field SHALL appear on Execute receipts only — success and trap paths; capability receipts (filesystem/network host functions) SHALL NOT gain fuel semantics.

**CHANGE**: For two-phase:
- `phase="prepare"` receipt: `fuel_consumed = 0` (skip-if-zero omits it)
- `phase="commit"` receipt: `fuel_consumed` = actual consumed (measured after Commit execution)
- `phase="abort"` receipt: `fuel_consumed = 0` (skip-if-zero omits it)
- Host function receipts (`phase=""` legacy): unchanged, `fuel_consumed = 0`

---

## ADDED Requirements

### REQ-750: ExecutionReceipt Two-Phase Fields

`ExecutionReceipt` SHALL gain two new fields, declared **before `timestamp_ns`** to preserve canonical serialization order for backward compatibility:
- `phase: String` — one of `""` (legacy/host functions), `"prepare"`, `"commit"`, `"abort"`. Default `""` for backward compat.
- `pending_hash: [u8; 32]` — the BLAKE3 chain hash of the prepare receipt (`blake3(prev_hash || prepare_canonical_bytes)`). Non-zero **only** when `phase="prepare"`. For all other phases, `pending_hash = [0u8; 32]` and SHALL be serialized with `skip_serializing_if = "is_zero_array"` so legacy receipts and non-prepare receipts serialize byte-identically to pre-change schema.

### REQ-751: Two-Phase Receipt Flow

The two-phase receipt flow SHALL be:

1. **Prepare** (`phase="prepare"`):
   - Signed receipt with: `result="pending"`, `path=""`, `size=0`, `fuel_consumed=0`, `pending_hash = blake3(prev_hash || prepare_canonical_bytes)`
   - This receipt is the **first link** in the two-phase chain; its chain hash becomes the `pending_hash` key

2. **Commit** (`phase="commit"`):
   - Signed receipt with: actual `result` (BLAKE3 hash of guest output), actual `path`/`size` from capability, actual `fuel_consumed`, `pending_hash` = prepare's `pending_hash` (copied)
   - Chain hash continues normally: `commit_hash = blake3(prepare_hash || commit_canonical_bytes)`

3. **Abort** (`phase="abort"`):
   - Signed receipt with: `result="aborted"`, `path=""`, `size=0`, `fuel_consumed=0`, `pending_hash` = prepare's `pending_hash` (copied), optional `error` field in result string
   - Chain hash continues from abort: `abort_hash = blake3(prepare_hash || abort_canonical_bytes)`

**NOTE**: `fuel_consumed` appears **only on `commit` receipts** (and trap receipts). Prepare and Abort receipts have `fuel_consumed = 0` (omitted via skip-if-zero). This is consistent with D5 — fuel is execution-scoped.

### REQ-752: ReceiptEmitter Two-Phase API

`ReceiptEmitter` SHALL expose the following methods for two-phase emission:

```rust
pub fn prepare(
    &mut self,
    capability: &str,
    config: &PolicyConfig,
    wasm_module: &[u8],
) -> Result<(ExecutionReceipt, PrepareHandle)>;

pub fn commit(
    &mut self,
    handle: PrepareHandle,
    result: &[u8],
    path: &str,
    size: u64,
    fuel_consumed: u64,
) -> Result<ExecutionReceipt>;

pub fn abort(
    &mut self,
    handle: PrepareHandle,
    error: Option<&str>,
) -> Result<ExecutionReceipt>;
```

Where `PrepareHandle` is an opaque type containing the `pending_hash` (key into the pending map). The gRPC layer maps `prepare_hash` (hex string) to `PrepareHandle`.

### REQ-753: PendingReceipt Internal Struct

`ReceiptEmitter` SHALL maintain an internal `PendingReceipt` struct in the pending map:

```rust
struct PendingReceipt {
    prepare_receipt: ExecutionReceipt,    // signed, phase="prepare"
    sandbox_handle: SandboxHandle,        // reference to the prepared sandbox
    created_at: SystemTime,
    capability_name: String,
    config: PolicyConfig,
    wasm_module: Vec<u8>,
}
```

Keyed by `pending_hash` (the prepare receipt's chain hash = `blake3(prev_hash || prepare_canonical_bytes)`).

### REQ-754: TTL Cleanup — 10 Minutes Fixed (D4)

A background task SHALL run every 60 seconds in `ReceiptEmitter::new()` (spawned via `tokio::spawn`) and SHALL:
- Iterate the pending map
- Remove entries where `created_at` is older than **600 seconds (10 minutes)** — **this value is FIXED and NOT configurable in v1**
- For each expired entry, emit an `abort` receipt with `phase="abort"`, `result="aborted"`, `error="ttl_expired"`, `pending_hash` = the expired entry's `pending_hash`, `fuel_consumed=0`
- Log the cleanup event

**NORMATIVE CONSTANT**: `PENDING_TTL_SECS = 600` — this value SHALL appear in the spec and implementation as a constant. No config knob in v1.

### REQ-755: Commit Signing Failure — Documented Gap + External Compensation (D5)

If `ExecutePrepare` succeeds (signed prepare receipt exists) → `ExecuteCommit` executes WASM (side-effects occur on host) → **final commit signing fails**:
- The signed `prepare` receipt (`phase="prepare"`) **exists in the chain** — proves intent and pre-execution state
- `ReceiptEmitter` SHALL emit an `abort` receipt with: `phase="abort"`, `pending_hash` = prepare's `pending_hash`, `result="aborted"`, `error="commit_signing_failed"`, `fuel_consumed=0`
- The side effects on the host **are real and committed** (cannot be rolled back)
- The receipt chain shows: `prepare` → `abort(commit_signing_failed)`
- **External compensation required**: audit log entry + operator alert (same mitigation class as AD-005 S-416-W-after-rename)

This is a **known gap with documented mitigation** — NOT a silent failure. AD-016 records this decision.

### REQ-756: Host Functions Remain Legacy Single-Phase

Host function receipts (`aegis_fs_read`, `aegis_fs_write`, `aegis_http_fetch`) SHALL continue to emit `phase=""` (empty string) receipts with `pending_hash = [0u8; 32]`. These serialize byte-identically to pre-change schema. The two-phase protocol applies **ONLY to the Execute RPC level**.

---

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-920 | Prepare receipt structure | Valid Prepare call | Receipt emitted | `phase="prepare"`, `result="pending"`, `path=""`, `size=0`, `fuel_consumed=0` (omitted), `pending_hash != 0` |
| S-921 | Commit receipt structure | Valid Commit after Prepare | Receipt emitted | `phase="commit"`, actual result/path/size/fuel, `pending_hash` = prepare's |
| S-922 | Abort receipt structure | Valid Abort after Prepare | Receipt emitted | `phase="abort"`, `result="aborted"`, `path=""`, `size=0`, `fuel=0` (omitted), `pending_hash` = prepare's |
| S-923 | Prepare→Commit chain linkage | Prepare then Commit | Verify chain | `commit.prev_hash = prepare_hash`; `commit.pending_hash = prepare.pending_hash` |
| S-924 | Prepare→Abort chain linkage | Prepare then Abort | Verify chain | `abort.prev_hash = prepare_hash`; `abort.pending_hash = prepare.pending_hash` |
| S-925 | Legacy receipt unchanged | Host function emits receipt | Receipt serialized | `phase=""`, `pending_hash=0` (omitted); byte-identical to pre-change |
| S-926 | TTL expiry emits abort | Pending entry older than 600s | Background sweep runs | `abort` receipt emitted with `error="ttl_expired"`, entry removed |
| S-927 | Commit signing failure | Prepare OK → Commit executes → sign fails | Commit handler catches sign error | `prepare` receipt exists; `abort` receipt with `error="commit_signing_failed"` emitted; chain shows prepare→abort |
| S-928 | Concurrent two-phase flows | Multiple Prepare/Commit/Abort | Concurrent RPCs | Each has unique pending_hash; no cross-contamination; TTL sweep independent |
| E-929 | fuel_consumed only on commit | Prepare + Commit | Receipts inspected | Prepare: fuel=0 (omitted); Commit: fuel=actual; Abort: fuel=0 (omitted) |
| E-930 | Prepare pending_hash = chain hash | Prepare called | Compute hash | `pending_hash == blake3(prev_hash || prepare_canonical_bytes)` |

---

## REMOVED Requirements

None.

## RENAMED Requirements

None.

---

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-430 (modified) | S-928 |
| REQ-431 (modified) | S-920, S-921, S-922 |
| REQ-433 (modified) | S-927 |
| REQ-723 (modified) | E-929 |
| REQ-750 | S-920, S-921, S-922, S-925, E-930 |
| REQ-751 | S-920, S-921, S-922, S-923, S-924 |
| REQ-752 | S-920, S-921, S-922 |
| REQ-753 | S-920, S-921, S-922 |
| REQ-754 | S-926 |
| REQ-755 | S-927 |
| REQ-756 | S-925 |