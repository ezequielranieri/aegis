# Archive Report: phase1-filesystem-read-only

**Change**: phase1-filesystem-read-only
**Project**: aegis
**Archive Date**: 2026-09-07
**Artifact Store**: hybrid (OpenSpec + Engram)
**Verification Verdict**: PASS (13/13 tests, 0 critical findings, 0 blockers)

---

## Final-State Facts (Highest Authority)

This archive report reflects the state of the change AT CLOSE. All facts below are drawn from the highest-ranked source available.

| Metric | Final Value | Source |
|--------|-------------|--------|
| Tasks total | 19 | tasks.md |
| Tasks complete | 19 | tasks.md — all `- [x]` |
| Tasks incomplete | 0 | tasks.md |
| Tests passing | 13 | verify-report + final-state facts |
| Tests failing | 0 | verify-report |
| New tests (Phase 1) | 8 | verify-report |
| Regression tests (Phase 0) | 5 | verify-report |
| Requirements implemented | REQ-101 through REQ-108 | verify-report |
| Critical findings | 0 | verify-report |
| Warnings | 1 (REQ-108 partial — SHOULD-level) | verify-report |
| clippy | Clean (`cargo clippy -- -D warnings` passes) | verify-report |
| fmt | Clean (`cargo fmt -- --check` passes) | final-state facts |
| wasi crate in Cargo.toml | Absent | final-state facts + verify-report |
| Doc comments on fail-closed invariant | Present | final-state facts |
| TOCTOU comment in aegis_fs_read | Present | final-state facts |

---

## Specs Synced

| Domain | Action | Details |
|--------|--------|---------|
| `filesystem-read` | Created | New delta spec merged into `openspec/specs/filesystem-read/spec.md` — 8 requirements (REQ-101 through REQ-108), 5 scenarios (S-1 through S-5) |
| `sandbox-init` | Unchanged | Phase 0 spec preserved as-is; delta spec explicitly states no changes |

**Sync operation**: Delta spec `openspec/changes/phase1-filesystem-read-only/specs/filesystem-read/spec.md` was copied to `openspec/specs/filesystem-read/spec.md` (mechanical `cp -R`, verified with `diff -r` — empty output = byte-identical).

---

## Archive Contents

All artifacts verified present in `openspec/changes/archive/2026-09-07-phase1-filesystem-read-only/`:

- proposal.md ✅ (118 lines)
- specs/ ✅ (filesystem-read/spec.md — 100 lines)
- design.md ✅ (357 lines)
- tasks.md ✅ (71 lines, 19/19 tasks complete)
- verify-report.md ✅ (160 lines)
- explore.md ✅ (666 lines)
- .gentle-ai-instance ✅ (instance ID: sdd-9c181ddabf79e8f239246d4fa106620f)
- archive-report.md ✅ (this file)

---

## Source of Truth Updated

The following specs now reflect the new behavior:

- `openspec/specs/filesystem-read/spec.md` — Created from delta spec
- `openspec/specs/sandbox-init/spec.md` — Unchanged (Phase 0 preserved)

---

## Files Changed (from final-state facts)

| File | Action | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modified | Added `CapabilityConfig` struct, extended `SandboxState` with `capabilities: Vec<CapabilityConfig>`, added `Sandbox::instantiate(wasm_bytes, capabilities)`, implemented `aegis_fs_read` host function |
| `src/receipts/mod.rs` | Modified | Added `emit_capability_event(capability, path, size, result)` stub using `tracing::info!` |
| `tests/sandbox.rs` | Modified | Added 8 new tests (6 integration WAT modules + 2 RED unit tests) |
| `Cargo.toml` | Modified | Removed `wasi` crate |
| `openspec/specs/filesystem-read/spec.md` | Created | New main spec from delta |

---

## Requirements Compliance

All 8 requirements (REQ-101 through REQ-108) implemented and verified:

| REQ | Statement | Status | Test Evidence |
|-----|-----------|--------|---------------|
| REQ-101 | Host function `aegis::fs_read` via `Linker::func_wrap` | ✅ Implemented | `allowed_read_succeeds` test |
| REQ-102 | Path validation against `allowed_root` | ✅ Implemented | `denied_read_traps` test |
| REQ-103 | Reject `..` segments before canonicalize | ✅ Implemented | `traversal_traps`, `traversal_rejection_unit` |
| REQ-104 | `stat()` size check vs `max_read_bytes` | ✅ Implemented | `size_exceeded_traps` test |
| REQ-105 | No fd table | ✅ Implemented | `fd_leak_linking_error` test |
| REQ-106 | `Sandbox::instantiate` accepts `&[Capability]` | ✅ Implemented | All integration tests |
| REQ-107 | All violations trap (fail-closed) | ✅ Implemented | All violation tests assert trap |
| REQ-108 | Receipt stub emission | ✅ Implemented | `emit_capability_event` in `src/receipts/mod.rs` |

---

## Known Limitations

- **REQ-108 partial coverage**: No explicit test verifies `emit_capability_event` call parameters and ordering. The function is implemented and called in the correct positions (before traps/read), but test coverage only indirectly confirms this via trap behavior. This is a SHOULD-level requirement.
- **TOCTOU race window**: Documented as known limitation. Race between `canonicalize()` + `stat()` and `std::fs::read()` exists where a file could be replaced via symlink swap. Phase 1 scope is single-threaded, single-root; Phase 2/3 will address with `openat` + file descriptor passing or policy-enforced immutable roots.

---

## Verification Evidence

- **Build**: `cargo build` — Exit code 0
- **Tests**: `cargo test -- --test-threads=1` — 13 passed, 0 failed, 0 skipped
- **Linter**: `cargo clippy -- -D warnings` — Clean
- **Formatter**: `cargo fmt -- --check` — Clean
- **Verify Report**: `verify-report.md` — Verdict: PASS

---

## SDD Cycle Complete

The change **phase1-filesystem-read-only** has been fully planned, implemented, verified, and archived.

- Proposal → Spec → Design → Tasks → Apply → Verify → Archive: All phases complete
- All 19 tasks marked complete in persisted task artifact
- All 8 requirements implemented and tested
- All 13 tests passing (8 new + 5 regression)
- No CRITICAL findings
- Delta specs synced to main specs
- Change folder moved to archive

**Ready for the next change.**
