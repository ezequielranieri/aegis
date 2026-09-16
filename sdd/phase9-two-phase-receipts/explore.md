# Exploration: Phase 9 Two-Phase Receipts

## Exploration: phase9-two-phase-receipts

### Current State

The Phase 9 two-phase receipts implementation is **already substantially written** in the codebase. The proposal is validated against existing code — this is not a greenfield implementation but a validation of what's already there.

**Already implemented:**
- `proto/aegis/v1/aegis.proto`: 3 new RPCs (`ExecutePrepare`, `ExecuteCommit`, `ExecuteAbort`) + messages. `Execute` marked deprecated.
- `src/receipts/mod.rs`: `ExecutionReceipt` has `phase: String` + `pending_hash: [u8; 32]` fields with skip-if-zero/skip-if-empty. `ReceiptEmitter` has `prepare()`, `commit()`, `abort()`, `pending: HashMap<[u8;32], PendingReceipt>`, `spawn_ttl_sweep()`, `PrepareHandle`, `PendingReceipt` struct, `SandboxHandle` usage. TTL constants (600s/60s). `force_signing_failure` test hook.
- `src/grpc/handlers/mod.rs`: `execute_prepare`, `execute_commit`, `execute_abort` handlers implemented. `execute` deprecated wrapper with Prepare+Commit/Abort flow.
- `src/sandbox/mod.rs`: `SandboxHandle` (Arc<TokioMutex<Sandbox>>) with `lock()` async method. `fuel_consumed()` works post-call.
- `src/capabilities/mod.rs`: `Capability::TwoPhaseReceipts` marker variant exists.
- `src/config/mod.rs`: `CapabilityDef::TwoPhaseReceipts` variant with `try_into_capability()` conversion.
- `src/grpc/server.rs`: `spawn_ttl_sweep` called in `start_server_internal`.
- `src/receipts/mod.rs` tests: `prepare_commit_abort_flow`, `prepare_commit_idempotent`, `prepare_abort_idempotent`, `ttl_expiry_emits_abort`, `concurrent_pending_entries`, `commit_chain_verification`, `abort_chain_verification`, `orphaned_prepare_rejected`, `commit_without_prepare_rejected`, `verify_chain_legacy_still_works`.

**OpenSpec specs already written:**
- `openspec/changes/phase9-two-phase-receipts/specs/signed-receipts/spec.md`: REQ-750..756 defined
- `openspec/changes/phase9-two-phase-receipts/specs/grpc-runtime-server/spec.md`: REQ-730..740 defined
- `openspec/changes/phase9-two-phase-receipts/specs/receipt-verifier/spec.md`
- `openspec/changes/phase9-two-phase-receipts/specs/aegis-runtime-binary/spec.md`: REQ-820..823 defined
- `openspec/changes/phase9-two-phase-receipts/design.md`: Full design with sections 1-12
- `openspec/changes/phase9-two-phase-receipts/tasks.md`

### Affected Areas

**`src/receipts/mod.rs`** — Fully implemented. `ExecutionReceipt` fields, `ReceiptEmitter::prepare/commit/abort`, `PendingReceipt`, `PrepareHandle`, `SandboxHandle`, `spawn_ttl_sweep`, `cleanup_expired_pending`, `emit_abort_internal`, `create_receipt_for_hash`, `canonical_bytes_for_chain_without_pending_hash`, `verify_chain` state machine. Tests all present.

**`src/grpc/handlers/mod.rs`** — Fully implemented. `execute_prepare` validates `TwoPhaseReceipts` capability, calls `emitter.prepare()`, returns `prepare_hash` hex. `execute_commit` parses `prepare_hash`, gets sandbox/config/wasm_module from pending, executes WASM, calls `emitter.commit()`. `execute_abort` parses `prepare_hash`, calls `emitter.abort()`. `execute` deprecated wrapper with Prepare+Commit/Abort flow.

**`proto/aegis/v1/aegis.proto`** — Fully implemented. 3 new RPCs, messages, Execute deprecated comment.

