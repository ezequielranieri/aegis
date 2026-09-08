# Exploration: Phase 3 — Mature Receipts

## Current State

The receipts module (`src/receipts/mod.rs`) has a skeleton `ExecutionReceipt` struct and `ReceiptChain` that uses Ed25519 signing via `ring`. However, the actual emission mechanism is a stub: `emit_capability_event()` only calls `tracing::info!`. No signed receipts are produced or verified during sandbox execution.

**Current `ExecutionReceipt` fields:**
- `module_hash: String`, `input_hash: String`, `output_hash: String`, `timestamp: u64`, `previous_receipt_hash: Option<String>`, `signature: String`, `public_key: String`
- Signed via `ring::signature::Ed25519KeyPair::sign()`
- Hashed via `ring::digest::SHA256` (not BLAKE3 as specified in the crypto-receipts skill)
- Payload serialized as a colon-delimited string: `"{module_hash}:{input_hash}:{output_hash}:{timestamp}:{previous_receipt_hash}"`

**Current `emit_capability_event` signature:**
```rust
pub fn emit_capability_event(capability: &str, path: &str, size: u64, result: &str)
```
Only logs via `tracing::info!`. Called from `aegis_fs_read` at 5 points (before/after each validation step).

**Key finding**: `ed25519-dalek` is NOT in `Cargo.toml` — only `ring` provides Ed25519. The `ExecutionReceipt` already uses `ring::signature::Ed25519KeyPair`. The task description mentions `ed25519-dalek` but this is inconsistent with the actual dependencies.

**No `blake3` dependency exists.** The hash chain currently uses `ring::digest::SHA256`.

## Affected Areas

- `src/receipts/mod.rs` — Replace `emit_capability_event` stub with signed receipt emission
- `src/sandbox/mod.rs` — Integrate receipt emission into host function calls
- `Cargo.toml` — May need `blake3` for hash chaining; verify `ring` sufficiency for Ed25519
- `tests/sandbox.rs` — Add receipt verification tests
- `src/receipts/mod.rs` — Add standalone verifier tool/library

## Approaches

### Approach 1: Enhance Existing `ExecutionReceipt` + Replace Stub

Keep the current `ExecutionReceipt` struct and `ReceiptChain`, enhance `emit_capability_event` to create and sign receipts using the existing Ed25519 key pair. Replace `tracing::info!` with actual receipt creation.

**Pros:**
- Minimal structural change — works with existing types
- `ring` already provides Ed25519 signing — no new dependency for crypto
- `ReceiptChain` already implements hash-chain logic (`last_receipt_hash` tracking)
- `ExecutionReceipt::new()` and `verify()` already exist
- Tests can verify receipt chain integrity immediately

**Cons:**
- Current struct uses `SHA256` for hashing — crypto-receipts skill specifies BLAKE3
- Serialization uses colon-delimited string — not canonical CBOR/JSON as skill specifies
- `emit_capability_event` current signature doesn't pass a key pair — needs refactoring
- No standalone verifier exists yet

**Effort**: Low-Medium

### Approach 2: Full Restructure Per Crypto-Receipts Skill

Redesign the receipt system to match the crypto-receipts skill specification exactly:
- CBOR canonical serialization
- BLAKE3 hash chaining
- `blake3` dependency added to Cargo.toml
- Standalone verifier binary/library
- Merkle batch support
- Key rotation scheme

**Pros:**
- Matches industry best practices (BLAKE3, CBOR, merkle trees)
- Spec-compliant with crypto-receipts skill
- Future-proof — merkle batching for high-throughput scenarios
- Clear separation between receipt creation and verification

**Cons:**
- Large surface area change — touches every receipt-related code
- Adds `blake3` + `serde_cbor` dependencies
- Requires rewriting `ExecutionReceipt` serialization
- More time to implement and test
- Risk of introducing bugs in the hash chain logic

**Effort**: High

### Approach 3: Hybrid — Keep `ring`, Add `blake3`, Minimal Restructure

Keep `ring` for Ed25519 signing (proven working, already in use), add `blake3` for hash chaining, and minimally restructure the receipt format:
- Replace `SHA256` with `blake3` in `ExecutionReceipt::hash()`
- Add canonical serialization (serde_json with canonical format, or CBOR)
- Replace `emit_capability_event` stub with signed receipt creation
- Build a simple standalone verifier (CLI or library)
- Skip merkle batching (not needed for Phase 3)

