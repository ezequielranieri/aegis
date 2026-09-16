# Design — Phase 9: Two-Phase Receipts

Change: `phase9-two-phase-receipts`
Status: **Ready for review**
Predecessors: proposal.md approved; specs approved (grpc-runtime-server, signed-receipts, receipt-verifier, aegis-runtime-binary)

---

## 1. Purpose

Add two-phase receipt emission at the Execute RPC level: `ExecutePrepare` → `ExecuteCommit` / `ExecuteAbort`. A pending receipt is created and signed in Prepare (before WASM executes), the hash is returned to the caller. On Commit, the receipt is finalized with the actual execution result and appended to the chain. On Abort, the pending receipt is marked aborted and discarded. Host functions (`aegis_fs_write`, `aegis_http_fetch`) remain **legacy single-phase** (`phase=""`) — this change is Execute-only (D1).

This closes AD-005's receipt gap: `aegis_fs_write` performs atomic `rename()` **before** emitting the success receipt. If emit fails after rename, the host side effect is committed but the receipt chain has a gap. Two-phase moves the commit point into the protocol — a **signed pending receipt exists before WASM executes** with `phase="prepare"` and `pending_hash`. If final Commit signing fails, we have a signed prepare receipt proving intent/pre-state, plus an `abort` receipt with `error="commit_signing_failed"`. External compensation (audit log + alert) remains the mitigation — same class as AD-005.

---

## 2. Goals / Non-Goals

Per proposal D1–D8:

- **G1** — `ExecutionReceipt` gains `phase` (`prepare`|`commit`|`abort`|`""`) + `pending_hash` (REQ-750).
- **G2** — `ReceiptEmitter` gains `pending: HashMap<[u8;32], PendingReceipt>` with TTL cleanup (REQ-752, REQ-753, REQ-754).
- **G3** — Three new gRPC RPCs: `ExecutePrepare`, `ExecuteCommit`, `ExecuteAbort`; legacy `Execute` deprecated wrapper (REQ-730..740).
- **G4** — `ReceiptChain::verify_chain` validates prepare→commit/abort linkage, orphaned prepares, commit-after-abort (REQ-760..762).
- **G5** — `Capability::TwoPhaseReceipts` marker capability gates `ExecutePrepare` (REQ-820..823).
- **G6** — TTL sweep: 10 min (600s) fixed, emits `abort` with `error="ttl_expired"` (REQ-754).
- **G7** — Commit signing failure: prepare receipt exists, abort receipt emitted with `error="commit_signing_failed"` (REQ-755, AD-016).
- **NG1** — Host functions unchanged (single-phase, `phase=""`, `pending_hash=0`).
- **NG2** — No sandbox serialization for restart survival (in-memory for v1; documented limitation).
- **NG3** — No `schema_version` field (superseded by skip-if-zero evolution, Phase 8 D5).

---

## 3. Technical Approach

Three independent layers, each with a single thread of change (mirrors Phase 8 structure):

### 3.1 Receipts layer (`src/receipts/mod.rs`)

**`ExecutionReceipt`** — add two fields **before `timestamp_ns`** (canonical order):
```rust
pub phase: String,                    // "" (legacy) | "prepare" | "commit" | "abort"
#[serde(with = "serde_bytes")]
#[serde(default, skip_serializing_if = "is_zero_array")]
pub pending_hash: [u8; 32],          // non-zero only for phase="prepare"
```

`is_zero_array` helper parallels `is_zero`. With skip-if-zero:
- Legacy receipts (`phase=""`, `pending_hash=0`) serialize **byte-identically** to pre-change schema.
- `phase="prepare"`: `pending_hash = blake3(prev_hash || prepare_canonical_bytes)`.
- `phase="commit"|"abort"`: `pending_hash` copied from prepare; normal chain hash continues.

**`PendingReceipt`** (internal):
```rust
struct PendingReceipt {
    prepare_receipt: ExecutionReceipt,    // signed, phase="prepare"
    sandbox_handle: SandboxHandle,        // reference to prepared sandbox
    created_at: SystemTime,
    capability_name: String,
    config: PolicyConfig,
    wasm_module: Vec<u8>,
}
```

