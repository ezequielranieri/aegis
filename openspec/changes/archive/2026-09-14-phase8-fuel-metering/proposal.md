# Proposal: Fuel Metering for CPU Consumption Reporting (Phase 8)

- **Date**: 2026-09-12
- **Status**: proposed
- **Trigger**: AD-002 (DECISIONS.md, 2026-09-06) — "Fuel metering added when receipts need CPU consumption reporting (not as security limit)". Receipts exist since Phase 5 (signed-receipts) and carry execution results since Phase 7 (network fetch, REQ-610); the deferred trigger is now present.

## Summary

Enable Wasmtime fuel metering per execution as deterministic CPU **accounting** for receipts: `ExecutionReceipt` gains `fuel_consumed` (execute-only, skip-if-zero at struct end), the engine runs with `consume_fuel(true)` + a per-store budget (default ON, generous default calibrated by a design spike), exhaustion traps deterministically and maps to a dedicated `"fuel budget exceeded"` gRPC status via an explicit D4 cascade arm. Epoch interruption remains the security boundary (AD-002); fuel complements it, it does not replace it.

## Rationale

AD-002 deferred fuel until receipts needed CPU consumption reporting. That condition is met: signed receipts with hash-chaining are the audit contract (REQ-400..412, REQ-430..433), and a consumer of CPU accounting exists — verifiers of what actually executed. Today receipts record outcomes but no computation cost; fuel closes that gap with a deterministic, reproducible per-execution number. Epoch-only interruption remains imprecise for tight loops without imports (`loop { br 0 }` is bounded only at call boundaries), but that imprecision is a *security-boundary* concern already covered by epoch; fuel is observability, not another limit.

## Intent

