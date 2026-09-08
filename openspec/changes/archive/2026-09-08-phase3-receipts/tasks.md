# Tasks: Phase 3 — Mature Receipts

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~1200-1500 (new receipt module, config extension, sandbox refactor, test overhaul, CLI binary) |
| 400-line budget risk | **High** |
| Chained PRs recommended | **Yes** |
| Suggested split | PR 1: Foundation (deps + ReceiptsConfig + ExecutionReceipt); PR 2: Core (hash chain + ReceiptEmitter + Sandbox integration); PR 3: Integration (aegis_fs_read + regression gate + verify_chain); PR 4: CLI verifier + final tests |
| Delivery strategy | auto-chain |
| Chain strategy | feature-branch-chain |
| Decision needed before apply | Yes |

```text
Decision needed before apply: Yes
Chained PRs recommended: Yes
Chain strategy: feature-branch-chain
400-line budget risk: High
```

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|---------------------|-----------------|-------------------|
| 1 | Dependencies + ReceiptsConfig + ExecutionReceipt struct | PR 1 | `cargo test --lib receipts` | Unit test: deterministic serialization | `Cargo.toml`, `src/receipts/mod.rs`, `src/config/mod.rs` |
| 2 | Hash chain + ReceiptEmitter + fail-closed emit | PR 2 | `cargo test --lib receipts` | Unit test: 2-receipt hash chain | `src/receipts/mod.rs` |
| 3 | Key management + PolicyConfig extension + 0600 + Windows | PR 2 | `cargo test --test sandbox` | Integration: key file 0600/0644 | `src/config/mod.rs`, `src/sandbox/mod.rs` |
| 4 | Sandbox integration + get_receipt_chain | PR 2 | `cargo test --lib sandbox` | Unit test: SandboxState has ReceiptEmitter | `src/sandbox/mod.rs` |
| 5 | aegis_fs_read replacement + regression gate | PR 3 | `cargo test --test sandbox` | Integration: all 5 scenarios emit receipts | `src/sandbox/mod.rs`, `tests/sandbox.rs` |
| 6 | ReceiptChain verify_chain + CLI verifier | PR 3/4 | `cargo test --lib receipts` | E2E: `cargo run --bin aegis-verify` | `src/receipts/mod.rs`, `src/bin/aegis-verify.rs` |

---

## Phase 1: Foundation / Infrastructure

- [x] **1.1** Add `blake3 = "1.0"` to `[dependencies]` in `Cargo.toml` — trace: REQ-410, test-first: `cargo check` passes (RED → GREEN)
- [x] **1.2** Add `ReceiptsConfig { key_path: PathBuf }` struct and `Option<ReceiptsConfig>` field to `PolicyConfig` in `src/config/mod.rs` with `#[serde(default)]` — trace: REQ-421, test-first: `cargo test --lib config`
- [x] **1.3** Extend `ConfigError` with platform-unsupported variant if needed — trace: REQ-426

## Phase 2: Core Implementation — Receipts & Hash Chain

