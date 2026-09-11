# Documentation Specification

## Purpose

Define the documentation artifacts produced by Phase 6: four ADRs (AD-010 through AD-013), a threat model, a retrospective, Phase 5 archive, Phase 6 archive, and DECISIONS.md updates. All artifacts are pure documentation — no implementation changes.

## Requirements

### Requirement: ADR-010 Host Functions vs WASI

The system SHALL document the decision rationale for choosing custom host functions over WASI in AD-010. The ADR MUST cover: fine-grained capability control, security boundary clarity, path validation integration, receipt emission hooks, future extensibility. Tradeoffs (non-standard ABI, no WASI ecosystem compat, per-capability effort) MUST be explicitly stated.

#### Scenario: ADR-010 content completeness

- GIVEN AD-010 is written to DECISIONS.md
- WHEN the ADR is reviewed for completeness
- THEN it includes Context, Decision, Rationale, Tradeoffs, Consequences, and Source sections
- AND each rationale point maps to an existing code artifact (instantiate_with_capabilities, Capability enum)

### Requirement: ADR-011 test-utils Feature Flag

The system SHALL document the compile-time isolation pattern for test-only methods in AD-011. The ADR MUST cover: cfg-gate mechanism, dev-dependency pattern, explicit intent via feature name. Alternative (environment variable) rejection MUST be explained.

#### Scenario: ADR-011 isolation guarantee

- GIVEN AD-011 is written
- WHEN the ADR describes the isolation mechanism
- THEN it states that force_signing_failure is absent from release builds (zero runtime overhead)
- AND it documents that cargo test auto-enables the feature via dev-dependency

### Requirement: ADR-012 mTLS over Plain TLS

The system SHALL document the mutual TLS decision in AD-012. The ADR MUST cover: mutual authentication rationale, fail-closed default, CN/SAN validation, configurable identity. Known gap (TLS not enforced when config is None) MUST be documented.

#### Scenario: ADR-012 threat coverage

- GIVEN AD-012 is written
- WHEN the ADR describes security properties
- THEN it documents client_auth_mandatory returning true
- AND it states the AegisClientCertVerifier checks both CN and SAN

### Requirement: ADR-013 Scope Creep Cuts

The system SHALL document the Q6 scope reduction in AD-013. The ADR MUST list all five deferred items (network.http, WASI compat, fuel metering, two-phase receipts, GPU) with individual deferral rationale. The connection to Phase 5 false PASSes as motivation MUST be explicit.

#### Scenario: ADR-013 deferral completeness

- GIVEN AD-013 is written
- WHEN all five deferred items are reviewed
- THEN each item has a one-line rationale for deferral
- AND the ADR states Phase 6 is documentation-only

### Requirement: Threat Model Coverage

The system SHALL produce a threat model covering three boundaries: WASM sandbox, gRPC boundary, and receipt chain. Each boundary MUST list threats with mitigations and evidence references. Known gaps and acceptable risks MUST be documented separately with risk level assessment.

#### Scenario: Threat model three-boundary structure

- GIVEN the threat model is written
- WHEN it is reviewed for boundary coverage
- THEN it includes tables for WASM sandbox threats, gRPC boundary threats, and receipt chain threats
- AND each row has Threat, Mitigation, and Evidence columns
- AND a Known Gaps section documents residual risks

### Requirement: Retrospective Process Analysis

The system SHALL produce a retrospective documenting the three false PASSes pattern, scope creep decisions, patterns that worked, and patterns needing correction. The retrospective MUST include a table of false PASSes with root cause and lesson for each.

#### Scenario: Retrospective actionability

- GIVEN the retrospective is written
- WHEN it is reviewed for actionability
- THEN each "pattern needing correction" has a concrete corrective action stated
- AND the three false PASSes are listed with specific root causes (not generic)

### Requirement: Phase 5 Archive

The system SHALL archive Phase 5 artifacts to openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/. The archive MUST include proposal, design, specs, tasks, and verify-report. The archive is an audit trail — no modifications after archival.

#### Scenario: Phase 5 archive completeness

- GIVEN Phase 5 archive is created
- WHEN the archive folder is inspected
- THEN it contains proposal.md, design.md, specs/, tasks.md, and verify-report.md
- AND no files are modified after the archive timestamp

### Requirement: Phase 6 Archive

The system SHALL archive Phase 6 documentation to openspec/changes/archive/2026-09-11-phase6-documentation/. The archive MUST include explore.md, proposal.md, specs/, and all documentation artifacts produced by this phase.

#### Scenario: Phase 6 archive structure

- GIVEN Phase 6 archive is created
- WHEN the archive folder is inspected
- THEN it contains explore.md, proposal.md, and specs/ with the documentation domain spec

### Requirement: DECISIONS.md Updates

The system SHALL append AD-010 through AD-013 to DECISIONS.md. Each entry MUST follow the existing ADR format (number, title, status, context, decision, consequences). Entries MUST be numbered sequentially after AD-009.

#### Scenario: DECISIONS.md sequential numbering

- GIVEN DECISIONS.md is updated
- WHEN the new entries are reviewed
- THEN AD-010 follows AD-009 with no gap
- AND each ADR has status "Accepted" and includes all required sections
