# Tasks — Phase 9: Two-Phase Receipts

Change: `phase9-two-phase-receipts`  
Status: **Ready for apply**  
Predecessors: proposal.md approved; specs approved (grpc-runtime-server, signed-receipts, receipt-verifier, aegis-runtime-binary); design.md approved

---

## Dependency Graph

```
WU1: Protobuf + gRPC stubs (prereq de todo)
    ↓
WU2: ReceiptEmitter + PendingReceipt + TTL sweep (prereq de WU3/WU4)
    ↓
WU3: Receipt schema + verify_chain (depende WU2)
    ↓
WU4: gRPC handlers ExecutePrepare/Commit/Abort + D4 cascade + legacy Execute wrapper (depende WU1, WU2, WU3)
    ↓
WU5: Tests integrales + AD-016 + regresiones (depende WU4)
```

---

## Work Units

### WU1 — Protobuf + gRPC stubs

**Gate**: `cargo build` + `cargo test` (existing tests pass) + `cargo clippy -- -D warnings` + `cargo fmt -- --check`

**Archivos**:
- `proto/aegis/v1/aegis.proto` — add 3 RPCs + messages per design §4.1
- `build.rs` — ensure prost regeneration runs

**Scenarios cubiertos**: S-900, S-901, S-902, S-903, S-904, S-905, S-906, S-907, S-908 (proto definitions enable all E2E scenarios)

```
[ ] WU1-1 Add ExecutePrepare/ExecuteCommit/ExecuteAbort RPCs and messages to proto/aegis/v1/aegis.proto
[ ] WU1-2 Regenerate gRPC stubs (cargo build triggers build.rs)
[ ] WU1-3 Verify existing tests still compile and pass
```

---

### WU2 — ReceiptEmitter + PendingReceipt + TTL sweep

**Gate**: `cargo test receipts` + `cargo clippy -- -D warnings` + `cargo fmt -- --check`

**Archivos**:
- `src/receipts/mod.rs` — add `PendingReceipt` struct, `pending: HashMap<[u8;32], PendingReceipt>`, `PrepareHandle`, `prepare()`, `commit()`, `abort()`, TTL sweep task (spawned in `new()`)
- `src/sandbox/mod.rs` — add `SandboxHandle` type (opaque handle with `Arc<Mutex<Sandbox>>`)

**Scenarios cubiertos**: S-920, S-921, S-922, S-923, S-924, S-926, S-927, S-928, E-929, E-930, S-954, S-955

```
[ ] WU2-1 Add PendingReceipt struct with all fields (prepare_receipt, sandbox_handle, created_at, capability_name, config, wasm_module)
[ ] WU2-2 Add PrepareHandle opaque wrapper around pending_hash
[ ] WU2-3 Add SandboxHandle in src/sandbox/mod.rs (Arc<Mutex<Sandbox>>)
[ ] WU2-4 Add pending HashMap to ReceiptEmitter (protected by existing Mutex)
[ ] WU2-5 Implement prepare(capability, config, wasm_module) -> Result<(ExecutionReceipt, PrepareHandle)>
[ ] WU2-6 Implement commit(handle, result, path, size, fuel_consumed) -> Result<ExecutionReceipt> with idempotent lookup
[ ] WU2-7 Implement abort(handle, error) -> Result<ExecutionReceipt> with idempotent lookup
[ ] WU2-8 Implement TTL sweep: tokio::spawn in new(), runs every 60s, PENDING_TTL_SECS = 600, emits abort with error="ttl_expired" before removal
[ ] WU2-9 Unit tests for prepare/commit/abort flow, idempotency, TTL expiry, concurrent pending entries
```

---

### WU3 — Receipt schema + verify_chain

**Gate**: `cargo test receipts` + `cargo clippy -- -D warnings` + `cargo fmt -- --check`

**Archivos**:
- `src/receipts/mod.rs` — modify `ExecutionReceipt` struct (add `phase` + `pending_hash` before `timestamp_ns` with skip-if-zero), add `is_zero_array` helper, modify `ReceiptChain::verify_chain` with state machine per design §4.2

**Scenarios cubiertos**: S-920, S-921, S-922, S-923, S-924, S-925, S-930, S-931, S-940–S-955, E-930

