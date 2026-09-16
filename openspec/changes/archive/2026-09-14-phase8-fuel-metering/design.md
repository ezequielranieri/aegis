# Design — Phase 8: Fuel Metering

Change: `phase8-fuel-metering`
Status: **Ready for review**
Predecessors: proposal.md approved; specs approved (sandbox-init, receipt-verifier, signed-receipts, grpc-runtime-server)

## 1. Purpose

Add WebAssembly fuel metering (wasmtime `consume_fuel`) to the aegis sandbox so every
`Execute` RPC reports deterministic CPU-accounting in its receipt (`fuel_consumed`),
with a generous, calibration-backed default budget and an operator knob
(`execution.fuel_budget`). Fuel is instruction accounting; epoch interruption remains
the wall-clock security boundary (AD-002). This design is the SDD technical reference
for the change: spike evidence, exact file-level changes, and the test plan.

## 2. Goals / Non-Goals

Per proposal D1–D7:

- **G1** — Every Execute receipt carries `fuel_consumed: u64` (REQ-400 modified, REQ-723).
- **G2** — Every sandboxed execution runs with a mandatory explicit fuel budget;
  omitting `set_fuel` traps deterministically (REQ-001 modified, E-801).
- **G3** — Fuel-exhaustion traps are classified by a dedicated D4 cascade arm →
  `success=false`, `error_message="fuel budget exceeded"` (REQ-715 modified, S-802, E-803).
- **G4** — Operator knob `execution.fuel_budget: Option<u64>`; absent → design-calibrated
  default; present → honored (REQ-717, E-804, E-805).
- **G5** — Capability receipts (fs/network) never carry fuel semantics (D5 scope discipline, R4).
- **NG1** — No new normative budget constant in specs; the calibrated default lives HERE
  (REQ-717) and in code.
- **NG2** — No change to verifier logic or receipt verification semantics (REQ-451/452/453, R6).
- **NG3** — Fuel does not replace epoch; it complements it (D1, AD-002, R5).

## 3. Technical Approach

Three independent layers, each with a single thread of change:

1. **Sandbox (engine/store)** — `SandboxConfig` gains `fuel_budget: Option<u64>` and
   `consume_fuel: bool` (default `true` per REQ-001). `new_with_config` /
   `new_with_limits` enable `Config::consume_fuel(true)` at engine creation and call
   `store.set_fuel(effective_budget)` for every execution — never relying on the wasmtime
   default of 0. Fuel cannot be retrofitted after engine creation: `set_fuel` on a
   store whose engine lacks `consume_fuel` returns
   `"fuel is not configured in this store"` (verified empirically in the spike, T-10;
   wasmtime 24.0.13 `runtime/store.rs:875,1815-1832`). This is why the flag must live
   in `SandboxConfig`, not be applied per-call.
2. **Receipts** — `ExecutionReceipt` gains `fuel_consumed: u64` as the last field
   (after `timestamp_ns`) with `#[serde(default, skip_serializing_if = "is_zero")]`.
   `ExecutionReceipt::new` and `ReceiptEmitter::emit` gain one parameter. All
   capability-receipt call sites pass `0`; only the two Execute emit sites in
   `handlers/mod.rs` pass the measured value. With skip-if-zero, fuel=0 receipts are
   byte-identical to the pre-change schema (D5, E-802) — old chains verify unchanged.
3. **gRPC layer** — `AegisRuntimeService` gains `fuel_budget: Option<u64>` plumbed from
   `RuntimeConfig.execution.fuel_budget` in `server.rs:96-99`. The Execute handler reads
   `sandbox.fuel_consumed()` after the call (both success and error paths) and passes it
   to the execute receipt emit. The D4 cascade gains a fuel arm between the network
   guard (first, AD-014) and the filesystem cascade (R8, E-803).

### Design decisions carried from the proposal

| ID | Decision | Where it lands |
|----|----------|----------------|
| D1 | Fuel = deterministic instruction accounting; epoch = wall-clock boundary (complementary) | Sandbox config docs, design Risks R5 |
| D2 | Wording: "fuel budget exceeded" for exhaustion | D4 arm, S-802 |
| D3 | Real wasmtime 24 API: `consume_fuel` + `set_fuel`/`get_fuel` only (no `add_fuel`, no `fuel_consumed`) | Sandbox changes, Explore #125 |
| D4 | Budget always set at creation; default generous, calibration-backed | Section 6 |
| D5 | `fuel_consumed` skip-if-zero, last field, execute-only | Receipts changes |
| D6 | REQ-400 doc-delta in scope; verifier untouched | Specs |
| D7 | Fuel arm after network guard, before fs cascade; deterministic literal match | D4 cascade |
| AD-014 | Network guard remains the first branch of D4 | Preserved, E-803 regression |