**`ReceiptEmitter` API**:
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

`PrepareHandle` is an opaque wrapper around `pending_hash` (the map key). The gRPC layer maps hex `prepare_hash` → `PrepareHandle`.

**TTL sweep**: spawned in `ReceiptEmitter::new()` via `tokio::spawn`, runs every 60s. Iterates `pending` under the same `Mutex`, removes entries older than 600s, emits `abort` receipt with `error="ttl_expired"` **before** removal. Constant: `PENDING_TTL_SECS = 600`.

### 3.2 gRPC layer (`src/grpc/handlers/mod.rs`, `src/grpc/server.rs`, `proto/aegis/v1/aegis.proto`)

**Protobuf** — 3 new RPCs + messages:
```protobuf
message ExecutePrepareRequest {
  string capability_name = 1;
  bytes config = 2;
  bytes wasm_module = 3;
}
message ExecutePrepareResponse {
  bool success = 1;
  bytes receipt = 2;           // JSON ExecutionReceipt phase="prepare"
  string prepare_hash = 3;     // hex(pending_hash)
  string error_message = 4;
}
message ExecuteCommitRequest {
  string prepare_hash = 1;
  bytes result = 2;
}
message ExecuteCommitResponse {
  bool success = 1;
  bytes receipt = 2;           // JSON ExecutionReceipt phase="commit"
  string error_message = 3;
}
message ExecuteAbortRequest { string prepare_hash = 1; }
message ExecuteAbortResponse {
  bool success = 1;
  bytes receipt = 2;           // JSON ExecutionReceipt phase="abort"
  string error_message = 3;
}
```
`Execute` RPC marked deprecated in comments; internally maps to Prepare+Commit atomically (on Commit failure → Abort with `commit_signing_failed`).

**Handler flow**:
- `ExecutePrepare`: parse config, validate `TwoPhaseReceipts` capability, create sandbox (with fuel budget), sign `prepare` receipt (`result="pending"`, `path=""`, `size=0`, `fuel_consumed=0`), store `PendingReceipt` with sandbox handle, return receipt + `prepare_hash`.
- `ExecuteCommit`: lookup pending by `prepare_hash` (idempotent: if removed, return already-emitted commit receipt), execute WASM in prepared sandbox, capture result/fuel, sign `commit` receipt with actual result/path/size/fuel, remove pending.
- `ExecuteAbort`: lookup pending (idempotent), sign `abort` receipt (`result="aborted"`, `error` optional), discard sandbox, remove pending.
- `Execute` (deprecated): sequential Prepare+Commit; on Commit sign failure → Abort with `commit_signing_failed`.

**D4 cascade order in Commit** (REQ-715 modified):
1. `network ` guard (first) — catalog prefixes
2. Fuel arm — `all fuel consumed`
3. Filesystem cascade — traversal / outside / size / exceed / max
4. Signing failure — `receipt` / `signing` / `test-forced`

### 3.3 Verifier layer (`src/receipts/mod.rs`)

`ReceiptChain::verify_chain` gains state machine tracking `expected_pending_hash: Option<[u8;32]>`:

| Phase | Prev Hash | Pending Hash | Next Expected |
|-------|-----------|--------------|---------------|
| `""` (legacy) | normal chain | must be 0 | unchanged |
| `prepare` | normal chain | verify == `blake3(prev\|prepare_canonical)` | `Some(pending_hash)` |
| `commit`/`abort` | must equal `expected_pending_hash` | must equal `expected_pending_hash` | `None` (terminal) |

Rules:
- After `prepare`: next MUST be `commit` or `abort` with matching `pending_hash` (no orphaned prepares).
- After `commit`/`abort`: next can be new `prepare` or legacy `""`.
- `commit` after `abort` (same `pending_hash`) → FAIL.
- Idempotent retry: duplicate `commit`/`abort` with identical canonical bytes accepted (hash continues).

---

## 4. Detailed Design

### 4.1 `proto/aegis/v1/aegis.proto`

Add three RPCs and messages to `AegisRuntime` service. Mark `Execute` deprecated in comments.

