# Proposal: Two-Phase Receipts for Execute RPC (Phase 9)

- **Date**: 2026-09-14
- **Status**: proposed
- **Trigger**: AD-005 (DECISIONS.md, 2026-09-08) — "Receipt Gap on Write After Successful Rename" — `aegis_fs_write` does atomic `rename()` BEFORE emitting success receipt. If emit fails after rename, host has real write but no receipt in chain. Two-phase receipts close this gap at the Execute RPC level.

## Summary

Introduce **two-phase receipt emission** at the **Execute RPC** level: `ExecutePrepare` → `ExecuteCommit` / `ExecuteAbort`. A pending receipt is created and signed in Prepare (before WASM executes), the hash is returned to the caller. On Commit, the receipt is finalized with the actual execution result and appended to the chain. On Abort, the pending receipt is marked aborted and discarded. Host functions (`aegis_fs_write`, `aegis_http_fetch`) remain **legacy single-phase** (`phase=""`) — this change is Execute-only.

## Rationale

AD-005 documents a fundamental receipt integrity gap: `aegis_fs_write` performs an atomic `rename()` on the host filesystem **before** emitting the success receipt. If `ReceiptEmitter.emit()` fails after the rename (disk full, signing key corruption, clock skew, lock contention), the host side effect is committed but the receipt chain has a gap. The current mitigation is "explicit FAIL-CLOSED VIOLATION audit log + Trap" (AD-005) — honest but incomplete.

Two-phase receipts move the **commit point** into the protocol: the pending receipt is signed and its hash returned **before** WASM executes. If the final Commit signature fails, the side effects have already occurred but we have a **signed pending receipt** (with `phase="prepare"` and `pending_hash`) that proves the intent and the pre-execution state. This closes the AD-005 gap at the Execute RPC boundary.

Phase 8 (fuel metering, AD-015) established the pattern: Execute receipts carry execution-scoped data (`fuel_consumed`). Two-phase extends this with a `phase` field (`prepare` | `commit` | `abort`) and a `pending_hash` linking prepare→commit. The verifier is extended to understand the three-phase chain.

## Intent

Close AD-005's receipt gap for Execute RPC by ensuring **a signed receipt exists before WASM side effects execute**, with a deterministic Commit/Abort protocol that produces a verifiable chain even when final signing fails. The pending map lives in `ReceiptEmitter` with a **concrete TTL (10 minutes)** for automatic cleanup of orphaned prepares.

## Scope

### In Scope