```
[ ] WU3-1 Add phase: String field to ExecutionReceipt (default "", before timestamp_ns)
[ ] WU3-2 Add pending_hash: [u8;32] with serde_bytes + skip_serializing_if = "is_zero_array" (before timestamp_ns)
[ ] WU3-3 Add is_zero_array helper function
[ ] WU3-4 Modify verify_chain with expected_pending_hash state machine:
[ ] WU3-5   - Legacy (phase=""): normal chain, pending_hash must be zero
[ ] WU3-6   - Prepare: verify pending_hash = blake3(prev_hash || prepare_canonical), set expected_pending_hash
[ ] WU3-7   - Commit/Abort: require expected_pending_hash, verify prev_hash == pending_hash == expected, clear expected_pending_hash
[ ] WU3-8   - Reject: orphaned prepare, commit/abort without prepare, prepare after prepare, commit after abort, abort after commit
[ ] WU3-9   - Accept: idempotent duplicate commit/abort with identical canonical bytes
[ ] WU3-10 Unit tests for all verify_chain scenarios (S-940 through S-955)
```

---

### WU4 — gRPC handlers ExecutePrepare/Commit/Abort + D4 cascade + legacy Execute wrapper

**Gate**: `cargo test` (E2E) + `cargo clippy -- -D warnings` + `cargo fmt -- --check`

**Archivos**:
- `src/grpc/handlers/mod.rs` — add `execute_prepare`, `execute_commit`, `execute_abort` handlers; modify `execute` (deprecated wrapper) per design §4.3
- `src/grpc/server.rs` — no functional changes (auto-registers new RPCs via trait)
- `src/config/runtime.rs` — add `TwoPhaseReceipts` capability parsing per design §4.5

**Scenarios cubiertos**: S-900–S-912, E-913, E-914, S-970–S-974

```
[ ] WU4-1 Add TwoPhaseReceipts to Capability enum (marker capability)
[ ] WU4-2 Add two_phase_receipts parsing in PolicyConfig::try_into_capabilities()
[ ] WU4-3 Implement execute_prepare handler: parse config, validate TwoPhaseReceipts capability, create sandbox with fuel_budget, call ReceiptEmitter::prepare(), return receipt + prepare_hash
[ ] WU4-4 Implement execute_commit handler: parse prepare_hash hex -> PrepareHandle, call ReceiptEmitter::commit() with captured result/path/size/fuel, return commit receipt
[ ] WU4-5 Implement execute_abort handler: parse prepare_hash -> PrepareHandle, call ReceiptEmitter::abort(), return abort receipt
[ ] WU4-6 Implement D4 cascade order in Commit: network guard (catalog prefixes) → fuel arm (all fuel consumed) → filesystem cascade (traversal/outside/size/exceed/max) → signing failure (receipt/signing/test-forced)
[ ] WU4-7 Implement deprecated execute wrapper: Prepare + Commit atomically; on Commit signing failure -> Abort with error="commit_signing_failed"
[ ] WU4-8 E2E tests for ExecutePrepare/Commit/Abort happy paths, idempotency, not found, concurrent prepares, D4 cascade, legacy Execute wrapper
```

---

### WU5 — Tests integrales + AD-016 + regresiones

**Gate**: `cargo test` (full suite) + `cargo clippy -- -D warnings` + `cargo fmt -- --check`

**Archivos**:
- `tests/two_phase.rs` — new E2E test file for two-phase flows
- `DECISIONS.md` — add AD-016 (Commit signing failure documented gap + external compensation)

**Scenarios cubiertos**: All scenarios from specs (S-900–S-912, S-920–S-928, S-940–S-955, S-970–S-974, E-913, E-914, E-929, E-930)