```protobuf
service AegisRuntime {
  // Legacy single-phase Execute (DEPRECATED: use ExecutePrepare + ExecuteCommit/Abort)
  rpc Execute(ExecuteRequest) returns (ExecuteResponse);

  // Two-phase execution: Prepare creates sandbox + signs pending receipt
  rpc ExecutePrepare(ExecutePrepareRequest) returns (ExecutePrepareResponse);
  // Commit executes WASM in prepared sandbox + finalizes receipt
  rpc ExecuteCommit(ExecuteCommitRequest) returns (ExecuteCommitResponse);
  // Abort discards pending receipt + sandbox
  rpc ExecuteAbort(ExecuteAbortRequest) returns (ExecuteAbortResponse);

  rpc VerifyChain(VerifyChainRequest) returns (VerifyChainResponse);
  rpc GetReceiptChain(GetReceiptChainRequest) returns (GetReceiptChainResponse);
}
```

Full message definitions per REQ-731..738.

### 4.2 `src/receipts/mod.rs`

**`ExecutionReceipt`** (lines ~13-27):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub capability_name: String,
    pub action: String,
    pub result: String,
    pub path: String,
    pub size: u64,
    #[serde(with = "serde_bytes")] pub prev_hash: [u8; 32],
    #[serde(with = "serde_bytes")] pub signature: [u8; 64],
    // Phase 9: two-phase fields BEFORE timestamp_ns (canonical order)
    pub phase: String,
    #[serde(with = "serde_bytes")]
    #[serde(default, skip_serializing_if = "is_zero_array")]
    pub pending_hash: [u8; 32],
    pub timestamp_ns: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fuel_consumed: u64,
}

fn is_zero_array(v: &[u8; 32]) -> bool {
    v.iter().all(|&b| b == 0)
}
```

**`PendingReceipt`** (new, internal):
```rust
struct PendingReceipt {
    prepare_receipt: ExecutionReceipt,
    sandbox_handle: SandboxHandle,
    created_at: SystemTime,
    capability_name: String,
    config: PolicyConfig,
    wasm_module: Vec<u8>,
}
```

**`SandboxHandle`** (new, in `src/sandbox/mod.rs` or re-exported here):
```rust
/// Opaque handle to a prepared sandbox for Commit/Abort reuse.
pub struct SandboxHandle {
    // Could be an Arc<Mutex<Sandbox>> or index into a sandbox pool
    // For v1: Arc<Mutex<Sandbox>> held by ReceiptEmitter pending map
    inner: Arc<Mutex<Sandbox>>,
}
```

**`PrepareHandle`** (new):
```rust
/// Opaque handle returned by prepare(), used by commit()/abort().
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrepareHandle {
    pending_hash: [u8; 32],
}
```

**`ReceiptEmitter`** (lines ~211-296):
- Add `pending: HashMap<[u8; 32], PendingReceipt>` field (protected by same Mutex).
- `prepare()`: creates sandbox, signs prepare receipt, computes `pending_hash = blake3(prev_hash || prepare_canonical)`, stores `PendingReceipt`, returns `(receipt, PrepareHandle)`.
- `commit(handle, ...)`: lookup `pending.remove(&handle.pending_hash)` → if `None`, return already-emitted commit receipt (idempotent); else execute WASM in sandbox, sign commit receipt, return.
- `abort(handle, error)`: similar, emits abort receipt.
- TTL sweep task spawned in `new()`: every 60s, lock, iterate, for expired: emit abort with `error="ttl_expired"`, remove.

**`ReceiptChain::verify_chain`** (lines ~126-195):
Replace linear verification with state machine per REQ-760:
```rust
let mut expected_pending_hash: Option<[u8; 32]> = None;

