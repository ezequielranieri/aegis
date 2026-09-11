# Proposal: Phase 6 Documentation

## Intent

Phase 6 is a documentation-only phase that captures and solidifies the architectural decisions, threat model, and process retrospective from Phases 0–5. The system has 90 tests passing with a fully functional `aegis-runtime` binary, mTLS gRPC boundary, custom host functions, and signed receipt chains. This phase produces no implementation — only the documentation artifacts needed to close the SDD cycle for Phase 5 and establish a clean baseline for Phase 7.

## Scope

### In Scope
- ADR-010: Host Functions vs WASI (decision rationale, tradeoffs, consequences)
- ADR-011: test-utils Feature Flag for Signing Failure (compile-time isolation pattern)
- ADR-012: mTLS over Plain TLS (mutual auth with CN/SAN validation)
- ADR-013: Scope Creep Cuts — Q6 deferrals (network.http, WASI compat, fuel metering, two-phase receipts, GPU)
- Threat Model: Three boundaries (WASM sandbox, gRPC, receipt chain) with mitigations, evidence, known gaps
- Retrospective: Three false PASSes pattern, scope creep decision, patterns that worked/need correction
- Phase 5 Archive: Complete archive of Phase 5 artifacts to `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/`
- DECISIONS.md updates: Append AD-010 through AD-013

### Out of Scope
- Any code implementation or refactoring
- New capability implementations (network.http, fuel metering, WASI compat, two-phase receipts, GPU)
- Test changes or new tests
- Verify process improvements (documented as retrospective findings only)

## Capabilities

### New Capabilities
None

### Modified Capabilities
None

## Approach

Document the four ADRs, threat model, and retrospective from the completed exploration (`openspec/changes/2026-09-11-phase6-documentation/explore.md`) into the archive folder and update `DECISIONS.md`. No design or implementation work required — this is a pure documentation phase. The existing specs (`filesystem-read`, `filesystem-write`, `grpc-runtime-server`, `signed-receipts`, `sandbox-init`, `aegis-runtime-binary`, `receipt-verifier`) remain valid and unchanged.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `DECISIONS.md` | Modified | Append AD-010 through AD-013 |
| `openspec/changes/archive/2026-09-11-phase6-documentation/` | New | Archive folder with explore.md, ADRs, threat model, retrospective |
| `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/` | Modified | Complete Phase 5 archive (proposal, design, specs, tasks, verify-report) |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Documentation-only phase deprioritized | Medium | Treat explore.md and ADRs as first-class deliverables; explicit archive step |
| Phase 5 archive delayed further | Low | Phase 5 archive is explicitly in scope; no new dependencies |
| Threat model incomplete | Low | Current model covers three boundaries with evidence mapping; gaps documented |
| Retrospective findings not acted upon | Medium | Findings documented as process improvements for future phases, not current scope |

## Rollback Plan

If issues are found: delete the Phase 6 archive folder, revert `DECISIONS.md` to pre-AD-010 state. No code changes to revert.

## Dependencies

- Phase 5 archive must be complete (AD-009 resolved, 90 tests pass)
- Exploration document already completed at `openspec/changes/2026-09-11-phase6-documentation/explore.md`

## Success Criteria

- [ ] ADR-010 through ADR-013 appended to `DECISIONS.md`
- [ ] Phase 6 archive folder created with all documentation artifacts
- [ ] Phase 5 archive completed and moved to `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/`
- [ ] All artifacts persisted to both filesystem (openspec) and Engram (hybrid mode)
- [ ] No code changes introduced