Give every Execute RPC receipt a verifiable `fuel_consumed` value (success and trap) so CPU consumption is part of the cryptographic audit trail, while keeping epoch interruption as the fail-closed CPU limit. Requires enabling fuel deterministically on the existing wasmtime 24 engine (no API exists to read fuel after the fact — see Explore #125), a fresh per-execution budget (D3 precedent from Phase 7), byte-compatible receipt schema evolution, and correct trap classification in the D4 cascade.

## Scope

### In Scope

- Per-execution fuel: `Config::consume_fuel(true)` + `Store::set_fuel(budget)` on the fresh per-RPC sandbox; measure `consumed = budget - Store::get_fuel()`. Budget resets per Execute RPC (fresh sandbox per request, REQ-714).
- Default ON with a generous default budget (10M estimated — calibrated with evidence in a design spike over existing fs + network E2E). Operator knob `execution.fuel_budget: Option<u64>` in `ExecutionConfig` (`src/config/runtime.rs:34-38`), plumbed via `server.rs` → `AegisRuntimeService` (currently does not receive `ExecutionConfig`, `src/grpc/handlers/mod.rs:22-25`) → sandbox creation.
- `fuel_consumed` on the Execute receipt only, success **and** trap paths (`src/grpc/handlers/mod.rs:124-211`): `#[serde(default, skip_serializing_if = "is_zero")]` field **at the end** of `ExecutionReceipt` (after `timestamp_ns`) — fuel=0 receipts serialize byte-identically to the pre-change schema, old chains keep verifying.
- Explicit fuel arm in the D4 trap cascade (`src/grpc/handlers/mod.rs:341-389`): chain-walk for `starts_with("all fuel consumed")` → gRPC status `"fuel budget exceeded"`, **after** the network guard (first branch, unchanged) and **before** the fs cascade.
- REQ-400 documentation fix (`openspec/specs/receipt-verifier/spec.md:30`): the enumeration lists 6 fields but the struct has 8 (`path`, `size` missing) — doc-delta updating it to the 8 fields + `fuel_consumed`. Verifier logic (REQ-451/452/453) does not reference the enumeration; no new verification scenarios.
- AD-015 transcribed in DECISIONS.md at design/apply (decision-log: docs in the same work unit as code; Phase 7 precedent D6).

### Out of Scope

- `fuel_consumed` in capability receipts (fs/network host functions) — v1 carries the field only on Execute receipts; host-fn receipts keep 0 fuel semantics.
- Two-phase receipts (pending/confirm) — no consumer documented (Explore #124); AD-005 gap stays as-is.
- Wall-clock metering in receipts — fuel counts wasm instructions, not host I/O; epoch covers wall-clock (Explore #125, Learned 4/5).
- Fuel as a security limit — explicitly rejected framing; epoch interruption remains the security boundary (AD-002).
- Formal benchmark/cost model for fuel units; only operational calibration of the default budget.
- Explicit `schema_version` field on receipts — superseded by byte-compatible skip-if-zero evolution (see Approach).

## Capabilities

### New Capabilities

None. No new capability spec files will be created.

### Modified Capabilities

- `sandbox-init`: engine configuration gains `consume_fuel(true)` + mandatory per-store `set_fuel(budget)` (REQ-001 delta); fuel exhaustion is a deterministic trap.
- `receipt-verifier`: REQ-400 field enumeration corrected (8 real fields + `fuel_consumed`). Doc-delta only — no verifier logic change.
- `signed-receipts`: `ExecutionReceipt` gains `fuel_consumed` (execute-only, skip-if-zero, end-of-struct); canonical bytes stay identical for fuel=0.
- `grpc-runtime-server`: `RuntimeConfig.execution.fuel_budget` knob; fuel exhaustion maps to `success=false` with `"fuel budget exceeded"` (REQ-715 extension).

## Product Decisions (confirmed)

| # | Decision | Choice | Notes |
|---|----------|--------|-------|
| D1 | Framing | Fuel = observability/accounting for CPU consumption reporting; NOT a security boundary | AD-002; epoch interruption remains the security limit; complementary, not exclusive |
| D2 | Fuel scope | Budget per Store / fresh sandbox per RPC; no global fuel | D3 token-bucket precedent (Phase 7, AD-014); per-execution semantics match receipts |
| D3 | wasmtime 24 API | `Config::consume_fuel(true)` + `Store::set_fuel(budget)`; measure `consumed = budget - Store::get_fuel()` | Verified against 24.0.13 source (config.rs:534, store.rs:852/875): `add_fuel`/`fuel_consumed` DO NOT exist; exhaustion traps ALWAYS, fixed message `"wasm trap: all fuel consumed by WebAssembly"` |
| D4 | Default state | ON with generous budget (10M est.); knob `fuel_budget: Option<u64>` in `ExecutionConfig`, plumbing server.rs → handler | Uniform accounting + deterministic trap; default-0 trap hazard (Explore #125 Learned 1) mandates the budget is always set |
| D5 | Receipt schema | `fuel_consumed` on Execute receipt only (success + trap); `#[serde(default, skip_serializing_if = "is_zero")]` at struct end | fuel=0 → byte-identical canonical bytes → old chains verifiable; capability receipts untouched in v1 |
| D6 | REQ-400 | Doc-delta in scope: enumeration → 8 fields + fuel_consumed | Stale since Phase 3 (lists 6); REQ-451/452/453 do not reference it |
| D7 | D4 cascade | Explicit fuel arm: chain-walk `starts_with("all fuel consumed")` → `"fuel budget exceeded"`, after network guard, before fs cascade | Deterministic (fixed engine literal, not guest-controlled); network guard stays the first branch (AD-014) |

## Approach

Three forks were compared; the decisions above pick one side of each:

| Fork | Option A | Option B | Chosen |
|------|----------|----------|--------|
| Fuel scope | Per-execution budget on fresh sandbox (D2) | Global fuel pool across executions | **A** — fresh sandbox per RPC (REQ-714) is the natural reset; no shared mutable state, no cross-request contention, receipts independent of concurrent load. Global pool would couple receipt values to ambient traffic and need locking. |
| Default | ON with generous budget (D4) | OFF, opt-in per request | **A** — the trigger (AD-002) is receipts needing CPU reporting; OFF means `fuel_consumed` is absent/zero and accounting is empty; uniform ON also gives a deterministic trap as a side property. OFF saved overhead the project does not currently measure. |
| Schema evolution | skip-if-zero field at struct end (D5) | Explicit `schema_version` on receipts | **A** — byte-identical old chains with zero verifier changes; a version field would fork `verify_chain` logic (REQ-451/452/453) and version negotiation without any consumer. |

Rejected framing: wall-clock metering (non-deterministic; host I/O like `http_fetch` 2-5 s burns ~0 fuel — Explore #125 Learned 4) and fuel-as-security-limit (contradicts AD-002; epoch already bounds CPU).

Implementation shape: sandbox creation applies fuel (`SandboxConfig` carries the effective budget; `new_with_config` at `src/sandbox/mod.rs:365-408` adds `consume_fuel(true)` + `set_fuel`); after `execute_func.call` returns (success **or** trap — `get_fuel` works post-trap, store alive per request), compute `consumed` and carry it to the receipt emission (`emit` signature extends with `fuel_consumed`, ~18 call sites, mechanical — Explore #125 Learned 6); the D4 fuel arm maps the trap literal.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modified | `SandboxConfig` (:25-41) + `new_with_config` (:365-408): `consume_fuel(true)` + `set_fuel(budget)`; replace the AD-002 comment (:369) |
| `src/receipts/mod.rs` | Modified | `ExecutionReceipt` (:13-25): `fuel_consumed` at end, skip-if-zero; mirror in `new` (:32-63), `canonical_bytes_for_signing` (:69-81), `verify_signature` (:89-103); `emit` (:232-268) gains fuel param |
| `src/grpc/handlers/mod.rs` | Modified | `execute` (:29-212): read fuel post-call success+trap paths; D4 fuel arm (:341-389, after network guard); trap receipt emit (:193) carries fuel; service struct (:22-25) gains budget field |
| `src/config/runtime.rs` | Modified | `ExecutionConfig` (:34-38): `fuel_budget: Option<u64>` (serde default → None) |
| `src/grpc/server.rs` | Modified | `start_server_with_emitter` (:91-104): plumb `config.execution.fuel_budget` into `AegisRuntimeService` |
| `openspec/specs/receipt-verifier/spec.md` | Modified | REQ-400 doc-delta: 8 fields + `fuel_consumed` |
| `tests/` | Modified | Fuel accounting E2E; hostile-loop fuel trap; D4 mapping regression (network-first still holds); golden bytes for fuel=0 old-chain compat; calibration measurement |

## Risks

| # | Risk | Likelihood | Mitigation |
|---|------|------------|------------|
| R1 | API mismatch: `Store::add_fuel` / `fuel_consumed` / `InstancePre::with_fuel` / `out_of_fuel_trap` do NOT exist in wasmtime 24 — the wasmtime-sandbox skill references are stale | High (was already hit in explore) | Verified against 24.0.13 source (config.rs:534, store.rs:852/875); D3 records the real API; skill staleness documented in Explore #125; design uses only `consume_fuel` + `set_fuel`/`get_fuel` |
| R2 | `consume_fuel(true)` without `set_fuel` → store default 0 fuel → EVERY execution traps instantly (122-test suite goes red) | High if D4 default-ON lands without budget | D4: budget is always set at sandbox creation (mandatory); default generous 10M est. calibrated by design spike before apply |
| R3 | `fuel_consumed` changes canonical bytes → old receipt chains fail signature/hash verification | Medium | D5: skip-if-zero at struct end → fuel=0 byte-identical; golden test against pre-change bytes; field only on execute receipts |
| R4 | Capability receipts (fs/network host fns, ~18 `emit` call sites) accidentally gain fuel semantics or the field | Low | D5 scope discipline: only Execute-path `emit` passes fuel; v1 leaves host-fn receipts at 0 |
| R5 | Misleading metric: fuel counts wasm instructions only, not wall-clock — I/O-heavy executes (`http_fetch` 2-5 s) report ~0 fuel | Medium | Documented framing (D1): fuel = deterministic instruction accounting; epoch = wall-clock boundary; spec language keeps them separate (complementary, not exclusive) |
| R6 | REQ-400 drift: spec already lists 6 fields vs 8 real; a stale enumeration misleads verifier consumers | Medium (existing) | D6 doc-delta in scope; verifier logic (REQ-451/452/453) untouched, no new scenarios |
| R7 | Knob plumbing: `AegisRuntimeService` does not receive `ExecutionConfig`; operator budget would be ignored silently | Low | New service field set in `server.rs:101-104` from `config.execution.fuel_budget`; `Option<u64>` serde default keeps old configs parsing; tests cover both None and Some paths |
| R8 | Fuel-exhaustion traps misclassified by the D4 cascade (generic branch or fs "size/exceed" arm → wrong gRPC status) | Medium | D7: explicit chain-walk arm on `starts_with("all fuel consumed")` → `"fuel budget exceeded"`; after network guard (first, AD-014), before fs cascade; deterministic fixed literal, not guest-controlled; regression test asserts network-first ordering holds |

## Open Questions

1. **Default budget value**: 10M is an estimate — the design spike must measure real `fuel_consumed` on the existing fs + network E2E suites and the proposal's placeholder gets replaced with evidence before apply. No other open items; `Some(0)` semantics (explicit zero → immediate trap) is a design-level clarification, not a scope question.

## Evidence References

- DECISIONS.md AD-002 (framing trigger: fuel when receipts need CPU reporting), AD-014 (D4 chain-walk + per-execution D3 precedent)
- Engram `sdd/phase8-fuel-metering/explore` (#125): wasmtime 24 fuel API verification, risks, approach comparison
- Engram #124: Phase 8 recommendation (fuel metering; two-phase receipts require a consumer first)
- Code: `src/sandbox/mod.rs:25-41, :365-408`; `src/receipts/mod.rs:13-25, :69-81, :200-281`; `src/grpc/handlers/mod.rs:29-212, :341-389`; `src/config/runtime.rs:34-38`; `src/grpc/server.rs:101-104`
- Specs: `openspec/specs/receipt-verifier/spec.md` (REQ-400 stale), `openspec/specs/grpc-runtime-server/spec.md`, `openspec/specs/signed-receipts/spec.md` (REQ-430/431/433), `openspec/specs/network-http/spec.md` (evolution precedent)

## Rollback Plan

Flip `consume_fuel(true)` off in `new_with_config` and drop `set_fuel`; remove `fuel_consumed` from the struct and the `emit` param (skip-if-zero keeps pre-rollback receipts valid meanwhile); drop the D4 fuel arm (cascade returns to D4 Phase 7 shape); remove `fuel_budget` from `ExecutionConfig` (serde default already tolerates absence both ways). Existing configs, chains, and verifier untouched by either direction of this change.

## Dependencies

- wasmtime 24.0.13 (already in tree) — no new crates.
- Design spike: fuel calibration over existing E2E (fs + network) to set the default budget with evidence (Open Question 1).
- AD-015 transcription in DECISIONS.md at design/apply (decision-log: same work unit as code; AD-014 D6 precedent).

## Success Criteria

- [ ] `cargo test` (existing 122 + new fuel tests) all green; `cargo clippy -- -D warnings` and `cargo fmt -- --check` clean
- [ ] Production sandbox path runs with `consume_fuel(true)` + budget; hostile loop (`tests/sandbox.rs:486-541`) still traps (fuel or epoch, `is_err` preserved)
- [ ] Execute success receipt carries measured `fuel_consumed > 0`; fuel-trap receipt carries `fuel_consumed`; fuel=0 receipts byte-identical to pre-change schema (golden test)
- [ ] Fuel exhaustion → gRPC `success=false` with `"fuel budget exceeded"`; network traps still classified by the network guard first (regression test)
- [ ] `fuel_budget: None` (absent from config) → generous default; `Some(n)` → honored; old `runtime.toml` parses unchanged
- [ ] Design spike reports measured fuel for fs + network E2E and the calibrated default (replacing the 10M placeholder)