- [x] **2.1** Replace `ExecutionReceipt` struct in `src/receipts/mod.rs` with canonical JSON fields: `capability_name: String`, `action: String`, `result: String`, `path: String`, `size: u64`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64` — trace: REQ-400, REQ-401, test-first: `cargo test --lib receipts::tests::deterministic_serialization` (S-414)
- [x] **2.2** Implement `ExecutionReceipt::new()` constructor and `canonical_bytes()` method using `serde_json::to_vec` — trace: REQ-403 (deterministic), test-first: serialize same receipt twice, assert identical bytes
- [x] **2.3** Implement BLAKE3 hash chain: `prev_hash = blake3::hash(&[prev_hash, canonical_bytes].concat())`, genesis `prev_hash = [0u8; 32]` — trace: REQ-410, REQ-411, REQ-412, test-first: `cargo test --lib receipts::tests::hash_chain_integrity` (S-401)
- [x] **2.4** Implement `ReceiptChain` struct with `new()`, `add_receipt()`, `verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()>` — trace: REQ-450..453, test-first: verify valid chain → Ok; tampered payload (S-410), broken chain (S-411), expired timestamp (S-412), wrong key (S-413) → all fail
- [x] **2.5** Add `ReceiptChain::verify_chain` test vectors: genesis, valid chain, tampered payload, broken chain, expired timestamp (>±5s), wrong public key — trace: crypto-receipts skill, S-410..413

## Phase 3: Key Management & ReceiptEmitter

- [x] **3.1** Implement `ReceiptEmitter::new(key_pair)` and `emit(capability, action, path, size, result) -> Result<ExecutionReceipt>` — trace: REQ-430..432, test-first: `cargo test --lib receipts::tests::receipt_emitter_emit`
- [x] **3.2** Enforce fail-closed signing failure: if `emit()` returns `Err`, caller (`aegis_fs_read`) MUST return `Trap` immediately — trace: REQ-433, S-416, test-first: corrupt key → emit fails → verify Trap
- [x] **3.3** Load `Ed25519KeyPair` from `PolicyConfig.receipts.key_path` in sandbox creation; fail-closed if missing/invalid — trace: REQ-420, REQ-424, test-first: `cargo test --test sandbox key_file_missing_fails`
- [x] **3.4** Validate key file permissions `0600` on Unix using `std::os::unix::fs::PermissionsExt` — trace: REQ-423, S-402/S-403, test-first: 0600 → success; 0644 → `ConfigError::Io`
- [x] **3.5** **Windows unsupported**: `Sandbox::new` SHALL fail immediately with `ConfigError::Io("unsupported platform")` on non-Unix — trace: REQ-426, S-415, test-first: `#[cfg(not(unix))]` test → `ConfigError::Io`
- [x] **3.6** Key file `0600` check uses `std::os::unix::fs::PermissionsExt`; NO warning log on non-Unix — trace: REQ-426, no warning emitted

## Phase 4: Sandbox Integration

- [x] **4.1** Add `receipt_emitter: Option<ReceiptEmitter>` to `SandboxState` — trace: REQ-430
- [x] **4.2** Update `Sandbox::new_with_limits()` and `Sandbox::new_with_config()` to load key from config, create `ReceiptEmitter`, store in `SandboxState` — trace: REQ-420
- [x] **4.3** Add `Sandbox::get_receipt_chain() -> &ReceiptChain` method for CLI/test access — trace: design Decision: Sandbox Integration
- [x] **4.4** Update `Sandbox::from_config()` to pass `receipts.key_path` through — trace: REQ-420, REQ-421
- [x] **4.5** Update `SandboxState` to derive `Default` with `receipt_emitter: None` — trace: design

## Phase 5: aegis_fs_read Integration & REGRESSION GATE

- [x] **5.1** Replace `emit_capability_event` calls in `aegis_fs_read` with `caller.data_mut().receipt_emitter.as_mut()?.emit(...)` — trace: REQ-440, REQ-460..463
- [x] **5.2** Implement fail-closed emit failure in `aegis_fs_read`: if `emit()` returns `Err`, immediately return `Trap` with error — trace: REQ-433, design Decision: emit_capability_event Replacement, S-416
- [x] **5.3** Verify all 5 validation points in `aegis_fs_read` emit receipts: S-1 (result="success"), S-2 path traversal (result="trap"), S-3 `..` traversal (result="trap"), S-5 size exceeded (result="trap"), S-5 guest OOB (result="trap") — trace: REQ-460..463
- [x] **5.4** S-4 (fd leak): no receipt emitted — module fails at linking before host function runs — trace: REQ-441, pure regression (0 receipts)

### REGRESSION GATE — 5 Phase 1 Receipt Tests (REQ-464)

**CRITICAL**: Replace tracing-subscriber-based `CapturedCapabilityEvent` infrastructure with receipt-chain-based verification. All 5 tests require code changes (not just recompilation):