for receipt in receipts {
    match receipt.phase.as_str() {
        "" => {
            // Legacy: normal chain verification
            verify_signature_and_chain(receipt, &mut expected_prev_hash)?;
            // pending_hash must be zero
            if receipt.pending_hash != [0u8; 32] { bail!("legacy receipt has non-zero pending_hash") }
        }
        "prepare" => {
            // Verify prepare pending_hash
            let canonical = receipt.canonical_bytes_for_signing();
            let computed_pending = ReceiptChain::compute_chain_hash(&expected_prev_hash, &canonical);
            if receipt.pending_hash != computed_pending {
                bail!("prepare: pending_hash mismatch");
            }
            expected_pending_hash = Some(receipt.pending_hash);
            expected_prev_hash = computed_pending; // chain continues from prepare hash
        }
        "commit" | "abort" => {
            let expected = expected_pending_hash.ok_or_else(|| anyhow!("commit/abort without prepare"))?;
            if receipt.prev_hash != expected || receipt.pending_hash != expected {
                bail!("commit/abort: prev_hash/pending_hash mismatch");
            }
            // Normal chain update
            let canonical = receipt.canonical_bytes_for_signing();
            expected_prev_hash = ReceiptChain::compute_chain_hash(&expected, &canonical);
            expected_pending_hash = None; // terminal
        }
        _ => bail!("unknown phase: {}", receipt.phase),
    }
    // timestamp check unchanged
}
```

### 4.3 `src/grpc/handlers/mod.rs`

**Service struct** (lines ~22-26): unchanged (already has `fuel_budget`).

**New handlers** (add after `execute`):
```rust
async fn execute_prepare(&self, request: Request<ExecutePrepareRequest>) -> Result<Response<ExecutePrepareResponse>, Status>
async fn execute_commit(&self, request: Request<ExecuteCommitRequest>) -> Result<Response<ExecuteCommitResponse>, Status>
async fn execute_abort(&self, request: Request<ExecuteAbortRequest>) -> Result<Response<ExecuteAbortResponse>, Status>
```

**`execute_prepare`**:
1. Parse config, validate `TwoPhaseReceipts` capability.
2. Create sandbox with `fuel_budget`.
3. Store sandbox in emitter (same as Execute).
4. Lock emitter, call `prepare(capability, config, wasm_module)`.
5. Return signed prepare receipt + `prepare_hash` (hex).

**`execute_commit`**:
1. Parse `prepare_hash` hex → `[u8;32]` → `PrepareHandle`.
2. Lock emitter, call `commit(handle, result, path, size, fuel_consumed)`.
3. Capture fuel via `sandbox.fuel_consumed()`.
4. Return commit receipt.

**`execute_abort`**:
1. Parse `prepare_hash` → `PrepareHandle`.
2. Lock emitter, call `abort(handle, error)`.
3. Return abort receipt.

**`execute` (deprecated wrapper)**:
```rust
async fn execute(&self, request) -> Result<Response<ExecuteResponse>, Status> {
    // Call prepare logic inline (or reuse prepare handler)
    let (prepare_receipt, handle) = self.prepare_logic(...).await?;
    // Call commit logic
    match self.commit_logic(handle, ...).await {
        Ok(commit_receipt) => Ok(commit_receipt),
        Err(e) if e.code() == Code::Internal && e.message().contains("signing") => {
            // Commit signing failure → abort
            let abort_receipt = self.abort_logic(handle, Some("commit_signing_failed")).await?;
            Ok(abort_receipt.into())
        }
        Err(e) => Err(e),
    }
}
```

### 4.4 `src/sandbox/mod.rs`

**`SandboxHandle`** (new type, e.g., after `Sandbox` struct):
```rust
/// Opaque handle to a sandbox instance for two-phase reuse.
/// Held by ReceiptEmitter::PendingReceipt, released on Commit/Abort.
pub struct SandboxHandle {
    inner: Arc<Mutex<Sandbox>>,
}

