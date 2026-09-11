# Tasks: Phase 6 Documentation

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | 300-500 |
| 400-line budget risk | Medium |
| Chained PRs recommended | No |
| Suggested split | Single PR (documentation artifacts) |
| Delivery strategy | single-pr |
| Chain strategy | pending |
| Decision needed before apply | No |

Decision needed before apply: No
Chained PRs recommended: No
Chain strategy: pending
400-line budget risk: Medium

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Phase 6 archive folder with all documentation artifacts | Single PR | `ls openspec/changes/archive/2026-09-11-phase6-documentation/` | N/A — documentation only | Delete archive folder, revert DECISIONS.md |

---

## Phase 1: Archive Documentation Artifacts

- [ ] 1.1 Create `openspec/changes/archive/2026-09-11-phase6-documentation/` directory structure with `specs/documentation/spec.md`, `adrs/`, and `threat-model/` subdirectories
- [ ] 1.2 Create `openspec/changes/archive/2026-09-11-phase6-documentation/specs/documentation/spec.md` from explore.md requirements and scenarios
- [ ] 1.3 Create `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-010-host-functions-vs-wasi.md` with rationale, tradeoffs, consequences, and traceability
- [ ] 1.4 Create `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-011-test-utils-feature-flag.md` with compile-time isolation rationale and dev-dependency pattern
- [ ] 1.5 Create `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-012-mtls-over-plain-tls.md` with CN/SAN validation rationale and mTLS tradeoffs
- [ ] 1.6 Create `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md` with Q6 deferrals (network.http, WASI, fuel metering, two-phase receipts, GPU)
- [ ] 1.7 Create `openspec/changes/archive/2026-09-11-phase6-documentation/threat-model/threat-model.md` with 3 boundaries (WASM sandbox, gRPC, receipt chain), 25+ threat/mitigation pairs, and 6 known gaps
- [ ] 1.8 Create `openspec/changes/archive/2026-09-11-phase6-documentation/retrospective.md` with 3 false PASSes pattern analysis, AD-005 precedent, patterns that worked, and patterns needing correction
- [ ] 1.9 Create `openspec/changes/archive/2026-09-11-phase6-documentation/archive-report.md` documenting completion of Phase 6 documentation artifacts
- [ ] 1.10 Copy `openspec/changes/phase6-documentation/explore.md` into the archive folder as `openspec/changes/archive/2026-09-11-phase6-documentation/explore.md`

## Phase 2: Verify DECISIONS.md Completeness

- [ ] 2.1 Verify `DECISIONS.md` contains AD-010 (Host Functions vs WASI) — already appended at line 424
- [ ] 2.2 Verify `DECISIONS.md` contains AD-011 (test-utils feature flag) — already appended at line 459
- [ ] 2.3 Verify `DECISIONS.md` contains AD-012 (mTLS over plain TLS) — already appended at line 493
- [ ] 2.4 Verify `DECISIONS.md` contains AD-013 (Scope Creep Cuts Q6) — already appended at line 529
- [ ] 2.5 Verify Traceability table in `DECISIONS.md` includes AD-010 through AD-013 mappings

## Phase 3: Archive Phase 5 Completion

- [ ] 3.1 Verify `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/` contains all required artifacts: `proposal.md`, `design.md`, `specs/`, `tasks.md`, `verify-report.md`
- [ ] 3.2 Verify Phase 5 archive includes AD-008 and AD-009 resolution documentation
- [ ] 3.3 Verify Phase 5 `tasks.md` reflects completion of all implementation tasks

## Phase 4: Artifact Persistence and Verification

- [ ] 4.1 Persist all Phase 6 documentation artifacts to Engram (hybrid mode: `sdd/phase6-documentation/tasks`)
- [ ] 4.2 Verify no code changes were introduced — `git diff --stat` should show only documentation files
- [ ] 4.3 Confirm all 90 tests still pass with `cargo test` (no code changes expected)

---

## Notes

- **Scope**: DOCUMENTATION ONLY — no implementation, no tests, no code changes
- **DECISIONS.md updates are already complete**: AD-010 through AD-013 are already appended to `DECISIONS.md` (lines 424-561)
- **Phase 5 archive is already complete**: `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/` exists with all required artifacts
- **Primary work**: Creating the Phase 6 archive folder structure and organizing documentation artifacts from `explore.md` into separate files
- **Spec file**: `openspec/changes/phase6-documentation/specs/documentation/spec.md` already exists — task 1.2 mirrors it into the archive folder
