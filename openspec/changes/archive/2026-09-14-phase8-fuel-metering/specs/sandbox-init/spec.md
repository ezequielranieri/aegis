# Delta: sandbox-init — Phase 8 Fuel Metering

## MODIFIED Requirements

### REQ-001: Engine Configuration

The system SHALL create a Wasmtime `Engine` with `epoch_interruption(true)` and `consume_fuel(true)` enabled. Every sandbox created for execution SHALL set a per-execution fuel budget via `Store::set_fuel(effective_budget)` before running any module; `set_fuel` is mandatory and the store default of 0 SHALL NOT be relied upon. Fuel exhaustion SHALL produce a deterministic Wasmtime trap. Epoch interruption SHALL remain the CPU security boundary (AD-002); fuel metering is CPU consumption accounting and SHALL NOT replace or weaken epoch interruption.

(Previously: engine created with `epoch_interruption(true)` only; fuel metering SHALL NOT be enabled.)

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| — | Epoch enabled | Engine is created with config | Engine is constructed | `epoch_interruption` is true on the config |
| — | Fuel accounting active | Engine is created with `consume_fuel(true)` | Any module runs | Instructions consume fuel; remaining fuel is readable via `get_fuel` |
| — | Budget always set | Fresh sandbox created for execution | `set_fuel(effective_budget)` is applied | Execution does not trap instantly (store default 0 is never relied upon) |
| — | Exhaustion traps deterministically | Module consumes its entire budget | Execution continues | Deterministic Wasmtime trap (`all fuel consumed` literal) — never a graceful error |
| E-801 | `set_fuel` omitted | Engine with `consume_fuel(true)`, no budget set | Any module runs | Traps instantly (store default 0); unreachable in production because `set_fuel` is mandatory at sandbox creation — exercised by a unit test |

## ADDED Requirements

No requirements added.

## REMOVED Requirements

No requirements removed.

## RENAMED Requirements

No requirements renamed.

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-001 (modified) | E-801 |