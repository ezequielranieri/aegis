# Delta: grpc-runtime-server — Phase 8 Fuel Metering

## MODIFIED Requirements

### REQ-715: Fail-Closed Violation Trapping

Execute RPC SHALL trap and return `success=false` on any violation (path escape, traversal, size exceed, fuel budget exceeded) — fail-closed. Fuel exhaustion SHALL be classified by a dedicated D4 cascade arm matching the deterministic `all fuel consumed` trap literal and SHALL return `success=false` with `error_message` `"fuel budget exceeded"`. The fuel arm SHALL be evaluated after the network guard (which remains the first branch of the cascade) and before the filesystem cascade branches.

(Previously: trap and return `success=false` on path escape, traversal, and size exceed — fail-closed; fuel exhaustion had no dedicated cascade arm.)

## ADDED Requirements

### REQ-717: Fuel Budget Knob

The system SHALL accept an optional `execution.fuel_budget` (`Option<u64>`) in `RuntimeConfig`. When absent, the system SHALL apply a generous default budget calibrated by a design spike over the existing filesystem and network E2E suites — the default is design-calibrated and SHALL NOT be specified as a normative constant in this spec. When present, the system SHALL honor the configured value as the per-execution budget. Configurations without the key SHALL parse unchanged.

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-801 | Execute success reports fuel | Fresh sandbox with budget; module consumes fuel | Execute RPC succeeds | `success=true`; receipt carries `fuel_consumed` > 0 equal to `effective_budget - get_fuel()` |
| S-802 | Execute fuel exhaustion | Module consumes its full budget | Execute RPC runs | Deterministic trap; receipt carries `fuel_consumed`; `success=false` with `error_message` `"fuel budget exceeded"` |
| E-803 | D4 arm isolation | Execution traps from fuel exhaustion; module also holds fs/network capabilities | D4 cascade classifies the trap | Classified `"fuel budget exceeded"` — never the fs `size limit exceeded` or network branches; network traps still classified by the network guard (first branch) |
| E-804 | Budget absent | `runtime.toml` without `execution.fuel_budget` | Config parsed; Execute runs | Parses unchanged; design-calibrated default applied as effective budget |
| E-805 | Budget explicit | `execution.fuel_budget` set to a value `n` | Execute runs | `n` honored as the effective budget |

## REMOVED Requirements

No requirements removed.

## RENAMED Requirements

No requirements renamed.

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-715 (modified) | S-802, E-803 |
| REQ-717 | S-801, E-804, E-805 |