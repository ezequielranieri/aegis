# Archive Report: phase7-network-http

**Change**: phase7-network-http
**Project**: aegis
**Archive Date**: 2026-09-12
**Artifact Store**: hybrid (openspec filesystem + Engram)
**Change Type**: implementation (network.http capability — Rust, wasmtime 24, ureq 3, rustls 0.23 ring-only)
**Verification Verdict**: PASS (re-verify, supersedes the intermediate FAIL report)

## Intent

Reinstate the deferred `network.http` capability (ADR-013 cut item #1): WASM modules become TLS-only HTTPS clients via the `aegis_http_fetch` host function, with host allowlist, method allowlist, per-execution token-bucket rate limit, and 1 MiB response cap. Violations trap fail-closed with signed fetch receipts (S-601..S-606, REQ-601..REQ-610). Closes the threat-model gaps tracked at proposal time (network.wasm, TLS-not-mandatory, rate limiting).

## Final-State Facts

| Metric | Final Value | Source |
|--------|-------------|--------|
| Tasks | 22/22 `[x]` | `tasks.md` (persisted task artifact — Task Completion Gate passed) |
| Verify | `schema: gentle-ai.verify-result/v1`, verdict `pass`, blockers 0, critical 0 | `verify-report.md` (re-verify, supersedes prior FAIL; engram mirror #117) |
| Requirements | 10/10 (REQ-601..REQ-610) | `### Requirement:` heading count, delta/promoted spec |
| Scenario blocks | 10/10 (11 IDs S-601..S-606 + E-601..E-605 mapped onto 10 formal blocks) | `#### Scenario:` heading count + verify evidence map |
| Tests | 122/122 passing (31 lib + 20 config + 7 grpc + 14 network + 36 sandbox + 14 sandbox_config), 0 skipped | re-verify test suite (`cargo test`, exit 0, output sha256:dfc13b9a…) |
| Gate | fmt/clippy/all-targets clean; `cargo tree -i aws-lc-rs` EMPTY | re-verify Test Suite table |
| Commit chain | `26a9b53` → `8f77575` → `90b4de9`+`59e338d` → `5d22773` → `ad7aaaa` → `6a5e28f` → `07069e7` → `3b52063` → `81d75a6` → `8b19898` (HEAD) | `git log` |
| Remediation batch | `3b52063` (eprintln→tracing), `81d75a6` (REQ-610 success E2E + S-604 connect-stage), `8b19898` (ring-only pins, aws-lc-rs dropped) | launch-prompt final-state handoff + verify-report Remediation section |
| Main spec | `openspec/specs/network-http/spec.md` | created — full-spec mechanical copy, empty `diff -r` (this archive) |

## Archive Contents

`openspec/changes/archive/2026-09-12-phase7-network-http/`:

- proposal.md, design.md, tasks.md, verify-report.md
- archive-report.md (this file — additive, excluded from the readback diff)
- specs/network-http/spec.md

The archived `verify-report.md` is intentionally NOT committed: `.gitignore` treats `verify-report.md` as generated ("Verification reports (generated)"), and every prior archived change follows the same convention (disk-only presence, e.g. `2026-09-11-phase5-agent-gateway-integration/verify-report.md`).

## Synced / Promoted Specs

| Domain | Action | Details |
|--------|--------|---------|
| network-http | Created (main spec did not exist; delta was a full spec) | `openspec/specs/network-http/spec.md` — 10 requirements, 10 scenario blocks, byte-identical to the change delta (mechanical `cp` → temp, `diff -r` empty, `mv` atomic rename) |

## DECISIONS.md Updates

- **AD-014 Implementation Notes (archive closure)** added:
  - Ring-only TLS provider consequence: `rustls = { default-features = false, features = ["ring","std","tls12","logging"] }` + `tokio-rustls = { default-features = false, features = ["ring","logging","tls12"] }`; aws-lc-rs eliminated (`cargo tree -i aws-lc-rs` matches nothing, exit 101); `prefer-post-quantum` dropped (feature-tied to aws_lc_rs). Previously documented only in Cargo.toml:35-42 comments.
  - D4 chain-walk implementation note: network-first trap guard walks `std::iter::successors(e.source(), ...)` because wasmtime 24 wraps traps in error chains — the literal `msg.starts_with("network ")` never fires on the top-level `Display`; dispatch on the first chain frame starting with `network ` (endpoint/method/connection/timeout/response size/rate limit prefixes). Evidence: `src/grpc/handlers/mod.rs:342-389`.
- **AD-014 Traceability** paths updated to the archived locations (`openspec/changes/archive/2026-09-12-phase7-network-http/...`), noting the spec promotion.
- **AD-013 reference check**: `DECISIONS.md` AD-014 already cites the archived phase6 ADR path `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md`; that path exists and is unaffected by this archive — CONSISTENT, no change required.
- **design.md S-604 open question** (:110) checked off with resolution: ureq 3 distinguishes `Timeout::Connect` vs `Timeout::Global` (`src/sandbox/mod.rs:1144-1151`); both disjuncts E2E-tested (`network_stall_timeout_s604`, `network_connect_stage_timeout_s604`).

## Verification Evidence (mechanical copy contract)

All `diff -r` outputs were empty (the only passing evidence):

- Step B (spec promotion): `diff -r openspec/changes/phase7-network-http/specs/network-http/spec.md <temp>` → `DIFF_EMPTY_EXIT=0`; post-move identity re-check `diff -r <delta> openspec/specs/network-http/spec.md` → `PROMOTED_IDENTICAL_EXIT=0`.
- Step C (archive move): snapshot diff before fallback `diff -r <snapshot>/source <source>` → `FALLBACK_SOURCE_DIFF_EMPTY=0`; mandatory readback `diff -r <snapshot>/source <destination>` → `READBACK_EMPTY_EXIT=0`.
- `git mv` failed with status 128 ("source directory is empty") because no phase7 openspec file was ever tracked; the skill-mandated fallback (snapshot-verified `mv`) was used. Source directory confirmed absent after the move; destination contains the complete change folder.

## Final Counts

| Dimension | Count |
|-----------|-------|
| Tasks | 22/22 complete |
| Tests | 122/122 passing |
| Requirements | 10/10 (REQ-601..REQ-610) |
| Scenario blocks | 10/10 formal (11 IDs: S-601..S-606, E-601..E-605) |
| CRITICAL / WARNING / blockers | 0 / 0 / 0 |

## Deviations / Open Items

- **No blocking deviations.** The two prior WARNINGs (REQ-610 success E2E gap, S-604 connect-stage gap) were closed by remediation commits `81d75a6` + `3b52063` + `8b19898` and re-verified PASS; the ring-only SUGGESTION-1 is folded into AD-014 Implementation Notes (this archive).
- **Known issue (pre-existing, NOT fixed — out of archive scope)**: leftover error-path `eprintln!` at `src/grpc/handlers/mod.rs:312` (`DEBUG WASM instantiation error`) — fires only on instantiation failure, not the per-RPC hot path; the two hot-path `eprintln`s are now `tracing::debug!` (commit `3b52063`). Suggested follow-up: `tracing::error!` for consistency (an identical `tracing::error!` already logs the same error at :313).
- **Non-blocking test-hygiene suggestion (not applied)**: env cleanup in `network_execute_req610_success_via_grpc` (tests/network_http.rs:1174-1175) is not RAII — a mid-test panic could leak `AEGIS_TEST_NETWORK_PORT`/`AEGIS_TEST_CA_PEM`; structurally mitigated by the process-global `GRPC_NETWORK_ENV_LOCK` and the trap test's defensive removal.
- **Follow-ups (none blocking)**: none — all verify suggestions resolved (1 folded into AD-014, 2 tracked above, 3 tracked above, 4 checked off in design.md).

## Engram Persistence

- `sdd/phase7-network-http/archive-report` saved (type `architecture`, `capture_prompt: false`, project `aegis`).
- Engram mirror observation IDs for traceability: #101 (proposal), #102 (spec), #103 (design), #104 (tasks), #105 (apply-progress WU 1-4 + remediation), #117 (verify-report re-verify PASS, supersedes the FAIL). Filesystem copies were the authoritative read source for this run; the `apply-progress`/`verify-report` observations were NOT mutated — they remain intermediate snapshots, and this archive report is the terminal record per the Final-State Authority hierarchy.
- Prior phase5 archive inconsistency noted in the phase6 archive report (archived `2026-09-11-phase5-agent-gateway-integration/tasks.md` shows task 4.1 unchecked) is untouched per the audit-trail rule.

## Lessons Learned / Follow-Ups

- The verifier's recommendation to fold SUGGESTION-1 into AD-014 at archive time worked cleanly: Implementation Notes are the right home for dependency-graph consequences that only surface after the decision is transcribed.
- wasmtime 24 error-chain wrapping is a recurring gotcha: top-level `Display` text is not the trap message — chain-walk before matching (same class as the D4 note).
- Deliberate choice (verified against repo history): archived `verify-report.md` files stay disk-only (`.gitignore` "generated" pattern); archives are committed for all other artifacts with conventional messages.

## SDD Cycle Complete

phase7-network-http fully planned, implemented, verified, and archived. The `network.http` capability from ADR-013 item #1 is shipped: fail-closed config, TLS-only fetch host fn, per-execution rate limit, 1 MiB cap, trap receipts, and gRPC wiring all E2E-proven. Ready for the next change.