## 4. Detailed Design

### 4.1 Sandbox — `src/sandbox/mod.rs`

**`SandboxConfig` (:25-41)**:

```rust
pub struct SandboxConfig {
    pub memory_size: usize,
    pub table_elements: u32,
    pub instances: usize,
    pub memories: usize,
    pub epoch_interval: Duration,
    /// Fuel metering (phase 8): consume_fuel flag, always-on in production (REQ-001).
    pub consume_fuel: bool,          // default: true
    /// Per-execution fuel budget; None → default_fuel_budget().
    pub fuel_budget: Option<u64>,    // default: None
}
```

- `Default` keeps existing field values and adds `consume_fuel: true`, `fuel_budget: None`
  (no serde on `SandboxConfig` — it is built programmatically; there is no parse path).

**New module constant (single source of truth for the calibrated default)**:

```rust
/// Calibrated by the phase-8 design spike (Section 6): 10,000,000.
pub const DEFAULT_FUEL_BUDGET: u64 = 10_000_000;

pub fn default_fuel_budget() -> u64 { DEFAULT_FUEL_BUDGET }
```

**`Sandbox::new_with_config` (:365-408) and `new_with_limits`**:

- Replace the AD-002 comment block at :369 with the REQ-001 fuel setup.
- Pass `consume_fuel` into the engine `Config`: `config.consume_fuel(true)` (wasmtime
  24 `config.rs:534`) — always on REQ-001; gated only for test scenarios that assert
  the `"fuel is not configured"` error (T-10).
- Mutate the config to `ConsumeFuel` before `Engine::new` — the engine *must* be built
  with the option; stores cannot opt in later.
- After store creation: `store.set_fuel(config.fuel_budget.unwrap_or_else(default_fuel_budget))`
  (T-9 proves this yields a deterministic instant-trap store when omitted).
- Keep epoch setup untouched (spike ran epoch-off for deterministic fuel readings;
  production keeps `epoch_interruption(true)`).

**New accessor** (used by the Execute handler; kills budget-drift risk):

```rust
impl Sandbox {
    /// Fuel consumed by the most recent execution of this sandbox:
    /// resolved_budget - get_fuel(). 0 when fuel is not configured.
    pub fn fuel_consumed(&self) -> u64 {
        let resolved = self.budget_resolved; // captured at construction
        let remaining = self.store.get_fuel().unwrap_or(resolved);
        resolved.saturating_sub(remaining)
    }
}
```

(Budget is resolved once at construction from `fuel_budget.unwrap_or(DEFAULT_FUEL_BUDGET)`
and retained on the `Sandbox` struct — the handler never re-derives it.)

### 4.2 Receipts — `src/receipts/mod.rs`

**`ExecutionReceipt` (:13-25)** — add field at the END (D5):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub capability_name: String,
    pub action: String,
    pub result: String,
    pub path: String,
    pub size: u64,
    #[serde(with = "serde_bytes")] pub prev_hash: [u8; 32],
    #[serde(with = "serde_bytes")] pub signature: [u8; 64],
    pub timestamp_ns: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fuel_consumed: u64,
}

