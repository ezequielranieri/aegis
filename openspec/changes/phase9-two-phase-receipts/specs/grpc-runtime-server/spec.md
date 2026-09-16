# Delta: grpc-runtime-server — Phase 9 Two-Phase Receipts

## MODIFIED Requirements

### REQ-715: Execute RPC Trap Semantics (MODIFIED)

Execute RPC SHALL trap and return `success=false` on any violation (path escape, traversal, size exceed, fuel budget exceeded) — fail-closed.

**CHANGE**: The legacy single `Execute` RPC is **DEPRECATED** but retained for backward compatibility. It internally maps to `ExecutePrepare` + `ExecuteCommit` atomically. New callers SHALL use the three-RPC flow: `ExecutePrepare` → `ExecuteCommit` / `ExecuteAbort`.

**CHANGE**: The trap cascade in `ExecuteCommit` (and deprecated `Execute`) follows this order:
1. `network ` guard (first) — catalog prefixes: endpoint/method/connection/timeout/response-size/rate
2. Fuel arm — deterministic literal `all fuel consumed` (from Phase 8 AD-015 D4)
3. Filesystem cascade — traversal / outside / traversal attempt / size / exceed / max
4. Signing failure — receipt / signing / test-forced

### REQ-717: Fuel Budget Configuration (MODIFIED)

System SHALL accept an optional `execution.fuel_budget` (`Option<u64>`) in `RuntimeConfig`. When absent, the system SHALL apply a generous default budget calibrated by a design spike over the existing filesystem and network E2E suites — the default is design-calibrated and SHALL NOT be specified as a normative constant in this spec. When present, the system SHALL honor the configured value as the per-execution budget. Configurations without the key SHALL parse unchanged.

**CHANGE**: The fuel budget is set **once in `ExecutePrepare`** when the sandbox is created. `ExecuteCommit` and `ExecuteAbort` reuse the same sandbox instance via an in-memory handle, preserving the fuel accounting across the two-phase flow. `Execute` (deprecated) sets the budget per-call as before.

---

## ADDED Requirements

### REQ-730: ExecutePrepare RPC

System SHALL implement `ExecutePrepare(ExecutePrepareRequest) -> ExecutePrepareResponse` RPC that:
- Accepts `capability_name` (string), `config` (bytes, TOML-encoded PolicyConfig), `wasm_module` (bytes)
- Validates the capability is granted in config
- Creates a fresh sandbox with the configured fuel budget
- **Signs a `prepare` receipt** with: `phase="prepare"`, `result="pending"`, `path=""`, `size=0`, `fuel_consumed=0`, `pending_hash = BLAKE3(prev_hash || prepare_canonical_bytes)`
- Stores the pending receipt + sandbox handle in `ReceiptEmitter.pending` map keyed by `pending_hash`
- Returns the signed prepare receipt + `prepare_hash` (hex-encoded `pending_hash`)

### REQ-731: ExecutePrepareRequest

`ExecutePrepareRequest` SHALL contain:
- `capability_name` (string)
- `config` (bytes, TOML-encoded PolicyConfig)
- `wasm_module` (bytes)

### REQ-732: ExecutePrepareResponse

`ExecutePrepareResponse` SHALL contain:
- `success` (bool)
- `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="prepare"`)
- `prepare_hash` (string, hex-encoded `pending_hash` for Commit/Abort)
- `error_message` (string, populated on failure)

### REQ-733: ExecuteCommit RPC

System SHALL implement `ExecuteCommit(ExecuteCommitRequest) -> ExecuteCommitResponse` RPC that:
- Accepts `prepare_hash` (string, hex) and `result` (bytes, guest execution result)
- Looks up the pending entry in `ReceiptEmitter.pending` by `prepare_hash` — returns `NOT_FOUND` if missing (idempotency: duplicate Commit returns the already-emitted commit receipt)
- Executes the WASM module in the **prepared sandbox** (same instance from Prepare)
- Captures actual result, path, size, `fuel_consumed` from sandbox
- **Signs a `commit` receipt** with: `phase="commit"`, `pending_hash` (copied from prepare), actual result/path/size/fuel, normal chain hash continuation
- Removes the pending entry from the map
- Returns the signed commit receipt

### REQ-734: ExecuteCommitRequest

`ExecuteCommitRequest` SHALL contain:
- `prepare_hash` (string, hex-encoded)
- `result` (bytes, guest execution result from `execute` export)

### REQ-735: ExecuteCommitResponse

`ExecuteCommitResponse` SHALL contain:
- `success` (bool)
- `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="commit"`)
- `error_message` (string, populated on failure)

### REQ-736: ExecuteAbort RPC

System SHALL implement `ExecuteAbort(ExecuteAbortRequest) -> ExecuteAbortResponse` RPC that:
- Accepts `prepare_hash` (string, hex)
- Looks up the pending entry — returns `NOT_FOUND` if missing (idempotency: duplicate Abort returns the already-emitted abort receipt)
- **Signs an `abort` receipt** with: `phase="abort"`, `pending_hash` (copied from prepare), `result="aborted"`, `path=""`, `size=0`, `fuel_consumed=0`, chain continues from abort's hash
- Discards the sandbox, removes the pending entry
- Returns the signed abort receipt

