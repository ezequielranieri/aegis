# gRPC Runtime Server Specification

## Purpose

gRPC server exposing aegis WASM runtime capabilities (Execute, VerifyChain, GetReceiptChain) to external callers such as agent-gateway. The Ed25519 private key remains bound to the aegis-runtime process and is never exported.

## Requirements


| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-700 | System SHALL define a gRPC service `AegisRuntime` in `proto/aegis/v1/aegis.proto` with package `aegis.v1` | MUST |
| REQ-701 | System SHALL implement `Execute(ExecuteRequest) -> ExecuteResponse` RPC that runs a named capability and returns result + signed receipt | MUST |
| REQ-702 | System SHALL implement `VerifyChain(VerifyChainRequest) -> VerifyChainResponse` RPC that validates BLAKE3 hash chain + Ed25519 signatures | MUST |
| REQ-703 | System SHALL implement `GetReceiptChain(GetReceiptChainRequest) -> GetReceiptChainResponse` RPC that returns the current receipt chain | MUST |
| REQ-704 | System SHALL accept TOML configuration for host, port, TLS (cert_path, key_path, ca_cert_path for mTLS), and key_path via `RuntimeConfig` | MUST |
| REQ-705 | Ed25519 private key SHALL remain in `ReceiptEmitter` within the aegis-runtime process and MUST NOT be exported, serialized, or sent over the network | MUST |
| REQ-706 | System SHALL load Ed25519 key pair from `key_path` at startup and fail-closed if key is missing or invalid | MUST |
| REQ-707 | `ExecuteRequest` SHALL contain: `capability_name` (string), `config` (bytes, TOML-encoded PolicyConfig) | MUST |
| REQ-708 | `ExecuteResponse` SHALL contain: `result` (bytes), `receipt` (bytes, JSON-encoded ExecutionReceipt), `success` (bool) | MUST |
| REQ-709 | `VerifyChainRequest` SHALL contain: `receipts` (repeated bytes, JSON-encoded), `public_key` (bytes) | MUST |
| REQ-710 | `VerifyChainResponse` SHALL contain: `valid` (bool), `error_message` (string, populated on failure) | MUST |
| REQ-711 | `GetReceiptChainRequest` SHALL be empty | MUST |
| REQ-712 | `GetReceiptChainResponse` SHALL contain: `receipts` (repeated bytes, JSON-encoded ExecutionReceipt) | MUST |
| REQ-713 | System SHALL serve over h2 (HTTP/2) with **mutual TLS (mTLS) required** (TLS 1.2+). Server presents its certificate; client MUST present a valid certificate signed by the configured CA. Requests without valid client certificate are rejected. | MUST |
| REQ-714 | Execute RPC SHALL create a sandbox per request from the provided config and run the capability | MUST |
| REQ-715 | Execute RPC SHALL trap and return `success=false` on any violation (path escape, traversal, size exceed, fuel budget exceeded) — fail-closed. | MUST |
| REQ-716 | System SHALL validate client certificates against the configured CA certificate (`ca_cert_path` in config). Client certificate MUST include the expected identity (e.g., `agent-gateway` as CN or SAN). Requests with invalid, expired, or mismatched client certificates are rejected with `INVALID_CERT` gRPC status. | MUST |
| REQ-717 | System SHALL accept an optional `execution.fuel_budget` (`Option<u64>`) in `RuntimeConfig`. When absent, the system SHALL apply a generous default budget calibrated by a design spike over the existing filesystem and network E2E suites — the default is design-calibrated and SHALL NOT be specified as a normative constant in this spec. When present, the system SHALL honor the configured value as the per-execution budget. Configurations without the key SHALL parse unchanged. | MUST |
| REQ-730 | System SHALL implement `ExecutePrepare(ExecutePrepareRequest) -> ExecutePrepareResponse` RPC that:<br>• Accepts `capability_name` (string), `config` (bytes, TOML-encoded PolicyConfig), `wasm_module` (bytes)<br>• Validates the capability is granted in config<br>• Creates a fresh sandbox with the configured fuel budget<br>• **Signs a `prepare` receipt** with: `phase="prepare"`, `result="pending"`, `path=""`, `size=0`, `fuel_consumed=0`, `pending_hash = BLAKE3(prev_hash || prepare_canonical_bytes)`<br>• Stores the pending receipt + sandbox handle in `ReceiptEmitter.pending` map keyed by `pending_hash`<br>• Returns the signed prepare receipt + `prepare_hash` (hex-encoded `pending_hash`) | MUST |
| REQ-731 | `ExecutePrepareRequest` SHALL contain:<br>• `capability_name` (string)<br>• `config` (bytes, TOML-encoded PolicyConfig)<br>• `wasm_module` (bytes) | MUST |
| REQ-732 | `ExecutePrepareResponse` SHALL contain:<br>• `success` (bool)<br>• `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="prepare"`)<br>• `prepare_hash` (string, hex-encoded `pending_hash` for Commit/Abort)<br>• `error_message` (string, populated on failure) | MUST |
| REQ-733 | System SHALL implement `ExecuteCommit(ExecuteCommitRequest) -> ExecuteCommitResponse` RPC that:<br>• Accepts `prepare_hash` (string, hex) and `result` (bytes, guest execution result)<br>• Looks up the pending entry in `ReceiptEmitter.pending` by `prepare_hash` — returns `NOT_FOUND` if missing (idempotency: duplicate Commit returns the already-emitted commit receipt)<br>• Executes the WASM module in the **prepared sandbox** (same instance from Prepare)<br>• Captures actual result, path, size, `fuel_consumed` from sandbox<br>• **Signs a `commit` receipt** with: `phase="commit"`, `pending_hash` (copied from prepare), actual result/path/size/fuel, normal chain hash continuation<br>• Removes the pending entry from the map<br>• Returns the signed commit receipt | MUST |
| REQ-734 | `ExecuteCommitRequest` SHALL contain:<br>• `prepare_hash` (string, hex-encoded)<br>• `result` (bytes, guest execution result from `execute` export) | MUST |
| REQ-735 | `ExecuteCommitResponse` SHALL contain:<br>• `success` (bool)<br>• `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="commit"`)<br>• `error_message` (string, populated on failure) | MUST |
| REQ-736 | System SHALL implement `ExecuteAbort(ExecuteAbortRequest) -> ExecuteAbortResponse` RPC that:<br>• Accepts `prepare_hash` (string, hex)<br>• Looks up the pending entry — returns `NOT_FOUND` if missing (idempotency: duplicate Abort returns the already-emitted abort receipt)<br>• **Signs an `abort` receipt** with: `phase="abort"`, `pending_hash` (copied from prepare), `result="aborted"`, `path=""`, `size=0`, `fuel_consumed=0`, chain continues from abort's hash<br>• Discards the sandbox, removes the pending entry<br>• Returns the signed abort receipt | MUST |
| REQ-737 | `ExecuteAbortRequest` SHALL contain:<br>• `prepare_hash` (string, hex-encoded) | MUST |
| REQ-738 | `ExecuteAbortResponse` SHALL contain:<br>• `success` (bool)<br>• `receipt` (bytes, JSON-encoded `ExecutionReceipt` with `phase="abort"`)<br>• `error_message` (string, populated on failure) | MUST |
| REQ-739 | The two-phase protocol SHALL enforce the following state machine per `pending_hash`:<br>• `pending` (after Prepare) → `committed` (after Commit) OR `aborted` (after Abort)<br>• **No Commit after Abort**: second Commit/Abort on same `pending_hash` returns the already-emitted receipt (idempotent)<br>• **No Prepare reuse**: each Prepare creates a unique `pending_hash` (BLAKE3 chain hash); concurrent Prepares are independent | MUST |
| REQ-740 | The legacy `Execute` RPC SHALL be retained for backward compatibility and SHALL internally:<br>• Call `ExecutePrepare` logic (create sandbox, sign prepare receipt, store pending)<br>• Call `ExecuteCommit` logic (execute WASM in same sandbox, sign commit receipt)<br>• If Commit fails (signing failure), call `ExecuteAbort` logic (emit abort receipt with `error="commit_signing_failed"`)<br>• Return the final commit/abort receipt to the caller | MUST |

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| E-803 | D4 arm isolation | Execution traps from fuel exhaustion; module also holds fs/network capabilities | D4 cascade classifies the trap | Classified `"fuel budget exceeded"` — never the fs `size limit exceeded` or network branches; network traps still classified by the network guard (first branch) |
| E-804 | Budget absent | `runtime.toml` without `execution.fuel_budget` | Config parsed; Execute runs | Parses unchanged; design-calibrated default applied as effective budget |
| E-805 | Budget explicit | `execution.fuel_budget` set to a value `n` | Execute runs | `n` honored as the effective budget |
| E-913 | D4 cascade in Commit |  Commit executes WASM that traps |  Trap from network / fuel / fs / signing |  Classified correctly per cascade order (network first, then fuel, then fs, then signing) |
| E-914 | Budget set in Prepare |  fuel_budget configured |  ExecutePrepare called |  Sandbox created with budget; Commit reuses same sandbox + budget |
| S-700 | Execute happy path | Valid config with filesystem.read capability, Ed25519 key loaded | Client sends ExecuteRequest for filesystem.read | ExecuteResponse with result bytes, valid signed receipt, success=true |
| S-701 | Execute violation trap | Config with filesystem.read, capability granted | Client sends ExecuteRequest for path outside allowed_root | ExecuteResponse with success=false, error message |
| S-702 | Execute signing failure | Corrupt Ed25519 key file | Client sends ExecuteRequest | ExecuteResponse with success=false |
| S-703 | VerifyChain valid chain | Two valid receipts emitted sequentially, correct public key | Client sends VerifyChainRequest | VerifyChainResponse with valid=true |
| S-704 | VerifyChain tampered receipt | Chain with one tampered receipt | Client sends VerifyChainRequest | VerifyChainResponse with valid=false, error_message indicates tampering |
| S-705 | GetReceiptChain after executions | Server has emitted 3 receipts | Client sends GetReceiptChainRequest | VerifyChainResponse with 3 JSON-encoded receipts |
| S-706 | Startup with missing key | No key file at configured key_path | Server starts | Server fails to start, error logged |
| S-707 | Startup with invalid key perms | Key file with 0644 permissions | Server starts | Server fails to start, error logged |
| S-708 | TLS enabled | Valid TLS cert and key configured | Client connects with TLS | Connection established, RPCs succeed |
| S-709 | mTLS client cert validation | Valid TLS cert/key + `ca_cert_path`, client presents valid cert signed by CA | Client sends ExecuteRequest | Request succeeds, response with receipt |
| S-710 | mTLS rejects invalid client cert | Valid TLS cert/key + `ca_cert_path`, client presents self-signed or wrong CA cert | Client sends ExecuteRequest | Request rejected with INVALID_CERT, no execution |
| S-711 | mTLS rejects missing client cert | Valid TLS cert/key + `ca_cert_path`, client connects without client cert | Client sends ExecuteRequest | Request rejected with INVALID_CERT, no execution |
| S-712 | Server shutdown | Server is running, active RPCs in progress | SIGTERM received | Server drains active RPCs, then exits |
| S-801 | Execute success reports fuel | Fresh sandbox with budget; module consumes fuel | Execute RPC succeeds | `success=true`; receipt carries `fuel_consumed` > 0 equal to `effective_budget - get_fuel()` |
| S-802 | Execute fuel exhaustion | Module consumes its full budget | Execute RPC runs | Deterministic trap; receipt carries `fuel_consumed`; `success=false` with `error_message` `"fuel budget exceeded"` |
| S-900 | ExecutePrepare happy path |  Valid config + capability + WASM module |  Client calls ExecutePrepare |  `success=true`, signed `phase="prepare"` receipt with `pending_hash`, `prepare_hash` returned |
| S-901 | ExecutePrepare invalid capability |  Capability not granted in config |  Client calls ExecutePrepare |  `success=false`, error_message indicates capability not granted |
| S-902 | ExecutePrepare missing WASM |  Valid config, empty wasm_module |  Client calls ExecutePrepare |  `success=false`, error_message "wasm_module is required" |
| S-903 | ExecuteCommit happy path |  Prepare succeeded, pending entry exists |  Client calls ExecuteCommit with prepare_hash + result |  `success=true`, signed `phase="commit"` receipt with actual result/fuel, `pending_hash` links to prepare |
| S-904 | ExecuteCommit idempotent |  Prepare succeeded, Commit already called |  Client calls ExecuteCommit again with same prepare_hash |  Returns the already-emitted commit receipt (no re-execution) |
| S-905 | ExecuteCommit not found |  No pending entry for prepare_hash |  Client calls ExecuteCommit |  `success=false`, error_message "prepare not found" |
| S-906 | ExecuteAbort happy path |  Prepare succeeded, pending entry exists |  Client calls ExecuteAbort with prepare_hash |  `success=true`, signed `phase="abort"` receipt, chain continues from abort hash |
| S-907 | ExecuteAbort idempotent |  Prepare succeeded, Abort already called |  Client calls ExecuteAbort again with same prepare_hash |  Returns the already-emitted abort receipt |
| S-908 | ExecuteAbort not found |  No pending entry for prepare_hash |  Client calls ExecuteAbort |  `success=false`, error_message "prepare not found" |
| S-909 | Prepare→Commit chain |  Prepare then Commit |  Client calls Prepare then Commit |  Chain: prepare(prev_hash=genesis) → commit(pending_hash=prepare_hash) |
| S-910 | Prepare→Abort chain |  Prepare then Abort |  Client calls Prepare then Abort |  Chain: prepare(prev_hash=genesis) → abort(pending_hash=prepare_hash) |
| S-911 | Legacy Execute deprecated |  Server running |  Client calls legacy Execute RPC |  Works atomically (Prepare+Commit internally); receipt has `phase="commit"` |
| S-912 | Concurrent Prepares |  Multiple concurrent ExecutePrepare calls |  Each with different capability/WASM |  Each gets unique prepare_hash; no cross-talk; independent pending entries |

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-700 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-701 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-702 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-703 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-704 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-705 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-706 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-707 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-708 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-709 | S-7000, S-7001, S-7002, S-7003, S-7004, S-7005, S-7006, S-7007, S-7008, S-7009 |
| REQ-710 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-711 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-712 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-713 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-714 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-715 | E-913, S-903 |
| REQ-716 | S-7100, S-7101, S-7102, S-7103, S-7104, S-7105, S-7106, S-7107, S-7108, S-7109 |
| REQ-717 | E-804, E-805, S-801 |
| REQ-730 | S-900, S-901, S-902 |
| REQ-731 | S-900, S-901, S-902 |
| REQ-732 | S-900, S-901, S-902 |
| REQ-733 | S-903, S-904, S-905 |
| REQ-734 | S-903, S-904, S-905 |
| REQ-735 | S-903, S-904, S-905 |
| REQ-736 | S-906, S-907, S-908 |
| REQ-737 | S-906, S-907, S-908 |
| REQ-738 | S-906, S-907, S-908 |
| REQ-739 | S-904, S-907, S-912 |
| REQ-740 | S-911 |
