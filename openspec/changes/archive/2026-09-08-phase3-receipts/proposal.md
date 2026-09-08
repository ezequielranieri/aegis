# Proposal: Phase 3 — Mature Receipts

## Intent

Replace the `emit_capability_event` tracing stub with cryptographically signed execution receipts that form a BLAKE3 hash chain. Every capability invocation (success, trap, size violation, path traversal, OOB) produces a verifiable receipt. An external verifier validates chain integrity without the runtime, enabling tamper-evident audit trails for all sandbox executions.

**Determinism note**: The receipt is deterministic *given a fixed timestamp*. The nanosecond timestamp is the only source of variation between otherwise identical executions — this is intentional (each receipt proves execution at a specific moment). Reproducibility for testing is achieved by freezing the clock in tests.

## Scope

### In Scope
- Expand `ExecutionReceipt` with `capability_name`, `action`, `result`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`
- Add `blake3 = "1.0"` dependency for hash chaining (replace SHA256)
- Keep `ring` for Ed25519 — no `ed25519-dalek`
- **Signing key management**: Load `Ed25519KeyPair` from separate key file at sandbox creation (fail-closed if missing). Key path specified via `PolicyConfig.receipts.key_path`. Key persists across restarts for verifiable runtime identity. No auto-generation — fail-closed if missing.
- `ReceiptEmitter` struct in `SandboxState` holding `Ed25519KeyPair` + `ReceiptChain`
- Replace `emit_capability_event` stub with signed receipt creation (new signature accesses `ReceiptEmitter` via `caller.data_mut()`)
- **REGRESSION GATE**: All 5 Phase 1 receipt tests (`receipt_s1_happy_path` through `receipt_s5_guest_oob`) must pass after replacement — same 5 scenarios (S-1 through S-5) verified with mature receipts instead of stub
- `ReceiptChain::verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` library function
- Optional CLI verifier (`cargo run --bin aegis-verify`)
- Update `aegis_fs_read` to emit mature receipts for all 5 scenarios (S-1 through S-5)
- Canonical JSON serialization via `serde_json` (struct field order) — NO `serde_cbor`

### Out of Scope
- `filesystem.write` / `network.http` enforcement (separate Surface A)
- Merkle batching for high-throughput receipts
- Key rotation / epoch-based key management
- External KMS integration
- CBOR serialization (deferred)

## Capabilities

### New Capabilities
- `signed-receipts`: Cryptographically signed execution receipts with BLAKE3 hash chain for all capability invocations
- `receipt-verifier`: Stateless library function + optional CLI to verify receipt chain integrity without runtime

### Modified Capabilities
- `filesystem-read`: Adds receipt emission for every invocation (success + all violation types)

## Approach

Hybrid approach (Approach 3 from exploration): keep `ring` for Ed25519, add `blake3` for hash chaining, minimal restructure.

1. **Dependencies**: Add `blake3 = "1.0"` to `Cargo.toml`. Keep `ring`, `serde_json`, `hex`.
2. **Receipt structure**: Redefine `ExecutionReceipt` with fixed-size arrays for `prev_hash`/`signature`, nanosecond timestamp, capability/action/result fields. Canonical serialization = `serde_json::to_vec` on struct (field declaration order = canonical order).
3. **Hash chain**: `prev_hash = blake3::hash(&[prev_hash, canonical_bytes].concat())`. Genesis = `[0u8; 32]`.
4. **Key management**: Load `Ed25519KeyPair` from `PolicyConfig.receipts.key_path` (via extended `PolicyConfig` with `[receipts]` section pointing to a separate key file). Reuses existing fallback chain + `ConfigError` fail-closed. Key persists across restarts for verifiable runtime identity.
5. **Emission**: `ReceiptEmitter::emit(capability, action, path, size, result) -> Result<ExecutionReceipt>`. Called from `aegis_fs_read` via `caller.data_mut().receipt_emitter.as_mut()`.
6. **Verification**: `ReceiptChain::verify_chain()` iterates receipts, verifies Ed25519 signature over canonical bytes, verifies `hash == blake3(prev_hash || canonical_bytes)`, checks timestamp ±5s window. CLI wraps this.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/receipts/mod.rs` | Modified | Replace stub, expand `ExecutionReceipt`, add `ReceiptEmitter`, `blake3` hash chain, verifier |
| `src/sandbox/mod.rs` | Modified | Add `ReceiptEmitter` to `SandboxState`, **load `Ed25519KeyPair` from `PolicyConfig.receipts.key_path` at sandbox creation; fail-closed if file missing or key invalid** |
| `src/config/mod.rs` | Modified | Extend `PolicyConfig` with optional `[receipts]` section (`key_path`) — reuses existing fallback chain + `ConfigError` fail-closed |
| `Cargo.toml` | Modified | Add `blake3 = "1.0"` dependency |
| `tests/sandbox.rs` | Modified | Add receipt verification tests; update capability event capture for new receipt format |
| `src/bin/aegis-verify.rs` | New | Optional CLI verifier binary |
| `openspec/config/aegis-keys.toml` | New | Example key config file (Ed25519 private/public key pair) |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| `blake3` dependency adds supply chain surface | Low | Well-audited, widely used, single-purpose |
| `emit_capability_event` refactor breaks 5 call sites | Medium | Centralize via `ReceiptEmitter` in `SandboxState`; access via `caller.data_mut()` |
| Canonical JSON serialization non-determinism | Low | Struct field order is stable in Rust; no `HashMap`/`BTreeMap` in receipt |
| Thread safety of `ReceiptEmitter` mutation | Low | `caller.data_mut()` gives exclusive access per host call; single-threaded execution |
| Verifier test vector coverage gaps | Medium | Follow crypto-receipts skill: genesis, valid chain, tampered payload, broken chain, expired timestamp, wrong key |

