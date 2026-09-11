# ADR-013: Scope Creep Cuts (Q6)

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
Phase 6 was originally scoped to include several features beyond documentation. Through the SDD process, these were cut to maintain focus and avoid introducing new implementation risks after Phase 5's close-call with false PASSes.

### Decision
Phase 6 is documentation-only. All implementation features are deferred to future phases.

### Items Cut from Q6 Scope
1. **`network.http` capability implementation** — Requires async HTTP client integration, URL validation, rate limiting. Deferred because it introduces new failure modes (network timeouts, DNS resolution) not covered by existing sandbox patterns.
2. **WASI compatibility layer** — Would allow existing WASI modules to run in aegis. Requires WASI host implementation or adapter. Deferred because it conflicts with the custom host function approach (AD-010) and would dilute the capability-based security model.
3. **Fuel metering** — Was deferred from Phase 0 (AD-002) as "Phase 1+". Still not implemented because epoch-only interruption is sufficient for current security requirements. Fuel metering adds per-instruction overhead and complexity without strengthening the security boundary.
4. **Two-phase receipt emission** — Proposed as a fix for the S-416-W-after-rename gap (AD-005). Requires a pending/receipt-commit protocol or external KMS with atomicity. Deferred because the current audit log + fail-closed trap approach is adequate for the threat model.
5. **GPU/compute capability** — Not in any spec. Would require Wasmtime host function for compute shaders. Deferred entirely — no spec exists.

### Rationale
- Phase 5 had 3 false PASSes (see Retrospective below) — introducing new implementation now risks repeating verification failures
- The documentation phase should solidify the architecture decisions before adding new capabilities
- Each deferred item has a clear reason for deferral and can be explored individually in future phases

### Consequences
- Phase 6 produces only documentation artifacts (explore.md, ADRs, threat model, retrospective)
- `network.http` remains a capability variant in `Capability` enum but has no host function implementation
- Fuel metering remains deferred; epoch-only continues as the CPU limit mechanism
- The S-416 receipt gap remains documented but unfixed (mitigated by audit log)

### Traceability
- Related: AD-002 (fuel metering deferred), AD-005 (S-416 receipt gap), AD-010 (host functions vs WASI)