**Pros:**
- Uses existing `ring` for Ed25519 (proven, tested)
- Adds only `blake3` + `serde_cbor` (or `serde_json` canonical mode) for hashing/serialization
- Incremental — doesn't rewrite everything at once
- Standalone verifier can be a simple library function + optional CLI
- Lower risk than full restructure

**Cons:**
- Not fully CBOR-compliant (if using serde_json canonical instead)
- No merkle batching (deferred to later)
- Still needs refactoring of `emit_capability_event` signature

**Effort**: Medium

## Detailed Analysis

### 1. Receipt Data Structure

The current `ExecutionReceipt` has the right fields but needs canonical serialization. The crypto-receipts skill specifies:
- `timestamp_ns` (current code uses `timestamp` = `u64` seconds — should be nanoseconds)
- `policy_hash`, `capability_grants`, `fuel_consumed`, `mem_peak`, `exit_code`, `stdout_hash`, `stderr_hash`
- Current struct has `module_hash`, `input_hash`, `output_hash` — different schema

**Recommendation**: Expand `ExecutionReceipt` to include the additional fields from the skill spec, but keep it minimal for Phase 3. The essential additions are:
- `capability_name: String` (what capability was invoked)
- `action: String` (read/write/http)
- `result: String` (Success/Trap/SizeExceeded)
- `prev_hash: [u8; 32]` (BLAKE3 hash of previous receipt — fixed-size array)
- `signature: [u8; 64]` (Ed25519 signature — fixed-size array, not hex String)

### 2. Signing Key Management

The current `ExecutionReceipt::new()` takes `&Ed25519KeyPair` as a parameter. This is a good design — the key is passed in, not stored in the receipt.

**Options for key storage in the sandbox:**

| Option | Pros | Cons |
|--------|------|------|
| Embedded in `SandboxState` | Simple, all host functions can access it | Key lives in process memory |
| External KMS | Better security | Adds dependency, network call overhead |
| Per-agent config file | Flexible, key rotation possible | Requires config loading |
| Environment variable | Simple, no file I/O | Not secure, not persistent |

**Recommendation**: Embed the signing key in `SandboxState` for Phase 3. This is the simplest approach and matches the existing pattern where `capabilities` are already stored in `SandboxState`. The key pair can be created at sandbox initialization time (`Sandbox::new_with_limits()` or `Sandbox::from_config()`). Key rotation can be addressed in a later phase.

### 3. Hash Chain Design

The current `ReceiptChain` struct tracks `last_receipt_hash: Option<String>` and updates it after each `create_receipt()`. This is correct for the hash chain concept.

**Change needed**: Replace `SHA256` with `BLAKE3` for the hash function.
- Add `blake3 = "1.0"` to Cargo.toml
- Change `ExecutionReceipt::hash()` to use `blake3::hash(&canonical_bytes)`
- Store `prev_hash` as `[u8; 32]` instead of `Option<String>` for type safety
- Genesis receipt has `prev_hash = [0u8; 32]`

### 4. Verification Tool Requirements

**Option A: Library function only**
- `ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &PublicKey) -> Result<()>`
- Stateless, pure function — easy to test
- Can be called from tests or future CLI

**Option B: CLI binary**
- `aegis verify-receipts <chain-file> <public-key>`
- Reads a JSON/CBOR file of receipts, verifies chain integrity
- Useful for external auditors

**Option C: Both**
- Library for in-process verification (tests, runtime)
- CLI for external verification (auditors, CI)

**Recommendation**: Option C — both library and CLI. The library function is essential for testing. The CLI is useful for external verification and is a small amount of work (using `clap` or manual argument parsing).

### 5. Integration with `emit_capability_event`

The current `emit_capability_event(capability, path, size, result)` is called from `aegis_fs_read` at 5 points. To replace the stub:

1. **Change signature**: Add `key_pair: &Ed25519KeyPair` and `prev_hash: Option<[u8; 32]>` parameters
2. **Or**: Store the key pair in `SandboxState` and have `emit_capability_event` access it via `caller.data()`
3. **Or**: Create a `ReceiptEmitter` struct that holds the key pair and chain state, stored in `SandboxState`

