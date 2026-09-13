# Tasks: Phase 3 — Enforcement (filesystem.write)

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~400-500 |
| 400-line budget risk | Medium |
| Chained PRs recommended | Yes |
| Suggested split | PR 1: Unit A (regression refactor) → PR 2: Unit B (write impl + tests) |
| Delivery strategy | ask-on-risk |
| Chain strategy | stacked-to-main |
| Decision needed before apply | Yes |

```text
Decision needed before apply: Yes
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: Medium
```

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Unit A: rename + regression gate | PR 1 | `cargo test` (71 pass) | `cargo test` | Revert `src/sandbox/mod.rs` + `tests/sandbox.rs` to pre-refactor |
| 2 | Unit B: `aegis_fs_write` + 8 tests | PR 2 | `cargo test --test sandbox` | `cargo test --test sandbox` | Remove `aegis_fs_write`, WAT modules, new tests; delete fixture |

---

## Phase 1: Unit A — CapabilityConfig Refactor (Regression-Gated)

- [x] **A.1** Rename `CapabilityConfig.max_read_bytes` → `max_bytes` in `src/sandbox/mod.rs` struct definition and doc comment (line 68)
- [x] **A.2** Update `From<&Capability>` impl: `FilesystemRead` maps `params.max_read_bytes` → `max_bytes`; `FilesystemWrite` maps `params.max_write_bytes` → `max_bytes`; `NetworkHttp` unchanged (line 78, 84, 89)
- [x] **A.3** Update `aegis_fs_read` to use `cap.max_bytes` instead of `cap.max_read_bytes` in size check (line 540) and error message (line 549)
- [x] **A.4** Update `aegis_fs_read` doc comment referencing `max_read_bytes` (line 462)
- [x] **A.5** Update test assertions in `tests/sandbox.rs`: `capability_config_from_filesystem_read` (line 857), `capability_config_from_filesystem_write` (line 878), `capability_config_from_network_http` (line 899) — change `config.max_read_bytes` → `config.max_bytes`
- [x] **A.6** Verify no remaining `cap.max_read_bytes` or `config.max_read_bytes` references in `src/sandbox/mod.rs` or `tests/sandbox.rs` (grep)
- [x] **A.7** 🚦 **REGRESSION GATE**: Run `cargo test` — all 71 existing tests must pass with zero failures. **Blocks all Unit B work.** ✅ 72 tests pass (0 failures)

---

## Phase 2: Unit B — `aegis_fs_write` Implementation

- [x] **B.1** Implement `aegis_fs_write` function in `src/sandbox/mod.rs` per design §5.2 pseudocode: get capability config, validate guest memory bounds for path and data, reject `..` segments (S-3-W), canonicalize + root check (S-2-W), size enforcement (S-5-W), create `.aegis_tmp` temp file, atomic rename, emit success receipt AFTER rename
- [x] **B.2** Register `aegis_fs_write` in `instantiate_with_capabilities()` for `Capability::FilesystemWrite(_)` variant via `linker.func_wrap("aegis", "fs_write", ...)` (replacing the current empty comment block at line 393-395)
- [x] **B.3** Create `tests/fixtures/config/filesystem-write.toml` with `[[capabilities]]` section: `name = "filesystem.write"`, `allowed_root = "/tmp/aegis-test-write"`, `max_write_bytes = 1048576`

---

## Phase 3: Unit B — Test Implementation (8 New Tests)

> Each test verifies one scenario from the spec. Uses existing WAT + `parse_str` + `sandbox_with_receipts` patterns from `tests/sandbox.rs`.

- [x] **B.4** `allowed_write_succeeds` (S-1-W): WAT module writes "hello world" to "output.txt" within root, data ≤ limit. Assert `result.is_ok()`, file exists at `allowed_root/output.txt`, receipt `result = "success"`.
- [x] **B.5** `denied_write_traps` (S-2-W): WAT module writes "../secret.txt". Assert `result.is_err()`, receipt `result = "trap"`.
- [x] **B.6** `traversal_write_traps` (S-3-W): WAT module writes "../../etc/passwd". Assert `result.is_err()` with "path traversal attempt", receipt `result = "trap"`.
- [x] **B.7** `size_exceeded_write_traps` (S-5-W): WAT module writes 2048 bytes with 1024 limit. Assert `result.is_err()`, receipt `result = "trap"`.
- [x] **B.8** `guest_memory_oob_write` (T-6-W): WAT module with `data_ptr` beyond 64KB. Assert `result.is_err()`, receipt `result = "trap"`, path = "".
- [x] **B.9** `symlink_escape_write_traps` (Symlink-W): Create symlink inside root pointing outside. Assert `result.is_err()`, receipt `result = "trap"`.
- [x] **B.10** `receipt_s416_write_signing_failure` (S-416-W): Force signing failure via `force_signing_failure()` before calling `fs_write`. Assert `result.is_err()`, NO new receipt added (chain.len() unchanged). Note: file IS written (design §7.3 known limitation — temp+rename before emit).
- [x] **B.11** `receipt_s416_write_after_rename` (S-416-W-after-rename): First emit succeeds (verify file written), then force signing failure on second emit. Assert `result.is_err()`, file IS at target path, FAIL-CLOSED VIOLATION logged.

---

## Dependencies

```
A.7 (regression gate) ──BLOCKS──▶ all B.1–B.11 tasks
B.1 ──depends-on──▶ A.1, A.2, A.3 (rename must be complete)
B.2 ──depends-on──▶ B.1
B.3 ──independent── (can be done in parallel with B.1)
B.4–B.11 ──depends-on──▶ B.1, B.2
```

## Rollback Plan

1. Revert `src/sandbox/mod.rs` to pre-refactor state (`CapabilityConfig.max_read_bytes`, no `aegis_fs_write`, empty `FilesystemWrite` branch)
2. Revert `tests/sandbox.rs`: remove all `filesystem.write` WAT modules (`ALLOWED_WRITE`, `DENIED_WRITE`, etc.) and 8 new test functions; revert `config.max_read_bytes` assertions
3. Delete `tests/fixtures/config/filesystem-write.toml`
4. Verify: `cargo test` → all 71 original tests pass
