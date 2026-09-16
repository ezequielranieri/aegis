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

## RENAMED Requirements

No requirements renamed.

## REMOVED Requirements

No requirements removed.
