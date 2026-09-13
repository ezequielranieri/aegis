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
| REQ-716 | System SHALL validate client certificates against the configured CA certificate (`ca_cert_path` in config). Client certificate MUST include the expected identity (e.g., `agent-gateway` as CN or SAN). Requests with invalid, expired, or mismatched client certificates are rejected with `INVALID_CERT` gRPC status. | MUST |
| REQ-714 | Execute RPC SHALL create a sandbox per request from the provided config and run the capability | MUST |
| REQ-715 | Execute RPC SHALL trap and return `success=false` on any violation (path escape, traversal, size exceed) — fail-closed | MUST |

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
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

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-701, REQ-714, REQ-715 | S-700, S-701, S-702 |
| REQ-702 | S-703, S-704 |
| REQ-703 | S-705 |
| REQ-704, REQ-706 | S-706, S-707 |
| REQ-705, REQ-706 | S-706, S-707 |
| REQ-713, REQ-716 | S-708, S-709, S-710, S-711 |
| REQ-700 | S-712 |
