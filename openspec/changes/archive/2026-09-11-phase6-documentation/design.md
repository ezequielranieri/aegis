# Design: Phase 6 Documentation

## Technical Approach

Documentation-only phase. The completed exploration (`explore.md`) is the content source; this phase curates it into per-artifact archive files under `openspec/changes/archive/2026-09-11-phase6-documentation/` and verifies `DECISIONS.md` already carries AD-010..AD-013. No code, test, or spec changes. Mapped to spec requirements: ADR completeness (ADR-010..013), threat model three-boundary coverage, retrospective actionability, Phase 5/6 archive completeness, DECISIONS.md sequential numbering.

## Architecture Decisions

### Decision: Archive Folder Structure
**Choice**: `openspec/changes/archive/2026-09-11-phase6-documentation/` with `adrs/`, `threat-model/`, `specs/documentation/`, plus `retrospective.md`, `archive-report.md`, `explore.md`, `proposal.md` at root.
**Alternatives**: Flat archive like prior phases (phase0–3 archives are flat: proposal/design/explore/specs/tasks/verify-report).
**Rationale**: Dated-dir convention under `openspec/changes/archive/` is preserved; `adrs/` and `threat-model/` subdirs are new because tasks.md 1.3–1.7 require standalone ADR and threat-model files. First archive to split documentation artifacts — noted deviation from prior flat convention, deliberate.

### Decision: ADR Document Format
**Choice**: Archive ADRs use the existing DECISIONS.md AD format: header (Date, Phase, Status), Context, Decision, Rationale, Tradeoffs, Consequences, Traceability.
**Alternatives**: New bespoke ADR template.
**Rationale**: DECISIONS.md is the canonical ADR source — AD-010..AD-013 already appended (verified, lines 424–561, Status: Accepted, Traceability table lines 577–580). Archive ADR files MIRROR those entries verbatim (same sections, same content) as phase-local snapshots. DECISIONS.md remains the standalone source of truth. Content is copied in the same phase from the same source, so no divergence risk.

### Decision: Threat Model Format
**Choice**: `threat-model/threat-model.md` with three boundary tables (WASM sandbox, gRPC, receipt chain) — columns Threat/Mitigation/Evidence — plus a Known Gaps and Acceptable Risks table with risk-level assessment.
**Rationale**: Matches spec scenario (three-boundary structure, Threat/Mitigation/Evidence columns, Known Gaps section) and explore.md's existing 3-table + 6-gap layout. 25 threat/mitigation pairs (11 sandbox + 9 gRPC + 7 receipt, per task 1.7).

### Decision: Retrospective Format
**Choice**: `retrospective.md` with: 3-false-PASS table (#, False PASS, Root Cause, Lesson), AD-005 precedent note, "patterns that worked", "patterns needing correction" each with a concrete corrective action.
**Rationale**: Spec requires concrete corrective actions and specific (non-generic) root causes; explore.md retrospective already provides both. Kept verbatim-faithful.

### Decision: explore.md — Copy Into Archive vs Split
**Choice**: BOTH. Copy `explore.md` verbatim into the archive root (task 1.10) AND split curated deliverables into per-artifact files (tasks 1.3–1.8).
**Rationale**: tasks.md is explicit on both: 1.10 copies explore.md; 1.3–1.8 require standalone files for ADRs/threat-model/retrospective. explore.md is the raw audit-trail record (consistent with phase0 archive containing explore.md); split files are curated, reviewable artifacts. Spec's Phase 6 Archive requirement (explore.md + proposal.md + specs/) is satisfied by the copy plus `specs/documentation/spec.md` mirror (task 1.2).

## File Changes

| File | Action | Source |
|------|--------|--------|
| `archive/2026-09-11-phase6-documentation/explore.md` | Create | Verbatim copy of change `explore.md` (task 1.10) |
| `archive/.../proposal.md` | Create | Verbatim copy of change `proposal.md` — required by spec, has NO explicit task (flag for sdd-tasks) |
| `archive/.../specs/documentation/spec.md` | Create | Verbatim mirror of change spec (task 1.2) |
| `archive/.../adrs/ADR-010-host-functions-vs-wasi.md` | Create | explore.md §ADR-010 (Context/Decision/Rationale/Tradeoffs/Consequences/Source) |
| `archive/.../adrs/ADR-011-test-utils-feature-flag.md` | Create | explore.md §ADR-011 |
| `archive/.../adrs/ADR-012-mtls-over-plain-tls.md` | Create | explore.md §ADR-012 |
| `archive/.../adrs/ADR-013-scope-creep-cuts-q6.md` | Create | explore.md §ADR-013 |
| `archive/.../threat-model/threat-model.md` | Create | explore.md Threat Model section (3 tables + known gaps) |
| `archive/.../retrospective.md` | Create | explore.md Retrospective section |
| `archive/.../archive-report.md` | Create | Synthesis of tasks.md completion + verification results |

All content derived from explore.md; no new technical content invented.

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Doc diff | Only documentation changed | `git diff --stat` shows only `openspec/` files (task 4.2) |
| Regression | 90 tests unchanged | `cargo test` must still report 90 passing (task 4.3) |
| Structure | Archive completeness | `ls -R` on archive folder: explore.md, proposal.md, specs/, adrs/×4, threat-model/, retrospective.md, archive-report.md |
| Content | ADR/threat-model accuracy | Content mirrors DECISIONS.md/explore.md verbatim — no behavioral testing possible or needed |

## Threat Matrix

`N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary.` (documentation only)

## Migration / Rollout

No migration required. Phase artifacts move to `openspec/changes/archive/2026-09-11-phase6-documentation/` at close. Rollback: delete archive folder; DECISIONS.md requires no revert (AD-010..AD-013 already appended, not modified by this phase).

## Open Questions

- [ ] `proposal.md` copy into the archive is required by the spec (Phase 6 Archive requirement) but has no corresponding task in tasks.md — confirm sdd-tasks adds it to task 1.x.