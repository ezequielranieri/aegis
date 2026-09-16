# Delta: signed-receipts — Phase 8 Fuel Metering

## ADDED Requirements

### REQ-723: fuel_consumed on Execute Receipts

Execute receipts SHALL carry `fuel_consumed: u64` reporting the fuel consumed by the execution, measured as `effective_budget - Store::get_fuel()` after the call returns. The field SHALL be serialized with `skip_serializing_if = "is_zero"` and SHALL be declared last, after `timestamp_ns`, so receipts with `fuel_consumed == 0` serialize byte-identically to the pre-change schema and existing chains keep verifying with zero verifier changes. In v1 the field SHALL appear on Execute receipts only — success and trap paths; capability receipts (filesystem/network host functions) SHALL NOT gain fuel semantics.

## MODIFIED Requirements

No requirements modified.

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-803 | Capability receipt without fuel | Filesystem/network host function emits a receipt | Receipt serialized | `fuel_consumed` absent (skip-if-zero); schema unchanged |
| E-802 | fuel=0 byte-identical golden compat | Execute receipt with `fuel_consumed == 0` | Canonical bytes computed | Byte-identical to the pre-change schema (golden test); old chains verify unchanged |

## REMOVED Requirements

No requirements removed.

## RENAMED Requirements

No requirements renamed.

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-723 | S-803, E-802 |