# Archive Report — Phase 8: Fuel Metering

Change: `phase8-fuel-metering`
Date: 2026-09-14
Status: **PASS** (verified by verify-report.md)

---

## 1. Summary

Phase 8 Fuel Metering adds deterministic WebAssembly fuel metering to the aegis sandbox so every `Execute` RPC reports CPU consumption in its receipt via `fuel_consumed`. The change enables Wasmtime `consume_fuel` at engine creation, sets a mandatory per-execution budget via `Store::set_fuel()`, reports `fuel_consumed` on Execute receipts (success and trap paths), classifies fuel exhaustion via a dedicated D4 cascade arm returning `"fuel budget exceeded"`, and provides an operator knob `execution.fuel_budget: Option<u64>` with a calibration-backed default of 10,000,000 fuel.

Epoch interruption remains the CPU security boundary (AD-002); fuel is observability/accounting, not a replacement limit.

---

## 2. Traceability Summary (REQs → Main Specs Promoted)

| Requirement | Delta Spec | Main Spec Promoted To | Type |
|-------------|------------|----------------------|------|
| **REQ-001** (modified) | `sandbox-init/spec.md` | `openspec/specs/sandbox-init/spec.md` | MODIFIED |
| **REQ-400** (modified, doc-delta) | `receipt-verifier/spec.md` | `openspec/specs/receipt-verifier/spec.md` | MODIFIED |
| **REQ-715** (modified) | `grpc-runtime-server/spec.md` | `openspec/specs/grpc-runtime-server/spec.md` | MODIFIED |
| **REQ-717** (added) | `grpc-runtime-server/spec.md` | `openspec/specs/grpc-runtime-server/spec.md` | ADDED |
| **REQ-723** (added) | `signed-receipts/spec.md` | `openspec/specs/signed-receipts/spec.md` | ADDED |

---

## 3. Commit History (WU1..WU5 + Fix Commit)

| WU | Commits | Description |
|----|---------|-------------|
| **WU1** | `fa07c2b`, `37ba382` | Sandbox foundation: `SandboxConfig` + `DEFAULT_FUEL_BUDGET`, `consume_fuel`, `set_fuel`, `fuel_consumed()` accessor |
| **WU2** | `5d7eaea` | Config + plumbing: `ExecutionConfig.fuel_budget`, server → service plumbing |
| **WU3** | `6dd3840` | Receipts + emit: `ExecutionReceipt.fuel_consumed` skip-if-zero, 25 call sites updated |
| **WU4** | `a2d0cef` | Handler Execute + D4 fuel arm: sandbox creation with budget, post-call fuel capture, D4 cascade arm |
| **WU5** | `1cabfda` | Tests integrales + regresiones + AD-015: E2E tests (`tests/fuel.rs`), unit tests, AD-015 entry |

**Total: 5 work units, 6 commits (WU1 split into 2 commits for logical separation), all gates green.**

---

## 4. AD-015 Link

**AD-015: Fuel metering via wasmtime `consume_fuel`** — transcribed in `DECISIONS.md:631-663` (commit `1cabfda`, WU5). Full ADR with Context, Decision, Rationale, Consequences, and Traceability table. Follows Phase 7 precedent (AD-014 at `DECISIONS.md:564-607` transcribed in same commit as Phase 7 code).

---

## 5. Verification Summary

| Gate | Command | Result |
|------|---------|--------|
| **Full test suite** | `cargo test` | ✅ **135 tests passed** (38 lib + 20 config + 6 fuel + 7 grpc_boundary + 14 network_http + 36 sandbox + 14 sandbox_config) |
| **Clippy** | `cargo clippy -- -D warnings` | ✅ Clean (0 warnings, 0 errors) |
| **Format check** | `cargo fmt -- --check` | ✅ Clean (no formatting changes needed) |

### Scenario Coverage (all passing)

