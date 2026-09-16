# Delta: signed-receipts — Phase 5 Agent Gateway Integration

## MODIFIED Requirements

### REQ-430: ReceiptEmitter in SandboxState

System SHALL store `ReceiptEmitter` in `SandboxState`. When `aegis-runtime` serves via gRPC, `ReceiptEmitter` SHALL be owned by the runtime process and shared across concurrent sandbox instances via `Arc<Mutex<ReceiptEmitter>>`.

(Previously: `ReceiptEmitter` stored in `SandboxState` per-sandbox instance only)

### REQ-431: ReceiptEmitter Holds Key Pair + Chain

`ReceiptEmitter` SHALL hold `Ed25519KeyPair` + `ReceiptChain`. The Ed25519 private key SHALL remain in-process and MUST NOT be serialized, exported, or transmitted over gRPC. Only the corresponding public key MAY be returned to callers (e.g., via `GetReceiptChain`).

(Previously: No explicit constraint on key export)

### REQ-433: Fail-Closed Signing Failure

Signing failure SHALL propagate as error, never produce unsigned receipt. If `emit()` returns `Err`, the calling host function MUST immediately return `Trap`. For gRPC Execute RPC, signing failure SHALL result in `ExecuteResponse.success=false`.

(Previously: Only applied to direct library host function calls)

## ADDED Requirements

### REQ-723: fuel_consumed on Execute Receipts

Execute receipts SHALL carry `fuel_consumed: u64` reporting the fuel consumed by the execution, measured as `effective_budget - Store::get_fuel()` after the call returns. The field SHALL be serialized with `skip_serializing_if = "is_zero"` and SHALL be declared last, after `timestamp_ns`, so receipts with `fuel_consumed == 0` serialize byte-identically to the pre-change schema and existing chains keep verifying with zero verifier changes. In v1 the field SHALL appear on Execute receipts only — success and trap paths; capability receipts (filesystem/network host functions) SHALL NOT gain fuel semantics.

### REQ-720: Key Bound to Runtime Process

The Ed25519 private key SHALL be loaded once at `aegis-runtime` startup and remain in `ReceiptEmitter` for the lifetime of the process. The private key MUST NOT be accessible via any gRPC RPC — neither as a request field nor as a response field.

### REQ-721: Public Key Export

`GetReceiptChain` response SHALL NOT include the public key. Callers obtain the public key via a separate mechanism (e.g., config or key file). This keeps the gRPC contract minimal and avoids accidental key exposure.

### REQ-722: CLI Verifier Unchanged

The optional CLI verifier (`aegis-verify`) SHALL continue to work unchanged. It reads receipts from a JSON file and verifies against a public key file — both provided by the user externally. The CLI verifier does not depend on the runtime process.

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-803 | Capability receipt without fuel | Filesystem/network host function emits a receipt | Receipt serialized | `fuel_consumed` absent (skip-if-zero); schema unchanged |
| E-802 | fuel=0 byte-identical golden compat | Execute receipt with `fuel_consumed == 0` | Canonical bytes computed | Byte-identical to the pre-change schema (golden test); old chains verify unchanged |
| S-720 | Key never over gRPC | Server running with loaded key | Any RPC executed | Private key bytes not present in any request or response protobuf message |
| S-721 | Execute signing failure via gRPC | Corrupt key loaded at startup | ExecuteRequest received | ExecuteResponse with success=false, error_message indicates signing failure |
| S-722 | CLI verifier after gRPC execution | Receipts emitted via gRPC Execute RPC, public key available | CLI verifier runs against exported receipt chain JSON | Verification succeeds with correct public key |
| S-723 | Concurrent Execute RPCs | Multiple concurrent ExecuteRequests | Requests processed in parallel | Each RPC emits valid receipt; chain integrity maintained across concurrent emissions |

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-430 (modified) | S-723 |
| REQ-431 (modified) | S-720, S-721 |
| REQ-433 (modified) | S-721 |
| REQ-720 | S-720 |
| REQ-721 | S-720 |
| REQ-722 | S-722 |
| REQ-723 | S-803, E-802 |