| Area | Change |
|------|--------|
| `ExecutionReceipt` struct | Add `phase: String` (`prepare` \| `commit` \| `abort`), `pending_hash: [u8; 32]` (zero for non-prepare) |
| `ReceiptEmitter` | `pending: HashMap<[u8; 32], PendingReceipt>` with TTL cleanup (10 min); `prepare()`, `commit(pending_hash, result, path, size, fuel)`, `abort(pending_hash)` |
| gRPC protobuf | 3 new RPCs: `ExecutePrepare`, `ExecuteCommit`, `ExecuteAbort`; `ExecuteRequest` gains `prepare_hash` for Commit/Abort |
| `AegisRuntimeService` | `fuel_budget` per-request; Prepare creates sandbox + pending receipt; Commit executes WASM + finalizes; Abort discards pending |
| `ReceiptChain::verify_chain` | Extended to validate prepare→commit linkage (`pending_hash` == prepare's chain hash), abort terminals |
| `Execute` RPC | **Deprecated** (kept for backward compat, internally maps to Prepare+Commit) |
| Tests | Prepare/Commit/Abort happy paths, TTL expiry, concurrent Prepare, Commit signing failure, Abort after Prepare, chain verification |

### Out of Scope

| Area | Reason |
|------|--------|
| Host functions two-phase (`aegis_fs_write`, `aegis_http_fetch`) | Legacy single-phase (`phase=""`) — no protocol change; AD-005 gap for host functions remains documented |
| `crypto.sign` / `crypto.verify` host functions | Not implemented; capability enum exists but no host function |
| Policy engine host function (guest evaluation) | Spec exists (`openspec/specs/policy/`) but no host function |
| WASI component model migration | Phase 10+; custom host functions stay (AD-010) |
| Capability TTL / dynamic grants | Phase 11+; static config per Execute request |
| `schema_version` field on receipts | Superseded by byte-compatible skip-if-zero evolution (Phase 8 D5) |

## Capabilities

### New Capabilities

None. No new capability spec files.

### Modified Capabilities

- `signed-receipts`: `ExecutionReceipt` gains `phase` + `pending_hash`; `ReceiptEmitter` gains `prepare`/`commit`/`abort` + pending map with TTL; canonical serialization order preserved (new fields before `timestamp_ns`).
- `receipt-verifier`: `verify_chain` validates prepare→commit (`pending_hash` linkage), abort receipts are terminal (no commit after abort), hash chain includes prepare phase.
- `grpc-runtime-server`: 3 new RPCs + `Execute` deprecated; request/response types extended; service implementation in `handlers/mod.rs`.
- `sandbox-init`: No change (fresh sandbox per Prepare; Commit/Abort reuse same sandbox instance via handle — see Approach).

## Product Decisions (confirmed)

| # | Decision | Choice | Notes |
|---|----------|--------|-------|
| D1 | Protocol level | Execute RPC only (3 RPCs: Prepare/Commit/Abort) | Host functions stay single-phase; AD-005 gap for host fns remains documented per AD-013 cut item #4 |
| D2 | Prepare timing | Prepare creates sandbox + signs pending receipt **before** WASM executes | Sandbox created in Prepare; Commit/Abort reuse it via in-memory handle |
| D3 | Pending state location | `ReceiptEmitter.pending: HashMap<[u8;32], PendingReceipt>` (not sandbox) | Shared across concurrent Execute RPCs; TTL cleanup centralized |
| D4 | TTL for pending cleanup | **10 minutes** (600 seconds) | Concrete value: covers human-in-the-loop approval flows; short enough to bound orphan risk; long enough for network latency + approval |
| D5 | Commit signing failure post-WASM | **Option A: "Gap conocido documentado + compensación externa"** (same class as AD-005) | If Prepare OK → Commit executes WASM (side-effects occur) → final signing fails: signed `prepare` receipt exists (proves intent/pre-state); Commit emits `abort` receipt with `phase="abort"` + `error="commit_signing_failed"`; side effects are real but chain shows abort. **External compensation required** (audit log + operator alert) — same mitigation class as AD-005 today. |
| D6 | Receipt schema evolution | `phase` + `pending_hash` added **before** `timestamp_ns`; `skip_if_zero` on `pending_hash` for non-prepare | Non-prepare receipts serialize byte-identically to pre-change (backward compatible); `phase` defaults to `""` (legacy) for host function receipts |
| D7 | Abort semantics | Abort receipt has `phase="abort"`, empty result, `size=0`, `fuel_consumed=0`; chain hash continues from prepare | Abort is a valid chain link; verifier accepts prepare→abort as terminal; no Commit after Abort |
| D8 | Deprecated Execute RPC | Kept for backward compat; internally calls Prepare+Commit atomically | Old clients work; new clients use 3-RPC flow; deprecation notice in docs |

## Approach

### Alternatives Compared

| Fork | Option A (Chosen) | Option B | Option C | Rationale |
|------|-------------------|----------|----------|-----------|
| **Protocol shape** | 3 separate RPCs: `ExecutePrepare`, `ExecuteCommit`, `ExecuteAbort` | Single `Execute` with `phase` field in request | Streaming RPC (bidirectional) | 3 RPCs = explicit state machine, natural gRPC mapping, easy retry/idempotency, clear failure boundaries. Single RPC with phase field couples client logic to server state. Streaming adds complexity without benefit (no server push). |
| **Prepare scope** | Prepare: create sandbox + sign pending receipt (no WASM execute) | Prepare: create sandbox + execute WASM + sign pending | Prepare: only sign receipt (no sandbox) | **A**: Sandbox created in Prepare lets Commit reuse it (fuel budget already set, modules loaded). B makes Prepare long-running and couples execution to prepare. C defers sandbox creation to Commit but loses fuel budget atomicity. |
| **Sandbox lifetime** | Fresh sandbox per Prepare; Commit/Abort reuse via handle | Fresh sandbox per RPC (Prepare, Commit, Abort each new) | Long-lived sandbox pool | **A**: Fuel budget set once in Prepare; Commit reuses same fuel accounting. B would double fuel consumption or require budget transfer. Pool adds state management complexity. |
| **Pending key** | `pending_hash = blake3(prev_hash || prepare_canonical_bytes)` (chain hash) | UUIDv7 / random | Monotonic counter | **A**: `pending_hash` IS the chain hash — verifiable linkage. UUID/counter adds mapping indirection and is not self-verifying. |
| **TTL cleanup** | Background task in `ReceiptEmitter` (10 min) | Lazy cleanup on next Prepare | No cleanup (unbounded) | **A**: Bounded memory; 10 min covers realistic approval latencies; lazy cleanup leaves orphans under load. |
| **Commit signing failure** | Option A (documented gap + external compensation) | Auto-retry signing (3×) | Emit unsigned receipt + flag | **A**: Auto-retry masks root cause (key corruption, disk full). Unsigned receipt breaks chain verification invariants. Documented gap is honest; external compensation (audit log + alert) is the existing AD-005 pattern. |

### Implementation Shape

1. **Receipt struct** (`src/receipts/mod.rs`):
   ```rust
   pub struct ExecutionReceipt {
       // ... existing fields ...
       pub phase: String,                    // "prepare" | "commit" | "abort" | "" (legacy)
       #[serde(with = "serde_bytes")]
       #[serde(default, skip_serializing_if = "is_zero_array")]
       pub pending_hash: [u8; 32],          // non-zero only for phase="prepare"
       // fuel_consumed, timestamp_ns after
   }
   ```

2. **PendingReceipt** (internal to `ReceiptEmitter`):
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

3. **ReceiptEmitter API**:
   - `prepare(capability, config, wasm_module) -> Result<(ExecutionReceipt, PrepareHandle)>`
   - `commit(handle, result, path, size, fuel_consumed) -> Result<ExecutionReceipt>`
   - `abort(handle) -> Result<ExecutionReceipt>`
   - Background TTL sweep (10 min) removing expired pending entries, emitting `abort` receipts with `error="ttl_expired"`

4. **gRPC** (`proto/aegis/v1/aegis.proto`):
   ```protobuf
   message ExecutePrepareRequest {
     string capability_name = 1;
     bytes config = 2;
     bytes wasm_module = 3;
   }
   message ExecutePrepareResponse {
     bool success = 1;
     bytes receipt = 2;           // signed prepare receipt (phase="prepare")
     string prepare_hash = 3;     // hex(pending_hash) for Commit/Abort
     string error_message = 4;
   }
   message ExecuteCommitRequest {
     string prepare_hash = 1;
     bytes result = 2;            // guest result bytes
   }
   message ExecuteCommitResponse { ... }  // final commit receipt
   message ExecuteAbortRequest { string prepare_hash = 1; }
   message ExecuteAbortResponse { ... }   // abort receipt
   ```

5. **Handler flow**:
   - `ExecutePrepare`: parse config, validate capability, create sandbox (with fuel budget), sign `prepare` receipt (result="pending", path="", size=0, fuel_consumed=0), store `PendingReceipt` with sandbox handle, return prepare receipt + `prepare_hash`
   - `ExecuteCommit`: lookup pending by `prepare_hash`, execute WASM in the prepared sandbox, capture result/fuel, emit `commit` receipt with actual result/path/size/fuel, remove pending
   - `ExecuteAbort`: lookup pending, emit `abort` receipt, discard sandbox, remove pending
   - `Execute` (deprecated): calls Prepare+Commit sequentially; on Commit failure, calls Abort

6. **Verifier** (`ReceiptChain::verify_chain`):
   - Track `expected_phase`: after `prepare` expect `commit` or `abort`; after `commit`/`abort` expect next `prepare` or end
   - For `prepare`: compute `pending_hash = blake3(prev_hash || prepare_canonical)`, store as expected for next
   - For `commit`: verify `receipt.pending_hash == expected_pending_hash`, then normal chain hash update
   - For `abort`: verify `receipt.pending_hash == expected_pending_hash`, chain continues from abort's hash

7. **TTL cleanup**: `tokio::spawn` in `ReceiptEmitter::new` runs every 60s, removes entries older than 10 min, emits `abort` with `error="ttl_expired"` before removal.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/receipts/mod.rs` | Modified | `ExecutionReceipt` (+phase, +pending_hash), `ReceiptEmitter` (+pending map, prepare/commit/abort, TTL task), `ReceiptChain::verify_chain` (phase-aware) |
| `proto/aegis/v1/aegis.proto` | Modified | 3 new RPCs + messages; `Execute` marked deprecated in comments |
| `src/grpc/handlers/mod.rs` | Modified | `execute_prepare`, `execute_commit`, `execute_abort` handlers; `execute` deprecated wrapper; sandbox handle management |
| `src/sandbox/mod.rs` | Minor | `SandboxHandle` type for passing prepared sandbox to Commit/Abort; `fuel_consumed()` works post-call |
| `src/config/runtime.rs` | Minor | No change (fuel_budget already plumbed) |
| `openspec/specs/signed-receipts/spec.md` | Modified | New REQs for two-phase fields, prepare/commit/abort semantics, TTL |
| `openspec/specs/receipt-verifier/spec.md` | Modified | REQ-400 field enumeration updated; new scenarios for prepare/commit/abort verification |
| `openspec/specs/grpc-runtime-server/spec.md` | Modified | New RPC requirements + scenarios; Execute deprecated |
| `tests/` | Modified | Two-phase E2E tests; TTL expiry; concurrent Prepare; Commit signing failure; Abort; chain verification |

## Risks

| # | Risk | Likelihood | Mitigation |
|---|------|------------|------------|
| R1 | Commit signing failure after WASM executes (side-effects real, no commit receipt) | Medium (same class as AD-005) | **D5 chosen**: signed `prepare` receipt exists proving intent/pre-state; `abort` receipt with `error="commit_signing_failed"` emitted; external audit log + alert (AD-005 pattern). Documented as **known gap with mitigation** — not silent. |
| R2 | Pending map memory leak (orphaned prepares never committed/aborted) | Low | **D4**: 10 min TTL + background sweep emits `abort` receipts for expired entries; metrics on pending map size |
| R3 | `prepare_hash` collision / replay attack | Negligible | `pending_hash` = BLAKE3 chain hash (32 bytes); cryptographically bound to prev_hash + prepare content |
| R4 | Sandbox handle reuse race (concurrent Commit on same prepare) | Low | `PendingReceipt` removed from map on first Commit/Abort; second lookup fails with `NOT_FOUND` |
| R5 | Verifier backward compat broken (old chains fail) | Low | `phase` defaults to `""`, `pending_hash` skip-if-zero; legacy receipts (host fns, old Execute) verify unchanged |
| R6 | TTL too short (legitimate long approval flows) | Low | 10 min covers human approval + network latency; configurable via `execution.prepare_ttl_secs` if needed (future knob) |
| R7 | Prepare+Commit atomicity: caller crashes between Prepare and Commit | Medium | Caller holds `prepare_hash`; can retry Commit idempotently; Abort available; TTL bounds abandonment |
| R8 | Fuel accounting: Prepare creates sandbox (sets budget), Commit executes — fuel_consumed measured correctly | Low | `Sandbox::fuel_consumed()` reads `budget - get_fuel()` post-call; works for both success and trap in Commit |

## Open Questions

1. **Prepare sandbox handle serialization**: Should the sandbox be held in memory (current design) or serialized to disk for survive-restart? → **In-memory for v1** (Execute RPC is per-request; restart = chain reset anyway). Document as known limitation.

2. **Concurrent Prepare for same capability**: Allowed (different `prepare_hash` each). No deduplication — each Prepare gets unique chain hash. Acceptable.

3. **Execute RPC deprecation timeline**: Keep for 2 phases minimum; remove in Phase 11+. Document in spec.

## Evidence References

- **AD-005**: DECISIONS.md lines 151-200 — S-416-W-after-rename gap, current mitigation (audit log + trap)
- **Explore #138** (engram `sdd/phase9-candidates/explore`): Candidate A recommended; alternatives compared; risks enumerated
- **AD-013**: DECISIONS.md lines 529-562 — two-phase receipts explicitly cut from Phase 6, deferred to future phase
- **AD-015**: DECISIONS.md lines 631-663 — Phase 8 fuel metering precedent (Execute-scoped receipt fields, skip-if-zero evolution)
- **Code paths**:
  - `src/receipts/mod.rs:13-27` (ExecutionReceipt struct), `:211-296` (ReceiptEmitter), `:126-195` (ReceiptChain::verify_chain)
  - `src/grpc/handlers/mod.rs:29-225` (Execute RPC), `:227-300` (VerifyChain), `:302-340` (GetReceiptChain)
  - `proto/aegis/v1/aegis.proto:5-14, 22-33` (service + Execute messages)
  - `src/sandbox/mod.rs:365-408` (Sandbox::new_with_config with fuel_budget)
- **Specs**: `openspec/specs/signed-receipts/spec.md` (REQ-430..433, REQ-723), `openspec/specs/grpc-runtime-server/spec.md` (REQ-701, REQ-714, REQ-715, REQ-717), `openspec/specs/receipt-verifier/spec.md` (REQ-400, REQ-450..453)

## Rollback Plan

1. Remove `ExecutePrepare`/`ExecuteCommit`/`ExecuteAbort` RPCs from protobuf and handlers
2. Remove `phase` + `pending_hash` from `ExecutionReceipt` (skip-if-zero keeps old receipts valid)
3. Remove `pending` map + `prepare`/`commit`/`abort` from `ReceiptEmitter`; restore single `emit()`
4. Revert `ReceiptChain::verify_chain` to linear verification (no phase logic)
5. Restore `Execute` as primary RPC (no deprecation)
6. Configs, chains, verifier untouched — skip-if-zero + default phase="" ensure backward compat both directions

## Dependencies

- wasmtime 24 (already in tree), ring, blake3, tokio (already in tree) — no new crates
- Phase 8 fuel metering (AD-015) provides `fuel_budget` plumbing and `Sandbox::fuel_consumed()`
- Decision-log skill: AD-016 transcribed at design/apply (same work unit as code; AD-014 D6 precedent)

## Success Criteria

- [ ] `cargo test` (existing + new two-phase tests) all green; `cargo clippy -- -D warnings` and `cargo fmt -- --check` clean
- [ ] `ExecutePrepare` returns signed `phase="prepare"` receipt with `pending_hash`; chain hash verifiable
- [ ] `ExecuteCommit` executes WASM in prepared sandbox, returns `phase="commit"` receipt with actual result/fuel; `pending_hash` links to prepare
- [ ] `ExecuteAbort` returns `phase="abort"` receipt; chain continues from prepare hash
- [ ] TTL sweep (10 min) emits `abort` receipts for expired prepares; pending map bounded
- [ ] Commit signing failure (forced via test-utils): `prepare` receipt exists, `abort` receipt emitted with `error="commit_signing_failed"`, side effects real — **gap documented explicitly in AD-016**
- [ ] Legacy `Execute` RPC works (deprecated wrapper); old receipt chains (host functions, pre-Phase9) verify unchanged
- [ ] Concurrent `ExecutePrepare` calls: each gets unique `prepare_hash`; no cross-talk
- [ ] Verifier validates prepare→commit, prepare→abort, reject commit after abort, reject prepare after commit
- [ ] AD-016 recorded in DECISIONS.md with D5 rationale (commit signing failure = documented gap + external compensation)