impl SandboxHandle {
    pub fn new(sandbox: Sandbox) -> Self {
        Self { inner: Arc::new(Mutex::new(sandbox)) }
    }
    pub fn lock(&self) -> tokio::sync::MutexGuard<'_, Sandbox> {
        self.inner.lock().await
    }
}
```
(Or synchronous `Mutex` if no await in critical section — use `std::sync::Mutex` for consistency with `ReceiptEmitter`.)

**No changes to `fuel_consumed()`** — works as-is: Commit calls `sandbox.fuel_consumed()` after execution.

### 4.5 `src/config/runtime.rs`

**`ExecutionConfig`** — no change (already has `fuel_budget`).

**`PolicyConfig` / capability parsing** (for REQ-821):
```rust
impl TryFrom<PolicyConfig> for Vec<Capability> {
    fn try_from(config: PolicyConfig) -> Result<Vec<Capability>> {
        // ... existing logic ...
        if config.two_phase_receipts == Some(true) {
            capabilities.push(Capability::TwoPhaseReceipts);
        }
        Ok(capabilities)
    }
}
```

### 4.6 `src/grpc/server.rs`

No functional changes — service registration already uses `AegisRuntimeServer::new(aegis_service)` which will automatically include the new RPCs once the trait impl is updated.

---

## 5. ADR-016 Draft — Commit Signing Failure Gap

> **ADR-016: Commit signing failure — documented gap with external compensation**
>
> **Status:** Draft (design) → Proposed (apply)
>
> **Context:** Two-phase receipts (Phase 9) create a signed `prepare` receipt before WASM executes. If `ExecutePrepare` succeeds → `ExecuteCommit` executes WASM (host side-effects occur) → **final commit signing fails** (disk full, key corruption, clock skew, lock contention): the side effects are real and committed, but no `commit` receipt is produced.
>
> **Decision:** Same mitigation class as AD-005 (S-416-W-after-rename):
> - The signed `prepare` receipt (`phase="prepare"`) **exists in the chain** — proves intent and pre-execution state.
> - `ReceiptEmitter` emits an `abort` receipt with `phase="abort"`, `pending_hash` = prepare's `pending_hash`, `result="aborted"`, `error="commit_signing_failed"`, `fuel_consumed=0`.
> - The receipt chain shows: `prepare` → `abort(commit_signing_failed)`.
> - **External compensation required**: audit log entry + operator alert (identical to AD-005 pattern).
> - This is a **known gap with documented mitigation** — NOT a silent failure.
>
> **Alternatives considered:**
> - Auto-retry signing (3×): masks root cause (key corruption, disk full), adds latency, no guarantee.
> - Emit unsigned receipt + flag: breaks chain verification invariants (all receipts must be signed).
> - Block Commit until signing succeeds: unbounded wait, no progress guarantee.
>
> **Consequences:**
> - Operators MUST monitor for `abort` receipts with `error="commit_signing_failed"` and correlate with host side-effects.
> - Audit log integration (out of scope for v1) will automate this correlation.
> - Verifier accepts prepare→abort as valid terminal chain (REQ-760).
> - AD-016 recorded in DECISIONS.md same work unit as code (AD-014 D6 precedent).

---

## 6. Spike Results — Idempotency / Timeout R4 (Lightweight Reasoning)

**Requirement R4 from proposal**: "Sandbox handle reuse race (concurrent Commit on same prepare) — Low". Instead of heavy integration tests, we reason from the actual code in `src/receipts/mod.rs`.

### 6.1 Idempotency: `commit()` / `abort()` use `pending_hash` as key

**Evidence** (current `ReceiptEmitter::emit` pattern at lines 245-283):
```rust
pub fn emit(&mut self, ...) -> Result<ExecutionReceipt> {
    // ... signing ...
    self.last_prev_hash = ReceiptChain::compute_chain_hash(...);
    self.receipts.push(receipt.clone());
    Ok(receipt)
}
```

**Design for two-phase** (per REQ-752):
```rust
pub fn commit(&mut self, handle: PrepareHandle, ...) -> Result<ExecutionReceipt> {
    // Key line: remove returns the PendingReceipt if present, None if already removed
    let pending = self.pending.remove(&handle.pending_hash);
    let pending = match pending {
        Some(p) => p,
        None => {
            // Already committed/aborted — return the already-emitted receipt from chain
            return self.receipts.iter()
                .rev()
                .find(|r| r.pending_hash == handle.pending_hash && r.phase == "commit")
                .cloned()
                .ok_or_else(|| anyhow!("commit not found for handle"));
        }
    };
    // ... execute WASM, sign commit receipt, push to chain ...
}
```

**Atomicity**: The entire `commit()` method runs under `&mut self` (exclusive `Mutex` lock in `ReceiptEmitter`). The `HashMap::remove` is atomic within that lock. A second concurrent `commit()` call:
1. Waits for the mutex (first call holds it).
2. First call completes: removes entry, emits commit receipt, releases lock.
3. Second call acquires lock: `remove` returns `None` → finds commit receipt in chain → returns it.
**No race** — idempotent by construction.

Same logic applies to `abort()`.

### 6.2 TTL Sweep: `cleanup_expired_pending()` under same Mutex

**Design** (per REQ-754):
```rust
impl ReceiptEmitter {
    pub fn new(key_pair: Ed25519KeyPair) -> Self {
        let emitter = Self { ... };
        // Spawn background task
        tokio::spawn(Self::ttl_sweep(emitter.clone())); // Arc<Mutex<ReceiptEmitter>>
        emitter
    }

