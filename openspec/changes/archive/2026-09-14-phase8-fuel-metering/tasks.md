# Tasks — Phase 8: Fuel Metering

Change: `phase8-fuel-metering`
Status: **Ready for apply**
Predecessors: proposal.md approved; specs approved (sandbox-init, receipt-verifier, signed-receipts, grpc-runtime-server); design.md approved

---

## Work Units Overview

| WU | Name | Dependencies | Primary Files | Gate |
|----|------|--------------|---------------|------|
| 1 | Sandbox foundation | — | `src/sandbox/mod.rs` | `cargo test` + clippy/fmt |
| 2 | Config + plumbing | WU1 | `src/config/runtime.rs`, `src/grpc/server.rs`, `src/grpc/handlers/mod.rs` | `cargo test` + clippy/fmt |
| 3 | Receipts + emit | WU1 | `src/receipts/mod.rs`, 25 call sites | `cargo test` + clippy/fmt |
| 4 | Handler Execute + D4 fuel arm | WU1, WU2, WU3 | `src/grpc/handlers/mod.rs` | `cargo test` (E2E) + clippy/fmt |
| 5 | Tests integrales + regresiones + AD-015 | WU1–WU4 | Full suite, `DECISIONS.md` | `cargo test` + clippy/fmt → verde completo |

---

## WU1 — Sandbox Foundation (prereq de todo)

**Objetivo**: Motor Wasmtime con `consume_fuel(true)` + `set_fuel` obligatorio en cada store, `DEFAULT_FUEL_BUDGET = 10_000_000`, accessor `Sandbox::fuel_consumed()`.

| Task | Descripción | Archivos | Validación | Escenarios |
|------|-------------|----------|------------|------------|
| [ ] 1.1 | `SandboxConfig` + `DEFAULT_FUEL_BUDGET` + `default_fuel_budget()` — agregar `consume_fuel: bool` (default `true`), `fuel_budget: Option<u64>` (default `None`); constante del módulo y helper | `src/sandbox/mod.rs:25-41` + nuevo constante después de la struct | `cargo test` (existentes) + clippy/fmt | — |
| [ ] 1.2 | `new_with_config` / `new_with_limits`: `config.consume_fuel(true)` en engine + `store.set_fuel(effective_budget)` obligatorio; reemplazar comentario AD-002 (:369) por setup REQ-001 | `src/sandbox/mod.rs:365-408` | `cargo test` (existentes + nuevos unit) + clippy/fmt | E-801, E-801-inverso |
| [ ] 1.3 | Accessor `Sandbox::fuel_consumed()` — `resolved_budget - get_fuel()`; guardar `budget_resolved` en struct `Sandbox` | `src/sandbox/mod.rs` (nuevo método + campo en struct) | `cargo test` (unit nuevo) + clippy/fmt | E-801, E-801-inverso |
| [ ] 1.4 | Tests unit: E-801 (sin `set_fuel` → trap instantáneo), E-801-inverso (`set_fuel` sin flag engine → error), hostio básico con fuel | `src/sandbox/mod.rs` (tests module) | `cargo test sandbox::tests` + clippy/fmt | E-801, E-801-inverso |

**Gate WU1**: `cargo test` (122 existing + nuevos unit) + `cargo clippy -- -D warnings` + `cargo fmt -- --check` → verde.

---

## WU2 — Config + Plumbing (depende de WU1)

**Objetivo**: `ExecutionConfig.fuel_budget: Option<u64>` + plomería server → handler → sandbox.

| Task | Descripción | Archivos | Validación | Escenarios |
|------|-------------|----------|------------|------------|
| [ ] 2.1 | `ExecutionConfig.fuel_budget: Option<u64>` con `#[serde(default)]` | `src/config/runtime.rs:34-38` | `cargo test config` + clippy/fmt | E-804, E-805 |
| [ ] 2.2 | `start_server_with_emitter` plumb `config.execution.fuel_budget` → `AegisRuntimeService.fuel_budget` | `src/grpc/server.rs:91-104` (líneas 96-99) | `cargo test` + clippy/fmt | E-804, E-805 |
| [ ] 2.3 | `AegisRuntimeService` + campo `fuel_budget: Option<u64>` | `src/grpc/handlers/mod.rs:22-25` | `cargo test` + clippy/fmt | E-804, E-805 |
| [ ] 2.4 | Tests unit: E-804 (absent → default), E-805 (explicit honored), config parse sin clave | `src/config/runtime.rs` tests, `src/grpc/handlers/mod.rs` tests | `cargo test config` + clippy/fmt | E-804, E-805 |

**Gate WU2**: `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt -- --check` → verde.

---

## WU3 — Receipts + Emit (depende de WU1)

**Objetivo**: `ExecutionReceipt.fuel_consumed` skip-if-zero al final, `emit` extendido, 25 call sites actualizados.

