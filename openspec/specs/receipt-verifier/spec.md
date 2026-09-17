# Delta: receipt-verifier — Phase 3 Mature Receipts

## ADDED Requirements

### REQ-450: ReceiptChain::verify_chain Library Function
`ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` SHALL be a library function.

### REQ-451: Ed25519 Signature Validation
Verifier SHALL validate Ed25519 signature over canonical bytes.

### REQ-452: Hash Chain Integrity Validation
Verifier SHALL validate hash chain integrity (`prev_hash` linkage).

### REQ-453: Timestamp Window Validation
Verifier SHALL reject timestamps outside ±5s window from current time.

### REQ-454: Optional CLI Verifier
Optional CLI verifier (`aegis-verify` binary) MAY wrap `verify_chain`.

### REQ-410: BLAKE3 Hash Chain
System SHALL use BLAKE3 (`blake3` crate) for content hashing — no SHA-256.

### REQ-411: prev_hash Linkage
Each receipt's `prev_hash` SHALL equal `blake3(prev_hash ‖ canonical_bytes)`.

### REQ-412: Genesis prev_hash
Genesis receipt SHALL have `prev_hash = [0u8; 32]`.
### REQ-400: ExecutionReceipt Structure

System SHALL define `ExecutionReceipt` with fields: `capability_name`, `action`, `result`, `path`, `size`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`, `fuel_consumed: u64`.

### REQ-401: Canonical JSON Serialization
System SHALL serialize receipts via canonical JSON (`serde_json`) using struct field declaration order.

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-410 | Verifier detects tampered payload | Valid chain with one modified receipt | `verify_chain()` called | Verification fails: signature mismatch |
| S-411 | Verifier detects broken chain | Valid chain with one `prev_hash` altered | `verify_chain()` called | Verification fails: hash chain mismatch |
| S-412 | Verifier detects expired timestamp | Receipt with `timestamp_ns` > 5s old | `verify_chain()` called | Verification fails: timestamp outside window |
| S-413 | Verifier detects wrong public key | Valid chain signed with key A | `verify_chain()` called with key B | Verification fails: signature mismatch |
| S-454 | CLI verifier roundtrip | Valid chain file saved to disk | `aegis-verify` reads and validates | Verification passes |
| S-940 | Legacy chain verifies |  Pre-Phase9 chain (all `phase=""`) |  `verify_chain()` called |  Valid — backward compatible |
| S-941 | Prepare→Commit valid |  Chain: [prepare, commit] with matching `pending_hash` |  `verify_chain()` called |  Valid |
| S-942 | Prepare→Abort valid |  Chain: [prepare, abort] with matching `pending_hash` |  `verify_chain()` called |  Valid |
| S-943 | Prepare→Commit chain linkage |  prepare(prev_hash=genesis) → commit(prev_hash=prepare_hash, pending_hash=prepare_hash) |  `verify_chain()` called |  Valid; chain continues from commit hash |
| S-944 | Prepare→Abort chain linkage |  prepare(prev_hash=genesis) → abort(prev_hash=prepare_hash, pending_hash=prepare_hash) |  `verify_chain()` called |  Valid; chain continues from abort hash |
| S-945 | Orphaned prepare fails |  Chain: [prepare] only (no commit/abort) |  `verify_chain()` called |  **FAIL** — orphaned prepare |
| S-946 | Commit without prepare fails |  Chain: [commit] (no preceding prepare) |  `verify_chain()` called |  **FAIL** — commit without prepare |
| S-947 | Abort without prepare fails |  Chain: [abort] (no preceding prepare) |  `verify_chain()` called |  **FAIL** — abort without prepare |
| S-948 | Prepare then prepare fails |  Chain: [prepare, prepare] (no commit/abort between) |  `verify_chain()` called |  **FAIL** — orphaned first prepare |
| S-949 | Commit after abort fails |  Chain: [prepare, abort, commit] (same pending_hash) |  `verify_chain()` called |  **FAIL** — no commit after abort |
| S-950 | Abort after commit fails |  Chain: [prepare, commit, abort] (same pending_hash) |  `verify_chain()` called |  **FAIL** — no abort after commit |
| S-951 | Idempotent Commit accepted |  Chain: [prepare, commit, commit] (identical commit bytes) |  `verify_chain()` called |  Valid — second commit is valid chain link |
| S-952 | Idempotent Abort accepted |  Chain: [prepare, abort, abort] (identical abort bytes) |  `verify_chain()` called |  Valid — second abort is valid chain link |
| S-953 | Mixed legacy + two-phase |  Chain: [legacy, prepare, commit, legacy] |  `verify_chain()` called |  Valid — legacy = implicit commit; prepare starts new flow |
| S-954 | Wrong pending_hash in commit |  prepare(p) → commit(prev_hash=wrong, pending_hash=p) |  `verify_chain()` called |  **FAIL** — prev_hash mismatch |
| S-955 | TTL abort in chain |  Chain: [prepare, abort(error="ttl_expired")] |  `verify_chain()` called |  Valid — abort is valid terminal |

## RENAMED Requirements

No requirements renamed.

## REMOVED Requirements

No requirements removed.


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


## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-400 | S-940, S-941, S-942, S-943, S-944 |
| REQ-410 | S-410, S-411 |
| REQ-411 | S-411 |
| REQ-412 | S-410 |
| REQ-450 | S-940, S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953, S-954, S-955 |
| REQ-451 | S-940..S-955 |
| REQ-452 | S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953, S-954, S-955 |
| REQ-453 | S-940..S-955 |
| REQ-454 | S-454 |
| REQ-760 | S-941, S-942, S-943, S-944, S-945, S-946, S-947, S-948, S-949, S-950, S-951, S-952, S-953 |
| REQ-761 | S-945, S-948 |
| REQ-762 | S-951, S-952 |
