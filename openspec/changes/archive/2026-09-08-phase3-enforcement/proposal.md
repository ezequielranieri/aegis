# Proposal: Phase 3 — Enforcement (Surface A)

## Intent

Add `filesystem.write` capability with signed receipt emission to close the enforcement gap. The runtime already has `ReceiptEmitter`, `ExecutionReceipt`, hash chain, and `aegis_fs_read` as the proven template. This change implements the missing host function (`aegis_fs_write`) with atomic writes, mirrors all 5 receipt emission points + fail-closed signing (S-416), and adds comprehensive tests. `network.http` is deferred to a separate proposal after this phase archives.

## Scope

### In Scope

**Unit A (Regression-gated, must pass before Unit B starts):**
- Rename `CapabilityConfig.max_read_bytes` → `max_bytes` in `src/sandbox/mod.rs`
- Update `CapabilityConfig::from` for `FilesystemRead` and `FilesystemWrite` variants (only these two are in scope; `NetworkHttp` left unchanged)
- Update all call sites including `aegis_fs_read` (reads `cap.max_bytes`)
- **A.7 REGRESSION GATE (BLOCKS Unit B):** Run ALL 71 existing tests (`cargo test`) and confirm zero failures — this is a mandatory hard gate, explicitly checked before any Unit B work begins

**Unit B:**
- Implement `aegis_fs_write` host function in `src/sandbox/mod.rs`:
  - Atomic write (temp file + rename) using `.aegis_tmp` suffix
  - Same 5 validation points + receipt emission as `aegis_fs_read` (S-1..S-5)
  - Fail-closed on signing failure (S-416 pattern, Decision 4)
  - Overwrite allowed (like `std::fs::write`)
- Tests for `filesystem.write`: S-1..S-5 + S-416 + T-6 + Symlink escape
- Add TOML fixture: `tests/fixtures/config/filesystem-write.toml`

### Out of Scope
- `network.http` — separate proposal after this phase archives
- Any HTTP client dependency (`ureq`, `reqwest`, etc.)
- `NetworkHttpParams` additions (`allowed_methods`, `timeout_seconds`, `max_response_bytes`)
- Key rotation, Merkle batching, external KMS

## Capabilities

### New Capabilities
- `filesystem-write`: Atomic filesystem write with capability-based path validation, size limits, and signed receipt emission

### Modified Capabilities
- `filesystem-read`: Updated to read `max_bytes` instead of `max_read_bytes` (Unit A refactor only, no behavior change)
- `network.http`: Unchanged in this proposal (left for future proposal)

## Approach

Sequential two-unit approach with a hard regression gate:

1. **Unit A — CapabilityConfig refactor**: Rename `max_read_bytes` → `max_bytes`, update `CapabilityConfig::from` for `FilesystemRead` and `FilesystemWrite` variants only, update all call sites including `aegis_fs_read`. This is a pure rename/refactor — no new capability code. **Gate (A.7)**: All 71 existing tests must pass before Unit B begins.

2. **Unit B — filesystem.write implementation**: Mirror `aegis_fs_read` exactly with atomic write addition (temp file in `allowed_root` with `.aegis_tmp` suffix → `std::fs::rename`). All 5 receipt emission points + S-416 fail-closed. Overwrite behavior matches `std::fs::write`. Test suite mirrors `filesystem.read` WAT pattern.

Dependencies: None new (all deps already in `Cargo.toml` — `blake3`, `ring`, `tempfile`, `tokio`, `anyhow`, `serde_json`).

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modified | Rename `max_read_bytes` → `max_bytes` (Unit A), implement `aegis_fs_write` (Unit B), update `aegis_fs_read` call sites, register in `instantiate_with_capabilities()` |
| `src/capabilities/mod.rs` | None | Types already defined; no changes needed |
| `src/receipts/mod.rs` | None | Already supports all receipt patterns |
| `src/config/mod.rs` | None | Already validates all three types |
| `tests/sandbox.rs` | Modified | Add S-1..S-5 + S-416 + T-6 + Symlink tests for `filesystem.write` |
| `tests/fixtures/config/` | New | Add `filesystem-write.toml` fixture |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| `CapabilityConfig` rename touches many call sites | Medium | Unit A regression gate (71 tests) must pass before Unit B |
| Atomic write edge cases (cross-device rename, permissions) | Low | Temp file in same `allowed_root`; cleanup on failure; `std::fs::rename` atomic within filesystem |
| Receipt emission fail-closed logic | Low | Exact copy of proven `aegis_fs_read` pattern (Decision 4) |
| TOCTOU between `stat()` and write | Low | Inherited from `filesystem.read` — acceptable for Phase 3 |

## Rollback Plan

1. Revert `src/sandbox/mod.rs` to pre-refactor state (`max_read_bytes` field, no `aegis_fs_write`)
2. Revert `tests/sandbox.rs` to remove new `filesystem.write` tests
3. Delete `tests/fixtures/config/filesystem-write.toml`
4. Run `cargo test` to confirm all 71 original tests pass

## Dependencies

- No new dependencies (all required deps already in `Cargo.toml`)

## Success Criteria

- [ ] **Unit A**: `CapabilityConfig.max_read_bytes` → `max_bytes` rename complete
- [ ] **Unit A (REGRESSION GATE)**: All 71 existing tests pass (`cargo test`) — **hard gate, blocks Unit B**
- [ ] **Unit B**: `aegis_fs_write` implemented with atomic write (`.aegis_tmp` + rename)
- [ ] **Unit B**: All 5 receipt emission points (S-1..S-5) + S-416 working for `filesystem.write`
- [ ] **Unit B**: 7 new tests pass (S-1-W through Symlink-W)
- [ ] `cargo clippy` and `cargo fmt` clean
- [ ] No regression in `filesystem.read` or receipt chain tests