fn is_zero(v: &u64) -> bool { *v == 0 }
```

Ordering requirement (REQ-723): `fuel_consumed` comes after `timestamp_ns` — this is the
struct's last field, so `canonical_bytes_for_chain` (which derives from field order) and
`canonical_bytes_for_signing` (:69-81, which constructs the unsigned twin) keep
`fuel=0` receipts byte-identical to the pre-change schema (E-802).

**`ExecutionReceipt::new` (:32-63)** — add `fuel_consumed: u64` parameter; copy into the
struct before signing (the canonical bytes include it — with 0 it is skipped,
byte-identical).

**`ReceiptEmitter::emit` (:232-268)** — add `fuel_consumed: u64` parameter; forward to
`ExecutionReceipt::new`. Doc comment: capability receipts (fs/network) pass `0`; only
the Execute path passes a non-zero value (R4).

**Call-site inventory** (25 direct `.emit(` sites, all signature-touched; only two get
real fuel):

| Site | Third | Fuel value |
|------|-------|-----------|
| `src/sandbox/mod.rs` — fs_read rejects ×4 + success ×1 (:639/:657/:676/:693/:710) | capability | `0` |
| `src/sandbox/mod.rs` — fs_write rejects ×7 + success ×2 (:781/:799/:838/:879/:889/:909/:924/:972/:986) | capability | `0` |
| `src/sandbox/mod.rs` — `emit_network_receipt` helper (:1287) | capability | `0` |
| `src/grpc/handlers/mod.rs` — execute success (:160) | Execute | `sandbox.fuel_consumed()` |
| `src/grpc/handlers/mod.rs` — execute trap (:193) | Execute | `sandbox.fuel_consumed()` |
| `src/receipts/mod.rs` — tests ×8 (:540-613) | test | `0` |

(`emit_network_receipt` is called 14× from network host fns; none of those call sites
change — the helper's single internal `.emit` passes `0`.)

### 4.3 gRPC handler — `src/grpc/handlers/mod.rs`

**Service struct (:22-25)**:

```rust
pub struct AegisRuntimeService {
    pub receipt_emitter: Arc<Mutex<ReceiptEmitter>>,
    pub semaphore: Arc<Semaphore>,
    pub fuel_budget: Option<u64>,   // NEW
}
```

**`execute` (:29-212)**:

1. Sandbox creation (:73-84, both branches):
   ```rust
   let mut sandbox = Sandbox::new_with_config(
       SandboxConfig { fuel_budget: self.fuel_budget, ..SandboxConfig::default() },
       test_mode, // AEGIS_TEST_MODE keeps epoch off; fuel stays on (E-804/E-805)
   )?;
   ```
   (test_mode branch keeps `new_with_config` + the same fuel plumbing; only epoch
   differs — S-802 must trap on fuel deterministically in tests, independent of epoch.)
2. After `execute_wasm_capability` (:118-120), capture fuel ONCE before the match:
   ```rust
   let fuel_consumed = sandbox.fuel_consumed();
   ```
   Valid on both paths: `get_fuel()` succeeds even after an exhaustion trap
   (returns 0 → consumed == budget; verified in spike T-8/T-9).
3. Success emit (:159-167): pass `fuel_consumed`.
4. Trap emit (:193): pass `fuel_consumed` — S-802 receipt carries exhaustion fuel.
5. D4 cascade (:341-388) — new arm evaluated after the network guard, before the fs
   cascade (D7, AD-014):

   ```rust
   // ... existing network guard (first, unchanged) ...
   if let Some(frame) = std::iter::successors(e.source(), |s| s.source())
       .map(|s| s.to_string())
       .find(|f| f.contains("all fuel consumed"))
   {
       // Deterministic wasmtime literal (trap_encoding.rs:142) — not guest-controlled.
       return Status::failed_precondition("fuel budget exceeded");
   }
   ```

   Rationale for ARM ORDER (E-803 regression): fuel trap frames (`"wasm trap: all fuel
   consumed by WebAssembly"`) contain none of the fs-branch markers
   (`traversal`, `outside`, `size`, `exceed`, `max`, `signing`, `receipt`), so without
   this arm they fall into the generic `"WASM execution trapped"` branch (observed
   today at :329-386) — the exact misclassification R8 warns about. The network guard
   runs first because network traps never carry fuel frames; ordering is asserted by
   regression `e803_d4_arm_ordering`.

### 4.4 Config plumbing — `src/config/runtime.rs`, `src/grpc/server.rs`

**`ExecutionConfig` (:35-38)** — add field with serde default (E-804: absent key parses
unchanged):

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExecutionConfig {
    pub max_concurrent: usize,
    #[serde(default)]
    pub fuel_budget: Option<u64>,
}
```

**`start_server_with_emitter` (`src/grpc/server.rs:91-104`)** — thread through service
construction (:96-99):

```rust
let aegis_service = AegisRuntimeService {
    receipt_emitter: receipt_emitter.clone(),
    semaphore: Arc::clone(&_semaphore),
    fuel_budget: config.execution.fuel_budget,
};
```

No other constructor call sites: `AegisRuntimeService` is built only here (verified).

## 5. Verification Approach

- Unit: `receipts` determinism/golden (E-802), sandbox fuel plumbing (E-801, T-9/T-10),
  config parse (E-804/E-805).
- E2E (`tests/`): fs, network, hostile-loop suites keep passing green; S-802 runs with
  an explicit small budget; E-803 asserts arm ordering.
- Full suite gate: `cargo test` (122 tests) and `cargo clippy` clean before archive.

## 6. Calibration Spike — measured evidence

Disposable spike (`tests/fuel_spike.rs`, written, run green, then DELETED — tree left
clean) measured real fuel consumption on wasmtime 24.0.13, engine `consume_fuel(true)`,
epoch OFF (deterministic fuel readings), `StoreLimitsBuilder` mirroring production
defaults (memory 1 MiB / 2 MiB for 1-MiB-payload workloads, table 1024, instances 4,
memories 2, trap-on-grow-failure), stub host fns `aegis::fs_read`/`fs_write`/`http_fetch`
replicating the guest-visible ABI (bounds-checked, tiny host bodies → 0 fuel host-side).

| # | Workload (module shape) | Budget | Fuel consumed | Wall ms | Trapped | Notes |
|---|-------------------------|--------|---------------|---------|---------|-------|
| 1 | `grpc_boundary.rs` `safe_read_module` exact (fs read 13 B) | 100 M | **9** | 0.048 | no | worst fs E2E shape |
| 2 | fs write 11 B (`ALLOWED_WRITE` shape) | 100 M | **6** | 0.035 | no | |
| 3 | fs read 1 MiB (17 pages, 2 MiB store) | 100 M | **6** | 0.036 | no | bytes ≠ fuel (host-side copy) |
| 4 | http fetch small (25 B body) | 100 M | **11** | 0.061 | no | **worst E2E-shaped = 11 fuel** |
| 5 | http fetch 1 MiB (17 pages) | 100 M | **11** | 0.033 | no | |
| 6 | hostile `loop br 0` | 100 M | 100 M (full) | 57–143 (machine-var) | yes | literal `wasm trap: all fuel consumed by WebAssembly` |
| 7 | arithmetic compute 1 M iters (13 fuel/iter) | 100 M | 13,000,005 | 0.562 | no | representative legit guest compute |
| 8 | compute 100 K iters under small budget | 256 | 256 (full) | 0.033 | yes | exhaustion → `get_fuel()==0` post-trap → consumed == budget (S-802 math) |
| 9 | E-801: no `set_fuel` (default 0) | 0 | 0 | 0.128 | yes | instant deterministic trap |
| 10 | `set_fuel` without `consume_fuel` | — | — | — | Err | `fuel is not configured in this store` → flag MUST be engine-level (D3) |

Key findings:

1. **E2E-suite-shaped executes burn single-digit fuel** (max 11) — one host call + tail
   is ~10 wasm instructions. Host I/O (file reads, TLS fetches) costs 0 fuel.
2. **Bytes are free**: 1 MiB vs 25 B transfers differ by 0 fuel (host-side copy) — fuel
   is instruction accounting, not wall-clock or I/O (R5 evidence).
3. **Fuel burn rate** (tight loop): ~0.7–1.7 M fuel/ms across runs. **At budget 100 M,
   the fuel-vs-epoch race is machine-dependent** (57 ms vs 143 ms vs the 100 ms epoch
   interval) — a non-deterministic classification risk for S-802. **At 10 M the fuel
   trap fires in ~7–14 ms, deterministically before the epoch** → stable classification.
4. **Exhaustion math verified**: post-trap `get_fuel()==0` → `consumed == budget`
   exactly (receipt computation `budget - get_fuel()` is exact; wasmtime
   `store.rs:1216`).
5. **E-801 both directions verified**: no `set_fuel` → instant trap; hot-path
   `set_fuel` without engine flag → hard error (proves engine-level flag requirement).

### Recommended default: `10_000_000` (10 M fuel)

| Criterion | Requirement | Measured worst | 10 M gives | Verdict |
|-----------|-------------|----------------|------------|---------|
| Headroom over E2E-shaped executes | 10–100× (R2/D4) | 11 fuel | 909,090× | ✓ far exceeds |
| Legit guest compute before exhaustion | "generous" (REQ-717) | 13 fuel/iter | ~770 K iterations (~10 ms-scale) | ✓ covers every shipped workload |
| Runaway loop trap latency | fast, deterministic | 7–14 ms → **before** 100 ms epoch, always | fuel arm classifies (S-802) | ✓ |
| Operator tunability | knob honored | — | E-805 sets any `n` | ✓ |

10 M matches the proposal's estimate but now carries evidence and, critically, sits an
order of magnitude below the measured epoch race window — making `"fuel budget
exceeded"` the deterministic outcome for compute-hungry guests while epoch remains the
wall-clock backstop for I/O-bound ones (AD-002). A guest legitimately burning >10 M fuel
(~770 K arithmetic iterations) traps by design; operators raise
`execution.fuel_budget` for compute-heavy workloads (documented knob, E-805).

## 7. ADR-015 — draft (to be transcribed in apply, same work unit)

> **ADR-015: Fuel metering via wasmtime `consume_fuel`**
>
> **Status:** Draft (design) → Proposed (apply)
>
> **Context:** Execute receipts lacked any CPU-usage signal; epoch interruption bounds
> wall-clock but cannot be reported deterministically per execution. The wasmtime-sandbox
> skill suggested a stale API (`add_fuel`, `fuel_consumed`) that does not exist in
> wasmtime 24 (Explore #125, design spike R1).
>
> **Decision:** Enable wasmtime `consume_fuel` at engine creation for every sandbox
> (REQ-001), set an explicit per-execution budget via `store.set_fuel()` — never relying
> on the default 0 — with a calibration-backed default of 10 M fuel (design spike,
> Section 6). Report `fuel_consumed` on Execute receipts only (D5), last field,
> skip-if-zero, byte-identical at 0 (E-802). Classify exhaustion via a dedicated D4 arm
> (deterministic literal `all fuel consumed`) placed after the network guard and before
> the fs cascade (D7). Fuel is instruction accounting; epoch interruption remains the
> wall-clock security boundary (complementary, not exclusive — D1).
>
> **Consequences:** Deterministic per-execution CPU accounting in receipts; S-802
> exhaustion classification stable; configs without the knob keep current behavior
> (E-804); operators can raise `execution.fuel_budget` for compute-heavy guests (E-805);
> capability receipts deliberately carry 0 (R4); verifier logic untouched (R6).

## 8. Risks — status after spike

| # | Risk | Pre-spike | Post-spike status | Mitigation delivered |
|---|------|-----------|-------------------|----------------------|
| R1 | Stale wasmtime skill API | High | **Closed by evidence** | Spike T-10 + source verification (D3); only `consume_fuel`/`set_fuel`/`get_fuel` designed |
| R2 | Default-0 instant-trap suite | High | **Closed by design** | Mandatory `set_fuel` at creation; 10 M default = 909,090× headroom over max E2E-shaped (11) |
| R3 | Canonical bytes change | Medium | **Covered** | skip-if-zero last field; E-802 golden test vs pre-change bytes |
| R4 | Capability receipts gain fuel | Low | **Scope pinned** | 23 of 25 emit sites pass 0; only the 2 Execute sites pass fuel |
| R5 | Misleading metric (I/O ~0 fuel) | Medium | **Evidence-backed** | Spike rows 3/5: 1 MiB ≈ 6–11 fuel; documented D1 framing |
| R6 | REQ-400 drift | Medium | **In scope** | Doc-delta; verifier untouched |
| R7 | Knob silently ignored | Low | **Plumbed** | Service field from `config.execution.fuel_budget` (server.rs:96-99); E-804/E-805 tests |
| R8 | D4 misclassification | Medium | **De-risked** | Chain-walk fuel arm after network guard, before fs cascade; E-803 ordering regression |

## 9. Test Plan

| Scenario | Type | Where | Assertion |
|----------|------|-------|-----------|
| S-801 execute success reports fuel | E2E | `tests/fuel.rs` (new) | module with compute loop + fs read → `success=true`, receipt `fuel_consumed == budget - get_fuel()` (>0) |
| S-802 fuel exhaustion | E2E | `tests/fuel.rs` | hostile loop under explicit small budget → deterministic trap, `fuel_consumed == budget`, `success=false`, `error_message == "fuel budget exceeded"` |
| E-801 no `set_fuel` → instant trap | unit | `sandbox` unit tests | store without budget traps on first call; default-0 semantics |
| E-801-inverse `set_fuel` w/o engine flag | unit | `sandbox` unit tests | returns `"fuel is not configured in this store"` |
| E-802 golden bytes fuel=0 | unit | `src/receipts/mod.rs` tests | serialized receipt with `fuel_consumed=0` byte-equal to captured pre-change golden |
| S-803 skip-if-zero + field order | unit | `src/receipts/mod.rs` tests | serialized JSON has no `fuel_consumed` key at 0; key present and last at >0 |
| E-803 D4 arm isolation + ordering | E2E | `tests/fuel.rs` | module with fs+network caps traps on fuel → `"fuel budget exceeded"`; network trap still classified by network guard (first branch) |
| E-804 budget absent | unit | `src/config/runtime.rs` tests | `[execution]` without `fuel_budget` parses; default constant applied |
| E-805 budget explicit | unit | `src/config/runtime.rs` tests | `fuel_budget = n` parsed and honored |
| Regression | E2E | existing 122-test suite | fs, network, hostile-loop, golden-chain suites stay green; `cargo clippy` clean |

## 10. Out of Scope

- Wall-clock-based budgets / per-operation fuel weighting by bytes.
- Fuel on capability receipts (REQ-723 scope discipline).
- Verifier logic changes (REQ-451/452/453 remain untouched).
- Metric export of fuel for telemetry (can build on `fuel_consumed` later).