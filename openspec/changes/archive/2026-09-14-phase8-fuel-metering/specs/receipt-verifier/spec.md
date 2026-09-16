# Delta: receipt-verifier — Phase 8 Fuel Metering

## MODIFIED Requirements

### REQ-400: ExecutionReceipt Structure

System SHALL define `ExecutionReceipt` with fields: `capability_name`, `action`, `result`, `path`, `size`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`, `fuel_consumed: u64`.

(Previously: enumeration listed 6 fields — `path` and `size` were missing from the list and `fuel_consumed` did not exist.)

## Scenarios

No new scenarios. This is a documentation delta: REQ-400's field enumeration is corrected to match the real struct (8 fields + `fuel_consumed`). Verifier logic (REQ-451, REQ-452, REQ-453) does not reference the enumeration and is unchanged; the existing S-410..S-413 and S-454 scenarios remain the complete verification coverage.

## ADDED Requirements

No requirements added.

## REMOVED Requirements

No requirements removed.

## RENAMED Requirements

No requirements renamed.

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-400 (modified) | — (doc-delta only; no new scenarios) |