    async fn ttl_sweep(emitter: Arc<Mutex<ReceiptEmitter>>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut emitter = emitter.lock().await;
            emitter.cleanup_expired_pending();
        }
    }

    fn cleanup_expired_pending(&mut self) {
        let now = SystemTime::now();
        let expired: Vec<_> = self.pending.iter()
            .filter(|(_, p)| now.duration_since(p.created_at).unwrap_or_default() > Duration::from_secs(PENDING_TTL_SECS))
            .map(|(k, _)| *k)
            .collect();

        for pending_hash in expired {
            if let Some(pending) = self.pending.remove(&pending_hash) {
                // Emit abort receipt BEFORE removal (still holding lock)
                let abort_receipt = self.emit_abort_internal(pending.prepare_receipt.pending_hash, Some("ttl_expired"))?;
                self.receipts.push(abort_receipt);
            }
        }
    }
}
```

**Race analysis** (sweep vs. Commit in progress):
- Sweep takes `Mutex` lock, iterates, removes expired entries.
- Commit takes same `Mutex` lock, does `pending.remove(&hash)`.
- **Case 1**: Sweep runs first, finds entry expired → removes it, emits abort. Commit runs later: `remove` returns `None` → returns already-emitted abort receipt (idempotent).
- **Case 2**: Commit runs first, acquires lock → removes entry, emits commit, releases lock. Sweep runs later: entry already gone → no action.
- **Case 3**: Commit in progress (holding lock) when sweep's 60s tick fires. Sweep waits for lock. Commit finishes, releases lock. Sweep acquires lock, sees entry already removed (by Commit) → no action.
**No race** — single mutex serializes all access.

### 6.3 Recovery: Sweep running while Commit in progress

This is covered by Case 3 above. The lock serializes. No double-emit, no lost receipt.

---

## 7. Risks — Post-Spike Status

| # | Risk | Likelihood | Post-Spike Status | Mitigation |
|---|------|------------|-------------------|------------|
| R1 | Commit signing failure after WASM executes | Medium | **Documented gap (AD-016)** | Signed prepare exists; abort with `commit_signing_failed`; external audit+alert |
| R2 | Pending map memory leak (orphaned prepares) | Low | **Closed by design** | 600s TTL + 60s sweep; metrics on pending map size |
| R3 | `prepare_hash` collision / replay | Negligible | **Closed by crypto** | BLAKE3 chain hash (32 bytes); bound to prev_hash + prepare content |
| R4 | Sandbox handle reuse race (concurrent Commit) | Low | **Closed by spike (§6.1)** | `remove` under mutex → idempotent |
| R5 | Verifier backward compat broken | Low | **Closed by skip-if-zero** | `phase=""` default, `pending_hash` skip-if-zero; legacy chains verify unchanged |
| R6 | TTL too short (long approval flows) | Low | **600s covers human approval** | Configurable knob deferred to Phase 11+ |
| R7 | Caller crashes between Prepare and Commit | Medium | **Idempotent retry + TTL bound** | Caller holds `prepare_hash`; can retry Commit; Abort available |
| R8 | Fuel accounting correctness (Prepare sets budget, Commit measures) | Low | **Phase 8 evidence** | `fuel_consumed()` reads `budget - get_fuel()` post-call; works for trap paths |

---

## 8. Test Plan — Scenario Mapping

| Spec Scenario | Test Type | File | Assertion |
|---------------|-----------|------|-----------|
| S-900 | E2E | `tests/two_phase.rs` | Prepare returns signed `phase="prepare"` receipt with `pending_hash`, `prepare_hash` |
| S-901 | E2E | `tests/two_phase.rs` | Invalid capability → `success=false` |
| S-902 | E2E | `tests/two_phase.rs` | Empty wasm_module → error |
| S-903 | E2E | `tests/two_phase.rs` | Commit returns `phase="commit"` with actual result/fuel, `pending_hash` links |
| S-904 | E2E | `tests/two_phase.rs` | Duplicate Commit returns same commit receipt |
| S-905 | E2E | `tests/two_phase.rs` | Unknown prepare_hash → NOT_FOUND |
| S-906 | E2E | `tests/two_phase.rs` | Abort returns `phase="abort"` receipt |
| S-907 | E2E | `tests/two_phase.rs` | Duplicate Abort returns same abort receipt |
| S-908 | E2E | `tests/two_phase.rs` | Unknown prepare_hash → NOT_FOUND |
| S-909 | E2E | `tests/two_phase.rs` | Chain: prepare → commit verified |
| S-910 | E2E | `tests/two_phase.rs` | Chain: prepare → abort verified |
| S-911 | E2E | `tests/two_phase.rs` | Legacy Execute works (Prepare+Commit internally) |
| S-912 | E2E | `tests/two_phase.rs` | Concurrent Prepares → unique hashes, no cross-talk |
| E-913 | E2E | `tests/two_phase.rs` | D4 cascade in Commit: network → fuel → fs → signing |
| E-914 | E2E | `tests/two_phase.rs` | Budget set in Prepare; Commit reuses sandbox + budget |
| S-920 | Unit | `src/receipts/mod.rs` | Prepare receipt structure: phase/result/path/size/fuel/pending_hash |
| S-921 | Unit | `src/receipts/mod.rs` | Commit receipt structure: actual values, pending_hash copied |
| S-922 | Unit | `src/receipts/mod.rs` | Abort receipt structure: aborted, pending_hash copied |
| S-923 | Unit | `src/receipts/mod.rs` | Chain linkage: commit.prev_hash = prepare_hash, commit.pending_hash = prepare.pending_hash |
| S-924 | Unit | `src/receipts/mod.rs` | Chain linkage: abort.prev_hash = prepare_hash, abort.pending_hash = prepare.pending_hash |
| S-925 | Unit | `src/receipts/mod.rs` | Legacy receipt byte-identical to pre-change |
| S-926 | Unit | `src/receipts/mod.rs` | TTL expiry emits abort with `error="ttl_expired"` |
| S-927 | Unit | `src/receipts/mod.rs` | Commit signing failure → prepare exists, abort with `commit_signing_failed` |
| S-928 | E2E | `tests/two_phase.rs` | Concurrent two-phase flows independent |
| E-929 | Unit | `src/receipts/mod.rs` | fuel_consumed only on commit (prepare/abort omitted) |
| E-930 | Unit | `src/receipts/mod.rs` | prepare.pending_hash == blake3(prev\|prepare_canonical) |
| S-940 | Unit | `src/receipts/mod.rs` | Legacy chain verifies (backward compat) |
| S-941 | Unit | `src/receipts/mod.rs` | Prepare→Commit valid |
| S-942 | Unit | `src/receipts/mod.rs` | Prepare→Abort valid |
| S-943 | Unit | `src/receipts/mod.rs` | Prepare→Commit chain linkage |
| S-944 | Unit | `src/receipts/mod.rs` | Prepare→Abort chain linkage |
| S-945 | Unit | `src/receipts/mod.rs` | Orphaned prepare fails |
| S-946 | Unit | `src/receipts/mod.rs` | Commit without prepare fails |
| S-947 | Unit | `src/receipts/mod.rs` | Abort without prepare fails |
| S-948 | Unit | `src/receipts/mod.rs` | Prepare then prepare fails |
| S-949 | Unit | `src/receipts/mod.rs` | Commit after abort fails |
| S-950 | Unit | `src/receipts/mod.rs` | Abort after commit fails |
| S-951 | Unit | `src/receipts/mod.rs` | Idempotent Commit accepted |
| S-952 | Unit | `src/receipts/mod.rs` | Idempotent Abort accepted |
| S-953 | Unit | `src/receipts/mod.rs` | Mixed legacy + two-phase valid |
| S-954 | Unit | `src/receipts/mod.rs` | Wrong pending_hash in commit fails |
| S-955 | Unit | `src/receipts/mod.rs` | TTL abort in chain valid |
| S-970 | E2E | `tests/two_phase.rs` | TwoPhaseReceipts granted → Prepare works |
| S-971 | E2E | `tests/two_phase.rs` | TwoPhaseReceipts denied → Prepare fails |
| S-972 | E2E | `tests/two_phase.rs` | Legacy Execute works without capability |
| S-973 | E2E | `tests/two_phase.rs` | Commit authorized by prepare_hash |
| S-974 | E2E | `tests/two_phase.rs` | Abort authorized by prepare_hash |

**Full suite gate**: `cargo test` (existing + new) all green; `cargo clippy -- -D warnings` and `cargo fmt -- --check` clean.

---

## 9. Out of Scope

- Host function two-phase (`aegis_fs_write`, `aegis_http_fetch`) — legacy single-phase.
- `crypto.sign` / `crypto.verify` host functions.
- Policy engine host function (guest evaluation).
- WASI component model migration (Phase 10+).
- Capability TTL / dynamic grants (Phase 11+).
- `schema_version` field (superseded by skip-if-zero).
- Sandbox serialization for restart survival (in-memory v1).

---

## 10. Rollback Plan

1. Remove `ExecutePrepare`/`ExecuteCommit`/`ExecuteAbort` RPCs from protobuf and handlers.
2. Remove `phase` + `pending_hash` from `ExecutionReceipt` (skip-if-zero keeps old receipts valid).
3. Remove `pending` map + `prepare`/`commit`/`abort` from `ReceiptEmitter`; restore single `emit()`.
4. Revert `ReceiptChain::verify_chain` to linear verification.
5. Restore `Execute` as primary RPC.
6. Configs, chains, verifier untouched — skip-if-zero + default `phase=""` ensure backward compat both directions.

---

## 11. Dependencies

- wasmtime 24 (already in tree), ring, blake3, tokio (already in tree) — **no new crates**.
- Phase 8 fuel metering (AD-015) provides `fuel_budget` plumbing and `Sandbox::fuel_consumed()`.
- Decision-log skill: AD-016 transcribed at design/apply (same work unit as code; AD-014 D6 precedent).

---

## 12. Success Criteria

- [ ] `cargo test` (existing + new two-phase tests) all green; `cargo clippy -- -D warnings` and `cargo fmt -- --check` clean.
- [ ] `ExecutePrepare` returns signed `phase="prepare"` receipt with `pending_hash`; chain hash verifiable.
- [ ] `ExecuteCommit` executes WASM in prepared sandbox, returns `phase="commit"` receipt with actual result/fuel; `pending_hash` links to prepare.
- [ ] `ExecuteAbort` returns `phase="abort"` receipt; chain continues from prepare hash.
- [ ] TTL sweep (10 min) emits `abort` receipts for expired prepares; pending map bounded.
- [ ] Commit signing failure (forced via test-utils): `prepare` receipt exists, `abort` receipt emitted with `error="commit_signing_failed"`, side effects real — **gap documented explicitly in AD-016**.
- [ ] Legacy `Execute` RPC works (deprecated wrapper); old receipt chains (host functions, pre-Phase9) verify unchanged.
- [ ] Concurrent `ExecutePrepare` calls: each gets unique `prepare_hash`; no cross-talk.
- [ ] Verifier validates prepare→commit, prepare→abort, reject commit after abort, reject prepare after commit.
- [ ] AD-016 recorded in DECISIONS.md with D5 rationale (commit signing failure = documented gap + external compensation).