| Task | Descripción | Archivos | Validación | Escenarios |
|------|-------------|----------|------------|------------|
| [ ] 3.1 | `ExecutionReceipt.fuel_consumed` último campo con `#[serde(default, skip_serializing_if = "is_zero")]`; helper `is_zero` | `src/receipts/mod.rs:13-25` | `cargo test receipts` + clippy/fmt | E-802, S-803 |
| [ ] 3.2 | `ExecutionReceipt::new` + `canonical_bytes_for_signing` + `verify_signature` actualizados con `fuel_consumed` | `src/receipts/mod.rs:32-63, 69-81, 89-103` | `cargo test receipts` + clippy/fmt | E-802, S-803 |
| [ ] 3.3 | `ReceiptEmitter::emit` signature extendida con `fuel_consumed: u64`; doc: capability receipts pasan `0`, solo execute pasa valor real | `src/receipts/mod.rs:232-268` | `cargo test receipts` + clippy/fmt | E-802, S-803 |
| [ ] 3.4 | Update 25 call sites `.emit(` — 23 capability pasan `0`, 2 execute pasan `sandbox.fuel_consumed()` | `src/sandbox/mod.rs` (fs read/write, `emit_network_receipt`), `src/grpc/handlers/mod.rs` (2 sites), `src/receipts/mod.rs` (tests) | `cargo test` + clippy/fmt | E-802, S-803 |
| [ ] 3.5 | Tests unit: E-802 golden bytes fuel=0 (byte-identical pre-cambio), S-803 capability receipt sin fuel, campo presente en >0, field order last | `src/receipts/mod.rs` tests | `cargo test receipts::tests` + clippy/fmt | E-802, S-803 |

**Gate WU3**: `cargo test` (receipts + verification) + `cargo clippy -- -D warnings` + `cargo fmt -- --check` → verde.

---

## WU4 — Handler Execute + D4 Fuel Arm (depende de WU1+WU2+WU3)

**Objetivo**: Execute handler crea sandbox con `fuel_budget`, captura `fuel_consumed` post-call (success + trap), emite con fuel, D4 fuel arm después de network guard y antes de fs cascade.

| Task | Descripción | Archivos | Validación | Escenarios |
|------|-------------|----------|------------|------------|
| [ ] 4.1 | `execute`: sandbox creation con `fuel_budget` de service (`SandboxConfig { fuel_budget: self.fuel_budget, ..default() }`) | `src/grpc/handlers/mod.rs:73-84` | `cargo test` + clippy/fmt | S-801, S-802 |
| [ ] 4.2 | Captura `fuel_consumed = sandbox.fuel_consumed()` post-call (success + trap) — `get_fuel()` funciona post-trap | `src/grpc/handlers/mod.rs:118-120, 193` | `cargo test` + clippy/fmt | S-801, S-802 |
| [ ] 4.3 | Emit success + trap con `fuel_consumed` | `src/grpc/handlers/mod.rs:159-167, 193` | `cargo test` + clippy/fmt | S-801, S-802 |
| [ ] 4.4 | D4 cascade fuel arm: chain-walk `contains("all fuel consumed")` → `"fuel budget exceeded"` después del network guard, antes del fs cascade | `src/grpc/handlers/mod.rs:341-388` | `cargo test` + clippy/fmt | S-802, E-803 |
| [ ] 4.5 | Tests E2E: S-801 (success con fuel>0), S-802 (agotamiento budget pequeño → trap + receipt + "fuel budget exceeded"), E-803 (aislamiento D4 arm + ordering vs network guard), E-804/805 (config) | `tests/fuel.rs` (nuevo) | `cargo test fuel` + clippy/fmt | S-801, S-802, E-803, E-804, E-805 |

**Gate WU4**: `cargo test` (incluyendo S-801/S-802/E-803/E-804/E-805) + `cargo clippy -- -D warnings` + `cargo fmt -- --check` → verde.

---

## WU5 — Tests Integrales + Regresiones + AD-015 (cierre)

**Objetivo**: Suite completa verde, hostile-loop sigue atrapando, AD-015 en DECISIONS.md, golden chain test.

| Task | Descripción | Archivos | Validación | Escenarios |
|------|-------------|----------|------------|------------|
| [ ] 5.1 | Full suite regression: 122 tests + nuevos fuel tests green; `cargo clippy -- -D warnings` + `fmt -- --check` | — | `cargo test` + clippy/fmt | All |
| [ ] 5.2 | `hostile-loop` (tests/sandbox.rs:486-541) sigue atrapando (fuel o epoch, `is_err`) | `tests/sandbox.rs` | `cargo test hostile_loop` | Regression |
| [ ] 5.3 | AD-015 transcripción en DECISIONS.md (mismo work unit que el código — precedente Phase 7 D6 / AD-014) | `DECISIONS.md` | `cargo test` + review manual | AD-015 |
| [ ] 5.4 | Golden chain test: cadenas viejas verifican con nuevo código (fuel=0 byte-identical) | `tests/receipts.rs` o `tests/fuel.rs` | `cargo test golden` | E-802, backward compat |

**Gate WU5**: `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt -- --check` → **verde completo**.

---

## Dependency Graph

```
WU1 (Sandbox Foundation)
    ├── WU2 (Config + Plumbing)
    ├── WU3 (Receipts + Emit)
    └── WU4 (Handler Execute + D4 Arm) ← necesita WU1, WU2, WU3
        
WU5 (Tests Integrales + AD-015) ← necesita WU1, WU2, WU3, WU4
```

---

## Open Questions

Ninguna — todos los ítems están cubiertos por specs/design. El valor del budget por defecto (10_000_000) viene calibrado del design spike §6.

---

## AD-015 Draft (para WU5.3 — transcripción en apply)

Ver design.md §7. Se transcribe **en el mismo work unit** que el código (precedente Phase 7 AD-014/D6).