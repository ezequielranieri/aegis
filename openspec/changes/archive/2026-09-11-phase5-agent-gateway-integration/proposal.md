# Proposal: Phase 5 Agent Gateway Integration

## Intent

Expose aegis WASM runtime capabilities via gRPC so agent-gateway (Go) can delegate tool execution to aegis-runtime while preserving aegis's BLAKE3+Ed25519 audit chain. This enables agent-gateway's `WasmExecutor` to call aegis instead of wazero, keeping execution proofs in aegis where the Ed25519 private key resides.

## Scope

### In Scope
- Define gRPC protobuf contract at `proto/aegis/v1/aegis.proto`
- Implement gRPC server in new binary `aegis-runtime` (`bin/aegis-runtime.rs`)
- Expose RPCs:
  - `Execute(ExecuteRequest) -> ExecuteResponse` — runs capability with config, returns result + signed receipt
  - `VerifyChain(VerifyChainRequest) -> VerifyChainResponse` — verifies BLAKE3 hash chain + Ed25519 signatures
  - `GetReceiptChain(GetReceiptChainRequest) -> GetReceiptChainResponse` — returns current receipt chain
- Ed25519 private key never leaves aegis-runtime process (stays in `ReceiptEmitter`)
- BLAKE3 hash chain + Ed25519 signatures remain aegis audit chain; agent-gateway maintains independent SHA256 chain
- gRPC server configuration via TOML (host, port, TLS optional)
- `aegis-runtime` as standalone binary with own `Cargo.toml` entry point

### Out of Scope
- TPE (Typed Partial Evaluation) implementation in agent-gateway — separate repo/proposal
- agent-gateway TPE integration — agent-gateway's own proposal
- BLAKE3 vs SHA256 reconciliation — two independent audit chains, different layers
- wazero migration/deprecation in agent-gateway — agent-gateway internal decision
- `network.http` capability implementation in aegis — deferred to future phase
- agent-gateway gRPC client implementation — agent-gateway's own proposal

## Capabilities

### New Capabilities
- `grpc-runtime-server`: gRPC server exposing Execute, VerifyChain, GetReceiptChain RPCs
- `aegis-runtime-binary`: Standalone binary entry point for aegis runtime service

### Modified Capabilities
- `signed-receipts`: Private key now strictly bound to aegis-runtime process; CLI verifier unchanged
- `capability-execution`: Execution now also accessible via gRPC (in addition to direct library use)

## Approach

gRPC with Protocol Buffers for Rust↔Go language boundary. Separate binary `aegis-runtime` for fault isolation (aegis crash ≠ gateway crash), aligning with Phase 0 fail-closed principle. In-process not possible (Rust≠Go). Ed25519 private key stays in `ReceiptEmitter` inside aegis-runtime; only public key exported. Two independent audit chains: aegis (BLAKE3+Ed25519 = execution proof), agent-gateway (SHA256 = routing/auth/HITL).

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `proto/aegis/v1/aegis.proto` | New | gRPC service definition |
| `bin/aegis-runtime.rs` | New | Standalone binary entry point |
| `src/grpc/` | New | gRPC server implementation |
| `src/receipts/emitter.rs` | Modified | Private key bound to runtime process |
| `Cargo.toml` | Modified | New binary target + tonic/prost dependencies |
| `config/runtime.toml` | New | gRPC server configuration |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| gRPC latency (~0.1-1ms per call) | Medium | Keep payloads small; batch if needed |
| Protobuf contract versioning | Medium | Use package versioning (v1); plan v2 migration path |
| Deployment complexity (two binaries) | Medium | Document systemd/docker-compose; health checks |
| Private key management in separate process | Low | Key generated at startup; never serialized; env var for persistence path |

## Rollback Plan

1. Remove `bin/aegis-runtime.rs` and `src/grpc/` directory
2. Revert `Cargo.toml` binary target and tonic/prost dependencies
3. Delete `proto/aegis/v1/aegis.proto`
4. Delete `config/runtime.toml`
5. `ReceiptEmitter` reverts to library-internal use (no gRPC exposure)

## Dependencies

- tonic 0.10+ (gRPC server)
- prost 0.12+ (protobuf codegen)
- Existing: ring (Ed25519), blake3, wasmtime, tokio

## Success Criteria

- [ ] `cargo build --bin aegis-runtime` compiles successfully
- [ ] `aegis-runtime --config config/runtime.toml` starts gRPC server on configured host:port
- [ ] `Execute` RPC runs `filesystem.read` capability, returns result + signed receipt
- [ ] `VerifyChain` RPC validates BLAKE3 hash chain + Ed25519 signatures correctly
- [ ] `GetReceiptChain` RPC returns full receipt chain from runtime process
- [ ] Private key never appears in logs, memory dumps, or network traffic
- [ ] Binary runs independently of main aegis binary