| Scenario | Test Name | File | Result |
|----------|-----------|------|--------|
| E-801 | `e801_no_set_fuel_instant_trap` | `src/sandbox/mod.rs` (unit) | ✅ PASS |
| E-801-inverso | `e801_inverse_set_fuel_without_consume_fuel_flag` | `src/sandbox/mod.rs` (unit) | ✅ PASS |
| S-801 | `s801_execute_success_reports_fuel` | `tests/fuel.rs` | ✅ PASS |
| S-802 | `s802_fuel_exhaustion` | `tests/fuel.rs` | ✅ PASS |
| E-803 | `e803_d4_arm_isolation_and_ordering` | `tests/fuel.rs` | ✅ PASS |
| E-804 | `e804_budget_absent_uses_default` | `tests/fuel.rs` | ✅ PASS |
| E-805 | `e805_budget_explicit_honored` | `tests/fuel.rs` | ✅ PASS |
| E-802 (golden) | `e802_golden_bytes_fuel_zero_byte_identical` | `src/receipts/mod.rs` (unit) | ✅ PASS |
| S-803 | `s803_skip_if_zero_field_order` | `src/receipts/mod.rs` (unit) | ✅ PASS |
| Regression hostile-loop | `regression_hostile_loop_epoch_still_works` | `tests/fuel.rs` | ✅ PASS |
| Golden chain backward compat | `e802_golden_bytes_fuel_zero_byte_identical` | `src/receipts/mod.rs` (unit) | ✅ PASS |

### Regression Checks

| Check | Result |
|-------|--------|
| Hostile-loop regression (infinite loop still traps via fuel or epoch, `is_err` preserved) | ✅ PASS |
| Golden chain backward compatibility (pre-change receipt chains verify unchanged) | ✅ PASS |
| Existing suite stability (122+ pre-existing tests remain green) | ✅ PASS (135 total, no regressions) |

---

## 6. Design & Calibration References

- **Design document**: `openspec/changes/archive/2026-09-14-phase8-fuel-metering/design.md` — spike evidence, exact file-level changes, test plan
- **Calibration spike (§6 design)**: Disposable spike measured real fuel consumption on wasmtime 24.0.13. Key finding: E2E-shaped executes burn max 11 fuel; hostile loop at 100M budget races with epoch (57–143ms machine-variable); at 10M budget fuel trap fires in 7–14ms deterministically before epoch. **Default: 10,000,000 fuel** (909,090× headroom over max E2E-shaped workload).

---

## 7. Main Specs Promoted (Links)

- `openspec/specs/sandbox-init/spec.md` — REQ-001 modified (engine config with `consume_fuel(true)` + mandatory `set_fuel`)
- `openspec/specs/receipt-verifier/spec.md` — REQ-400 modified (doc-delta: 8 fields + `fuel_consumed`)
- `openspec/specs/signed-receipts/spec.md` — REQ-723 added (fuel_consumed on Execute receipts, skip-if-zero, end-of-struct)
- `openspec/specs/grpc-runtime-server/spec.md` — REQ-715 modified (fuel exhaustion in D4 cascade), REQ-717 added (fuel budget knob)

---

## 8. Archived Artifacts (Complete Change Folder)

All change artifacts preserved in `openspec/changes/archive/2026-09-14-phase8-fuel-metering/`:
- `proposal.md` — original proposal with decisions D1–D7
- `specs/` — 4 delta specs (sandbox-init, receipt-verifier, signed-receipts, grpc-runtime-server)
- `design.md` — technical design with spike calibration (§6)
- `tasks.md` — WU1–WU5 task breakdown
- `verify-report.md` — PASS verification with full traceability
- `archive-report.md` — this file

---

## 9. Risks & Notes (from verify-report.md)

| Risk | Status | Notes |
|------|--------|-------|
| Fuel test warnings (unused imports in `tests/fuel.rs`) | **Low** | 6 clippy warnings in test file only. Does not affect production code or gate results. Recommended: cleanup in follow-up. |
| Test isolation (global `AEGIS_TEST_MODE` env var) | **Known** | Fuel tests use global mutex to serialize; design accepted. No flakes observed in CI runs. |
| Calibrated default (10M fuel) | **Evidence-backed** | Design spike (§6) measured max 11 fuel for E2E-shaped executes; 10M provides 909,090× headroom and deterministic pre-epoch trap at ~7–14ms. |

---

**SDD Cycle Complete** — Change archived, delta specs promoted to main specs, traceability verified, all gates green.