# Delta: receipt-verifier — Phase 9 Two-Phase Receipts

## MODIFIED Requirements

### REQ-400: ExecutionReceipt Structure (MODIFIED)

System SHALL define `ExecutionReceipt` with fields: `capability_name`, `action`, `result`, `path`, `size`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`, `fuel_consumed: u64`.

**CHANGE**: Two new fields added **before `timestamp_ns`** (canonical order preserved):
- `phase: String` — one of `""` (legacy), `"prepare"`, `"commit"`, `"abort"`. Default `""`.
- `pending_hash: [u8; 32]` — non-zero only for `phase="prepare"`. Serialized with `skip_serializing_if = "is_zero_array"`.

### REQ-450: ReceiptChain::verify_chain Library Function (MODIFIED)

`ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` SHALL be a library function.

**CHANGE**: Extended validation logic for two-phase receipts:

1. **Legacy receipt (`phase=""` or absent)**:
   - Treated as implicit `commit` (single-phase execution)
   - Normal chain verification: `prev_hash` links to previous receipt's chain hash
   - No `pending_hash` expected (must be zero/omitted)

2. **Prepare receipt (`phase="prepare"`)**:
   - `prev_hash` links to previous receipt's chain hash (normal)
   - Compute `pending_hash = blake3(prev_hash || prepare_canonical_bytes)` — **this MUST equal `receipt.pending_hash`**
   - Store `pending_hash` as expected for next receipt
   - Next receipt MUST be `commit` or `abort` with matching `pending_hash`

3. **Commit receipt (`phase="commit"`)**:
   - `prev_hash` MUST equal the `pending_hash` from the corresponding prepare
   - `pending_hash` field MUST equal the same `pending_hash` (copied from prepare)
   - Normal chain hash update: `next_expected = blake3(pending_hash || commit_canonical_bytes)`

4. **Abort receipt (`phase="abort"`)**:
   - `prev_hash` MUST equal the `pending_hash` from the corresponding prepare
   - `pending_hash` field MUST equal the same `pending_hash` (copied from prepare)
   - Chain continues: `next_expected = blake3(pending_hash || abort_canonical_bytes)`
   - Abort is a **valid terminal** — no further commit allowed for this prepare

**VALIDATION RULES**:
- After a `prepare`, the next receipt MUST be `commit` or `abort` with matching `pending_hash` — **no orphaned prepares in a valid final chain**
- After a `commit` or `abort`, the next receipt can be a new `prepare` (new Execute flow) or end of chain
- A `commit` or `abort` without a preceding `prepare` (i.e., no expected `pending_hash`) → **verification FAIL**
- A second `commit` after an `abort` for the same `pending_hash` → **verification FAIL**
- A `prepare` followed by another `prepare` without commit/abort → **verification FAIL** (orphaned prepare)

### REQ-451: Ed25519 Signature Validation (UNCHANGED)

Verifier SHALL validate Ed25519 signature over canonical bytes. Unchanged — applies to all phases.

### REQ-452: Hash Chain Integrity Validation (MODIFIED)

Verifier SHALL validate hash chain integrity (`prev_hash` linkage).

**CHANGE**: The linkage logic now depends on `phase`:
- `phase=""` (legacy): `prev_hash` = previous receipt's chain hash (normal)
- `phase="prepare"`: `prev_hash` = previous receipt's chain hash (normal); compute and verify `pending_hash`
- `phase="commit"|"abort"`: `prev_hash` = prepare's `pending_hash`; verify `pending_hash` matches prepare's

### REQ-453: Timestamp Window Validation (UNCHANGED)

Verifier SHALL reject timestamps outside ±5s window from current time. Unchanged — applies to all phases.

---

## ADDED Requirements

### REQ-760: Two-Phase Chain State Machine in Verifier

The verifier SHALL track an internal `expected_pending_hash: Option<[u8; 32]>` during chain iteration:

- **Initial state**: `expected_pending_hash = None`
- On `phase=""` (legacy): verify normal chain; `expected_pending_hash` unchanged
- On `phase="prepare"`:
  - Verify `receipt.pending_hash == blake3(prev_hash || prepare_canonical)`
  - Set `expected_pending_hash = Some(receipt.pending_hash)`
- On `phase="commit"` or `phase="abort"`:
  - Require `expected_pending_hash.is_some()` → else FAIL "commit/abort without prepare"
  - Verify `receipt.prev_hash == expected_pending_hash.unwrap()`
  - Verify `receipt.pending_hash == expected_pending_hash.unwrap()`
  - If `phase="abort"`: set `expected_pending_hash = None` (terminal)
  - If `phase="commit"`: set `expected_pending_hash = None` (terminal)
- After `commit`/`abort`: next receipt can be `prepare` (new flow) or `phase=""` (legacy)

### REQ-761: Orphaned Prepare Detection

A chain containing a `phase="prepare"` receipt that is **not followed by a matching `commit` or `abort`** SHALL fail verification with error indicating "orphaned prepare". This ensures every two-phase flow is properly closed.

### REQ-762: Idempotency Verification

The verifier SHALL accept chains where a `commit` or `abort` receipt appears twice consecutively with the same `pending_hash` and same canonical bytes (idempotent retry). The second occurrence is a valid chain link (hash continues from the first). This matches the idempotent Commit/Abort RPC semantics.

---

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-940 | Legacy chain verifies | Pre-Phase9 chain (all `phase=""`) | `verify_chain()` called | Valid — backward compatible |
| S-941 | Prepare→Commit valid | Chain: [prepare, commit] with matching `pending_hash` | `verify_chain()` called | Valid |
| S-942 | Prepare→Abort valid | Chain: [prepare, abort] with matching `pending_hash` | `verify_chain()` called | Valid |
| S-943 | Prepare→Commit chain linkage | prepare(prev_hash=genesis) → commit(prev_hash=prepare_hash, pending_hash=prepare_hash) | `verify_chain()` called | Valid; chain continues from commit hash |
| S-944 | Prepare→Abort chain linkage | prepare(prev_hash=genesis) → abort(prev_hash=prepare_hash, pending_hash=prepare_hash) | `verify_chain()` called | Valid; chain continues from abort hash |
| S-945 | Orphaned prepare fails | Chain: [prepare] only (no commit/abort) | `verify_chain()` called | **FAIL** — orphaned prepare |
| S-946 | Commit without prepare fails | Chain: [commit] (no preceding prepare) | `verify_chain()` called | **FAIL** — commit without prepare |
| S-947 | Abort without prepare fails | Chain: [abort] (no preceding prepare) | `verify_chain()` called | **FAIL** — abort without prepare |
| S-948 | Prepare then prepare fails | Chain: [prepare, prepare] (no commit/abort between) | `verify_chain()` called | **FAIL** — orphaned first prepare |
| S-949 | Commit after abort fails | Chain: [prepare, abort, commit] (same pending_hash) | `verify_chain()` called | **FAIL** — no commit after abort |
| S-950 | Abort after commit fails | Chain: [prepare, commit, abort] (same pending_hash) | `verify_chain()` called | **FAIL** — no abort after commit |
| S-951 | Idempotent Commit accepted | Chain: [prepare, commit, commit] (identical commit bytes) | `verify_chain()` called | Valid — second commit is valid chain link |
| S-952 | Idempotent Abort accepted | Chain: [prepare, abort, abort] (identical abort bytes) | `verify_chain()` called | Valid — second abort is valid chain link |
| S-953 | Mixed legacy + two-phase | Chain: [legacy, prepare, commit, legacy] | `verify_chain()` called | Valid — legacy = implicit commit; prepare starts new flow |
| S-954 | Wrong pending_hash in commit | prepare(p) → commit(prev_hash=wrong, pending_hash=p) | `verify_chain()` called | **FAIL** — prev_hash mismatch |
| S-955 | TTL abort in chain | Chain: [prepare, abort(error="ttl_expired")] | `verify_chain()` called | Valid — abort is valid terminal |

---

## REMOVED Requirements

None.

## RENAMED Requirements

None.

---

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-400 (modified) | S-940, S-941, S-942, S-943, S-944 |
| REQ-450 (modified) | S-940, S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953, S-954, S-955 |
| REQ-451 | S-940..S-955 |
| REQ-452 (modified) | S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953, S-954, S-955 |
| REQ-453 | S-940..S-955 |
| REQ-760 | S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953 |
| REQ-761 | S-945, S-948 |
| REQ-762 | S-951, S-952 |