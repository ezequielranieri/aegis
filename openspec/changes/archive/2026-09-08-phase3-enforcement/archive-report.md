# Archive Report: phase3-enforcement

## Final State at Close

**Change**: phase3-enforcement
**Archived to**: `openspec/changes/archive/2026-09-08-phase3-enforcement/`
**Mode**: hybrid (openspec filesystem + Engram)
**Date**: 2026-09-08

### Units Delivered

**Unit A — CapabilityConfig Refactor (Regression-Gated)**
- Renamed `CapabilityConfig.max_read_bytes` → `max_bytes` in `src/sandbox/mod.rs`
- Updated `From<&Capability>` impl: `FilesystemRead` maps `params.max_read_bytes` → `max_bytes`; `FilesystemWrite` maps `params.max_write_bytes` → `max_bytes`
- Updated `aegis_fs_read` to use `cap.max_bytes` instead of `cap.max_read_bytes`
- **Regression gate**: 72 tests passed (0 failures) — all existing tests green

**Unit B — `aegis_fs_write` Implementation**
- Implemented `aegis_fs_write` in `src/sandbox/mod.rs` with atomic write (temp file `.aegis_tmp` + `std::fs::rename`)
- Registered in `instantiate_with_capabilities()` for `FilesystemWrite` variant
- 7 receipt emission points implemented:
  1. S-1-W happy path → `result = "success"` (AFTER successful rename)
  2. S-2-W outside root → `result = "trap"`
  3. S-3-W traversal → `result = "trap"`
  4. S-5-W size exceed → `result = "trap"`
  5. Guest memory OOB → `result = "trap"`, path = ""
  6. Rename failed → `result = "trap"`
  7. S-416-W-after-rename → emit fails after rename → FAIL-CLOSED VIOLATION logged + Trap
- Symlink detection: `symlink_metadata` + `read_link` fallback (more robust than canonicalize alone)
- 8 new tests pass: `allowed_write_succeeds`, `denied_write_traps`, `traversal_write_traps`, `size_exceeded_write_traps`, `guest_memory_oob_write`, `symlink_escape_write_traps`, `receipt_s416_write_signing_failure`, `receipt_s416_write_after_rename`

## Verification Summary

| Metric | Value |
|--------|-------|
| Requirements | 15/15 ✅ |
| Scenarios | 12/12 ✅ |
| Tests | 80/80 passing ✅ |
| `cargo clippy` | Clean ✅ |
| `cargo fmt` | Clean ✅ |
| Build | Pass ✅ |
| CRITICAL findings | 0 |
| WARNING findings | 2 (S-416-W-after-rename gap, symlink detection approach) |

### Test Breakdown
- Unit tests: 72 (cargo test)
- Integration tests: 8 (cargo test --test sandbox)
- All existing tests pass (regression gate satisfied before Unit B)

## Known Limitations

### S-416-W-after-rename Gap
If receipt emission fails AFTER `std::fs::rename` succeeds, the file IS written to the host filesystem and cannot be retracted. The host function returns Trap and logs a FAIL-CLOSED VIOLATION, but the receipt chain has a gap: a real write occurred with no signed receipt. This is documented in the design (§7.3). Mitigation: idempotent retry is safe (overwrite semantics); partial fix requires two-phase commit architecture.

### Symlink Detection Approach
Uses `symlink_metadata` + `read_link` fallback rather than `canonicalize` alone. This is more robust because `canonicalize` resolves symlinks automatically and can miss symlink escape attempts. The `symlink_metadata` approach detects the symlink before canonicalize, and `read_link` confirms the target is outside `allowed_root`.

### Crash Between Temp Write and Rename
If the process crashes after writing the `.aegis_tmp` file but before `rename`, the temp file remains as an orphan in `allowed_root`. This is an acceptable limitation — the temp file is inside the root, contains valid data, and poses no security risk. Future cleanup jobs can remove orphaned temp files.

## PR Strategy Used

**Chained PRs with stacked-to-main delivery**:
- **PR 1** (~50 lines): Unit A — `max_read_bytes` → `max_bytes` refactor + regression gate. Pure refactor, no behavior change.
- **PR 2** (~520 lines): Unit B — `aegis_fs_write` implementation + 8 tests + fixture. Full implementation with atomic write, 7 receipt emission points, and S-416 patterns.
- **Chain strategy**: stacked-to-main (PR 1 merged first, PR 2 built on top)

## Source of Truth Updated

### Specs Synced
| Domain | Action | Details |
|--------|--------|---------|
| `filesystem-read` | Updated | `sdd-archive-compose` applied delta: REQ-104, REQ-204, REQ-464 modified to use `max_bytes` instead of `max_read_bytes` |
| `filesystem-write` | Created | Delta spec copied as full spec to `openspec/specs/filesystem-write/spec.md` (did not previously exist) |

### Main Spec Files
- `openspec/specs/filesystem-read/spec.md` — Updated with `max_bytes` references
- `openspec/specs/filesystem-write/spec.md` — New, contains full filesystem-write specification

## Archive Contents
- proposal.md ✅
- design.md ✅
- explore.md ✅
- tasks.md ✅ (18/18 tasks complete, all [x])
- verify-report.md ✅ (PASS: 15/15 requirements, 12/12 scenarios, 80 tests)
- specs/filesystem-read/spec.md ✅ (delta, composed into main spec)
- specs/filesystem-write/spec.md ✅ (full spec)

## Next Steps

**Phase complete.** The `filesystem.write` capability is fully implemented, verified, and archived.

**Future work** (separate proposal):
- `network.http` host function — deferred from this phase per proposal scope
- Two-phase receipt emission to close S-416-W-after-rename gap
- Orphaned temp file cleanup job
- `max_requests_per_second` separate field in `CapabilityConfig` (currently overloaded as `max_bytes`)