| Test | Type | Change Required | Annotation |
|------|------|----------------|------------|
| `receipt_s1_happy_path` | **REQUIRES MODIFICATION** | Replace `CapturedCapabilityEvent` assertion with `ExecutionReceipt` verification: `result="success"`, valid Ed25519 signature, correct `prev_hash`, non-zero `timestamp_ns` | Code change: new assertion logic |
| `receipt_s2_path_traversal` | **REQUIRES MODIFICATION** | Replace tracing capture with receipt chain access: `result="trap"`, signature verified | Code change: new assertion logic |
| `receipt_s3_traversal` | **REQUIRES MODIFICATION** | Replace tracing capture with receipt chain access: `result="trap"`, `path` contains `..` | Code change: new assertion logic |
| `receipt_s4_wasi_unknown_import` | **PURE REGRESSION** | Assertion stays: 0 receipts (module fails at linking). Replace `CapturedCapabilityEvent` check with `receipt_chain.len() == 0` | Code change: mechanism swap, logic identical |
| `receipt_s5_guest_oob` | **REQUIRES MODIFICATION** | Replace tracing capture with receipt chain access: `result="trap"`, `path=""` | Code change: new assertion logic |

**Key infrastructure changes for all tests**:
- Remove `CapabilityEventLayer`, `CapabilityEventVisitor`, `with_capability_capture` (tracing-based)
- Add `with_receipt_capture()` helper that accesses `Sandbox::get_receipt_chain()`
- All tests require a 0600-permission key file for sandbox creation
- Signing failure test (`receipt_s416_signing_failure`): force corrupt key → `aegis_fs_read` returns `Trap`, no data returned to caller — trace: REQ-433, S-416

- [x] **5.5** Update `tests/sandbox.rs`: replace `with_capability_capture` infrastructure with receipt-chain-based helpers; update all 5 Phase 1 receipt tests to verify `ExecutionReceipt` fields via `Sandbox::get_receipt_chain()` — trace: REQ-464, annotation: ALL 5 tests require code changes (infrastructure + assertions), S-4 is mechanism swap only

## Phase 6: Tests — Unit + Integration + Regression + E2E

- [x] **6.1** Unit tests: `ExecutionReceipt::new()` — valid receipt creation (S-400): signature, prev_hash, timestamp_ns, canonical JSON — trace: S-400, S-414
- [x] **6.2** Unit tests: `ReceiptChain::verify_chain()` success — valid chain + correct key → Ok — trace: S-401
- [x] **6.3** Unit tests: verifier rejects tampered payload (S-410), broken chain (S-411), expired timestamp (S-412), wrong public key (S-413) — trace: S-410..413
- [x] **6.4** Integration tests: key file 0600 → success (S-402), 0644 → `ConfigError::Io` (S-403), missing → `ConfigError::Io` (S-404), invalid base64 → `ConfigError` (S-405) — trace: S-402..405
- [x] **6.5** Integration tests: Windows unsupported → `ConfigError::Io("unsupported platform")` (S-415) — trace: S-415, `#[cfg(unix)]` guard for actual execution
- [x] **6.6** Integration tests: `aegis_fs_read` emits receipts for S-1 (success), S-2/S-3/S-5 (trap) — trace: S-406..409, REQ-460..463
- [x] **6.7** Signing failure test: corrupt key on S-1 call → `aegis_fs_read` returns `Trap`, no data returned to caller (S-416) — trace: S-416, REQ-433
- [x] **6.8** Regression gate: all 5 Phase 1 receipt tests pass (S-1 through S-5) — trace: REQ-464
- [x] **6.9** E2E test: CLI verifier reads saved receipt chain, validates — trace: REQ-454, S-454

## Phase 7: CLI Verifier & Cleanup

- [x] **7.1** Create `src/bin/aegis-verify.rs` wrapping `ReceiptChain::verify_chain()` — trace: REQ-454 (MAY), S-454
- [x] **7.2** Create `openspec/config/aegis-keys.toml` example key config template (base64-encoded Ed25519 keys) — trace: design Data Flow
- [x] **7.3** Run `cargo clippy` and `cargo fmt` — clean — trace: proposal success criteria
- [x] **7.4** Verify `cargo test` passes all unit, integration, regression, and E2E tests

---

## TDD Protocol

Every task follows RED → GREEN → REFACTOR:
1. **RED**: Write failing test first (test expects the new behavior)
2. **GREEN**: Implement minimal code to pass the test
3. **REFACTOR**: Clean up, ensure `cargo clippy` and `cargo fmt` clean

### Test-Freezing Convention
Tests that require deterministic timestamps must freeze `timestamp_ns` using a test-controlled clock (e.g., inject `timestamp_ns` parameter or use `std::time::SystemTime` mocking in test harness).