### REQ-737: ExecuteAbortRequest

`ExecuteAbortRequest` SHALL contain:
- `prepare_hash` (string, hex-encoded)

### REQ-738: ExecuteAbortResponse

`ExecuteAbortResponse` SHALL contain:
- `success` (bool)
- `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="abort"`)
- `error_message` (string, populated on failure)

### REQ-739: Two-Phase State Machine

The two-phase protocol SHALL enforce the following state machine per `pending_hash`:
- `pending` (after Prepare) → `committed` (after Commit) OR `aborted` (after Abort)
- **No Commit after Abort**: second Commit/Abort on same `pending_hash` returns the already-emitted receipt (idempotent)
- **No Prepare reuse**: each Prepare creates a unique `pending_hash` (BLAKE3 chain hash); concurrent Prepares are independent

### REQ-740: Execute RPC Deprecation (Backward Compatibility)

The legacy `Execute` RPC SHALL be retained for backward compatibility and SHALL internally:
1. Call `ExecutePrepare` logic (create sandbox, sign prepare receipt, store pending)
2. Call `ExecuteCommit` logic (execute WASM in same sandbox, sign commit receipt)
3. If Commit fails (signing failure), call `ExecuteAbort` logic (emit abort receipt with `error="commit_signing_failed"`)
4. Return the final commit/abort receipt to the caller

**NOTE**: Old clients using `Execute` continue to work. New clients SHALL use the explicit 3-RPC flow. Deprecation notice documented in protobuf comments. Removal target: Phase 11+.

---

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-900 | ExecutePrepare happy path | Valid config + capability + WASM module | Client calls ExecutePrepare | `success=true`, signed `phase="prepare"` receipt with `pending_hash`, `prepare_hash` returned |
| S-901 | ExecutePrepare invalid capability | Capability not granted in config | Client calls ExecutePrepare | `success=false`, error_message indicates capability not granted |
| S-902 | ExecutePrepare missing WASM | Valid config, empty wasm_module | Client calls ExecutePrepare | `success=false`, error_message "wasm_module is required" |
| S-903 | ExecuteCommit happy path | Prepare succeeded, pending entry exists | Client calls ExecuteCommit with prepare_hash + result | `success=true`, signed `phase="commit"` receipt with actual result/fuel, `pending_hash` links to prepare |
| S-904 | ExecuteCommit idempotent | Prepare succeeded, Commit already called | Client calls ExecuteCommit again with same prepare_hash | Returns the already-emitted commit receipt (no re-execution) |
| S-905 | ExecuteCommit not found | No pending entry for prepare_hash | Client calls ExecuteCommit | `success=false`, error_message "prepare not found" |
| S-906 | ExecuteAbort happy path | Prepare succeeded, pending entry exists | Client calls ExecuteAbort with prepare_hash | `success=true`, signed `phase="abort"` receipt, chain continues from abort hash |
| S-907 | ExecuteAbort idempotent | Prepare succeeded, Abort already called | Client calls ExecuteAbort again with same prepare_hash | Returns the already-emitted abort receipt |
| S-908 | ExecuteAbort not found | No pending entry for prepare_hash | Client calls ExecuteAbort | `success=false`, error_message "prepare not found" |
| S-909 | Prepare→Commit chain | Prepare then Commit | Client calls Prepare then Commit | Chain: prepare(prev_hash=genesis) → commit(pending_hash=prepare_hash) |
| S-910 | Prepare→Abort chain | Prepare then Abort | Client calls Prepare then Abort | Chain: prepare(prev_hash=genesis) → abort(pending_hash=prepare_hash) |
| S-911 | Legacy Execute deprecated | Server running | Client calls legacy Execute RPC | Works atomically (Prepare+Commit internally); receipt has `phase="commit"` |
| S-912 | Concurrent Prepares | Multiple concurrent ExecutePrepare calls | Each with different capability/WASM | Each gets unique prepare_hash; no cross-talk; independent pending entries |
| E-913 | D4 cascade in Commit | Commit executes WASM that traps | Trap from network / fuel / fs / signing | Classified correctly per cascade order (network first, then fuel, then fs, then signing) |
| E-914 | Budget set in Prepare | fuel_budget configured | ExecutePrepare called | Sandbox created with budget; Commit reuses same sandbox + budget |

---

## REMOVED Requirements

None.

## RENAMED Requirements

None.

---

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-715 (modified) | S-903, E-913 |
| REQ-717 (modified) | E-914 |
| REQ-730 | S-900, S-901, S-902 |
| REQ-731 | S-900 |
| REQ-732 | S-900, S-901, S-902 |
| REQ-733 | S-903, S-904, S-905, S-909 |
| REQ-734 | S-903 |
| REQ-735 | S-903, S-904, S-905 |
| REQ-736 | S-906, S-907, S-908, S-910 |
| REQ-737 | S-906 |
| REQ-738 | S-906, S-907, S-908 |
| REQ-739 | S-904, S-907, S-912 |
| REQ-740 | S-911 |