**`src/sandbox/mod.rs`** — `SandboxHandle` at line 642. `fuel_consumed()` at line 461. `Sandbox::new_with_config` sets fuel budget.

**`src/grpc/server.rs`** — `spawn_ttl_sweep` called at line 63.

**`src/capabilities/mod.rs`** — `TwoPhaseReceipts` variant at line 20.

**`src/config/mod.rs`** — `CapabilityDef::TwoPhaseReceipts` at line 52.

### Approaches

1. **3-RPC approach (Option A — implemented)** — `ExecutePrepare`, `ExecuteCommit`, `ExecuteAbort` as separate gRPC RPCs with `pending_hash` linkage. Sandbox created in Prepare, reused in Commit/Abort via `SandboxHandle`. TTL sweep at 10 min. Commit signing failure → abort with `error="commit_signing_failed"`.
   - Pros: Explicit state machine, natural gRPC mapping, easy retry/idempotency, clear failure boundaries
   - Cons: 3 RPCs for clients vs 1; state management complexity in `ReceiptEmitter`
   - Effort: Medium (already implemented)

2. **Single RPC with phase field (Option B — not chosen)** — Single `Execute` with `phase` in request. Couples client logic to server state.
   - Pros: Simpler client API
   - Cons: Couples client logic to server state, harder retry/idempotency
   - Effort: Low (not implemented)

### Recommendation

The implementation validates Option A. However, **critical fixes are needed** before spec/design finalization:

1. **Fix `PolicyConfig::default()` compilation error** — `PolicyConfig` has no `Default` impl; `src/receipts/mod.rs` tests call `crate::config::PolicyConfig::default()`. Blocks `cargo test` entirely.
2. **Address GAP 2**: `execute_commit` must emit abort receipt on signing failure (D5/REQ-755). Currently returns `Status::internal` without abort.
3. **Address GAP 1/4**: `execute_commit` must handle WASM trap by calling abort (like deprecated `execute`). Currently calls `commit()` on trap, returning `success: true`.
4. **Clean up dead code**: `ExecuteCommitRequest.result` field is unused.

### Risks

- **R1 (unchanged)**: Commit signing failure after WASM executes — handled in `execute` wrapper but NOT in `execute_commit` standalone RPC. Signed prepare exists but abort receipt may not be emitted per D5.
- **R7 (unchanged)**: Caller crashes between Prepare and Commit — TTL bounds this, but `execute_commit` trap handling creates inconsistent chain behavior vs deprecated `execute`.
- **Compilation failure**: `PolicyConfig::default()` blocks all tests in `src/receipts/mod.rs`.
- **`execute_commit` always returns `success: true` on trap**: Creates commit receipt with trap info but `success: true` — differs from deprecated `execute` behavior.
- **SandboxHandle uses `tokio::sync::Mutex`** while `ReceiptEmitter` uses `std::sync::Mutex`: Mixed sync/async mutexes, though functionally correct.

### Phase 8 Fuel Metering Verification

Phase 8 plumbing is fully in place:
- `SandboxConfig.fuel_budget: Option<u64>` → `Sandbox::new_with_config()` sets `store.set_fuel(budget_resolved)`
- `Sandbox::fuel_consumed()` returns `budget_resolved - store.get_fuel().unwrap_or(resolved)`
- `ExecutionConfig.fuel_budget` passed to `AegisRuntimeService.fuel_budget` → `ReceiptEmitter::prepare()` → `SandboxConfig`
- `execute_commit` captures `sandbox.fuel_consumed()` after WASM execution
- Skip-if-zero serialization verified by E-802/S-803 tests

### Ready for Proposal

**YES** — with caveats. The implementation validates the proposal approach, but requires fixes for compilation (`PolicyConfig::Default`) and consistency between `execute_commit` and the deprecated `execute` wrapper (trap handling and signing failure abort) before spec/design finalization. The core two-phase mechanism (prepare/commit/abort, pending map, TTL sweep, verify_chain state machine) is correctly implemented.