```
[ ] WU5-1 Create tests/two_phase.rs with E2E tests:
[ ] WU5-2   - Prepare happy path (S-900)
[ ] WU5-3   - Prepare invalid capability (S-901)
[ ] WU5-4   - Prepare missing WASM (S-902)
[ ] WU5-5   - Commit happy path (S-903)
[ ] WU5-6   - Commit idempotent (S-904)
[ ] WU5-7   - Commit not found (S-905)
[ ] WU5-8   - Abort happy path (S-906)
[ ] WU5-9   - Abort idempotent (S-907)
[ ] WU5-10  - Abort not found (S-908)
[ ] WU5-11  - Prepare→Commit chain (S-909)
[ ] WU5-12  - Prepare→Abort chain (S-910)
[ ] WU5-13  - Legacy Execute deprecated (S-911)
[ ] WU5-14  - Concurrent Prepares independent (S-912)
[ ] WU5-15  - D4 cascade in Commit (E-913)
[ ] WU5-16  - Budget set in Prepare, reused in Commit (E-914)
[ ] WU5-17  - TwoPhaseReceipts granted/denied (S-970, S-971)
[ ] WU5-18  - Legacy Execute works without capability (S-972)
[ ] WU5-19  - Commit/Abort authorized by prepare_hash (S-973, S-974)
[ ] WU5-20  - Commit signing failure forced test: prepare exists, abort with commit_signing_failed emitted
[ ] WU5-21 Add AD-016 to DECISIONS.md (same work unit as code, following AD-014 D6 / AD-015 precedent):
[ ] WU5-22   Title: "ADR-016: Commit signing failure — documented gap with external compensation"
[ ] WU5-23   Status: Accepted
[ ] WU5-24   Context: Two-phase receipts create signed prepare before WASM executes. If Commit executes WASM (side-effects real) then final signing fails...
[ ] WU5-25   Decision: Same mitigation class as AD-005. Signed prepare exists in chain. Abort receipt emitted with error="commit_signing_failed". External compensation required: audit log + operator alert.
[ ] WU5-26   Consequences: Operators must monitor for abort with commit_signing_failed; audit log integration out of scope v1; verifier accepts prepare→abort as valid terminal.
[ ] WU5-27 Run full test suite: cargo test (all existing + new), clippy, fmt
```

---

## Summary

| WU | Descripción | Archivos principales | Gate |
|----|-------------|---------------------|------|
| **WU1** | Protobuf + gRPC stubs | `proto/aegis/v1/aegis.proto`, `build.rs` | `cargo build` + `cargo test` |
| **WU2** | ReceiptEmitter + PendingReceipt + TTL | `src/receipts/mod.rs`, `src/sandbox/mod.rs` | `cargo test receipts` |
| **WU3** | Receipt schema + verify_chain | `src/receipts/mod.rs` | `cargo test receipts` |
| **WU4** | gRPC handlers + D4 + legacy Execute | `src/grpc/handlers/mod.rs`, `src/config/runtime.rs` | `cargo test` (E2E) |
| **WU5** | Tests integrales + AD-016 + regresiones | `tests/two_phase.rs`, `DECISIONS.md` | `cargo test` full suite |

---

## Notas de implementación

- **AD-016** va en el mismo WU que el código (WU5), siguiendo precedente Phase 7 D6 y Phase 8 AD-015
- **Orden de campos en ExecutionReceipt**: `phase` + `pending_hash` **antes de** `timestamp_ns` (orden canónico preservado para backward compat)
- **Skip-if-zero**: `pending_hash` con `is_zero_array` para que receipts legacy (`phase=""`, `pending_hash=0`) serialicen **byte-identicos** a pre-cambio
- **TTL**: constante `PENDING_TTL_SECS = 600` (10 min), NO configurable en v1
- **D4 cascade**: network → fuel → fs → signing (en Commit handler)
- **Capability gating**: `ExecutePrepare` requiere `TwoPhaseReceipts`; `ExecuteCommit`/`ExecuteAbort` autorizados implícitamente por `prepare_hash` lookup
- **Sandbox handle**: `SandboxHandle` con `Arc<Mutex<Sandbox>>` para reuso entre Prepare→Commit/Abort
- **Verifier state machine**: `expected_pending_hash: Option<[u8;32]>` trackea prepare→commit/abort
- **No new crates**: wasmtime 24, ring, blake3, tokio ya en tree

---

## Rollback Plan (del design §10)

1. Remove `ExecutePrepare`/`ExecuteCommit`/`ExecuteAbort` RPCs from protobuf and handlers
2. Remove `phase` + `pending_hash` from `ExecutionReceipt` (skip-if-zero keeps old receipts valid)
3. Remove `pending` map + `prepare`/`commit`/`abort` from `ReceiptEmitter`; restore single `emit()`
4. Revert `ReceiptChain::verify_chain` to linear verification
5. Restore `Execute` as primary RPC
6. Configs, chains, verifier untouched — skip-if-zero + default `phase=""` ensure backward compat both directions