## Rollback Plan

1. Revert `Cargo.toml` to remove `blake3`
2. Revert `src/receipts/mod.rs` to stub implementation (tracing::info!)
3. Remove `ReceiptEmitter` from `SandboxState`
4. Restore `emit_capability_event` with original signature
5. Delete `src/bin/aegis-verify.rs` and test additions
6. Delete key config file

## Dependencies

- `blake3 = "1.0"` (new)
- Existing: `ring`, `serde_json`, `hex`, `anyhow`, `wasmtime`

### Key Management Config Format (Integrated into PolicyConfig)

The signing key is stored in a **separate file** referenced by `PolicyConfig.receipts.key_path`. The policy config only holds the path — never the private key itself.

```toml
# .aegis/config.toml (example policy config)
[[capabilities]]
name = "filesystem.read"
allowed_root = "/data"
max_read_bytes = 1048576

[receipts]
key_path = "/etc/aegis/signing_key.toml"
```

```toml
# /etc/aegis/signing_key.toml (separate file for key isolation — 0600 perms)
[signing_key]
private_key = "base64_encoded_private_key"  # 32 bytes Ed25519, base64-encoded
public_key = "base64_encoded_public_key"    # 32 bytes Ed25519, base64-encoded
```

**Fail-closed**: If `receipts.key_path` missing from policy config, or the key file missing/invalid, sandbox creation fails with `ConfigError::MissingField` / `ConfigError::Io` — no ephemeral key generation by default. Key file permissions should be `0600` (validated at load time).

## Success Criteria

- [ ] `emit_capability_event` produces verifiable signed receipt with BLAKE3 hash chain
- [ ] `ReceiptChain::verify_chain` validates chain integrity (tamper detection works)
- [ ] `aegis_fs_read` emits receipt for every invocation (Success + all 4 violation types)
- [ ] All existing tests pass + new receipt tests pass
- [ ] `cargo clippy` and `cargo fmt` clean
- [ ] Optional CLI `aegis-verify` verifies a saved receipt chain file
- [ ] **Key file permissions validated**: sandbox creation fails with `ConfigError::Io` when key file permissions are not `0600` (test creates key file with `0644` and verifies fail-closed)