**Best approach**: Store a `ReceiptEmitter` in `SandboxState`. This keeps the host function signatures unchanged (no key pair passing through `Linker::func_wrap` closures) and centralizes receipt creation.

```rust
pub struct ReceiptEmitter {
    key_pair: Ed25519KeyPair,
    chain: ReceiptChain,
}

impl ReceiptEmitter {
    pub fn emit(&mut self, capability: &str, path: &str, size: u64, result: &str) -> Result<ExecutionReceipt> {
        let receipt = self.chain.create_receipt(...)?;
        // Store receipt hash for chain
        Ok(receipt)
    }
}
```

The `SandboxState` would contain `capabilities: Vec<CapabilityConfig>` and `receipt_emitter: Option<ReceiptEmitter>`.

### 6. Storage/Transport

**Options:**
- **In-memory only**: Receipts are generated and returned to the caller. No persistence.
- **File**: Write receipts to a JSON/CBOR file on disk.
- **Stdout**: Print receipts as JSON/CBOR to stdout.
- **Network**: Send to an external collector (OpenTelemetry already configured).

**Recommendation**: Support both in-memory (return receipts to the caller via `Sandbox` methods) and stdout/file output. The `ReceiptChain` can provide a method to serialize the chain. OpenTelemetry is already configured in the project, so receipts could also be emitted as OTLP spans — this is a natural integration.

For Phase 3, keep it simple: receipts are created in-memory during execution, and a `Sandbox::get_receipt_chain()` method returns the chain. A `Sandbox::save_receipts(path)` method writes to a JSON file.

## Recommendation

**Use Approach 3 (Hybrid)**: Keep `ring` for Ed25519, add `blake3` for hash chaining, minimal restructure. This provides the strongest security properties with the least risk and surface area change.

Specific decisions:
1. **Add `blake3` dependency** — replaces `SHA256` in hash chain
2. **Keep `ring` for Ed25519** — no need to switch to `ed25519-dalek`
3. **Store `ReceiptEmitter` in `SandboxState`** — centralizes receipt creation, keeps host function signatures clean
4. **Expand `ExecutionReceipt`** with `capability_name`, `action`, `result` fields
5. **Build verifier as library function** — `ReceiptChain::verify_chain()` — plus optional CLI
6. **Use serde_json with canonical serialization** — simpler than CBOR for Phase 3, can migrate later
7. **Store signing key in `SandboxState`** — generated at sandbox creation, passed to `ReceiptEmitter`

## Risks

- **`blake3` + `serde_cbor` dependency additions**: New dependencies introduce supply chain risk and potential compilation issues. `blake3` is well-audited and widely used, but still adds surface area.
- **`ring` vs `ed25519-dalek` discrepancy**: The task description mentions `ed25519-dalek` but it's not in Cargo.toml. Using `ring` is the existing proven approach, but this inconsistency should be resolved explicitly.
- **`emit_capability_event` refactoring**: Every call site in `aegis_fs_read` needs to be updated. The current function is called from 5 locations. If the receipt emitter is stored in `SandboxState`, the calls need access to `caller.data_mut()` — this is straightforward but requires careful implementation.
- **Canonical serialization**: Using `serde_json` instead of CBOR means the canonical form is less deterministic than CBOR. For Phase 3, JSON is acceptable and easier to debug. CBOR migration is a future improvement.
- **Thread safety**: `SandboxState` is accessed from `Caller` references. Adding a `ReceiptEmitter` with mutable state requires careful borrow management — `caller.data_mut()` gives mutable access, but the `ReceiptChain` mutation must be consistent.
- **Test coverage for receipt verification**: The standalone verifier needs comprehensive test vectors (genesis, valid chain, tampered payload, broken chain, expired timestamp, wrong key) per the crypto-receipts skill spec.

## Ready for Proposal

Yes — the analysis is clear. The hybrid approach (keep `ring`, add `blake3`, minimal restructure) provides the best balance of security, simplicity, and delivery speed. The main open question is whether to add `serde_cbor` or stick with canonical `serde_json` for serialization.

Key open questions for the proposal:
1. Should `serde_cbor` be added alongside `blake3`, or keep `serde_json` canonical for Phase 3?
2. Should the standalone verifier be a CLI binary or library-only for Phase 3?
3. Should `ed25519-dalek` replace `ring` for consistency with the task description, or keep `ring`?
