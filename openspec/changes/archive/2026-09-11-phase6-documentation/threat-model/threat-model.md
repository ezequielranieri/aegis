# Threat Model

Phase-local threat model snapshot (Phase 6, 2026-09-11). The aegis threat model covers three security boundaries: the WASM sandbox, the gRPC boundary, and the receipt chain.

## 1. WASM Sandbox Threat Model

| Threat | Mitigation | Evidence |
|--------|-----------|----------|
| Memory exhaustion | `StoreLimitsBuilder::memory_size(1 << 20)` + `trap_on_grow_failure(true)` | AD-001, REQ-003 |
| CPU exhaustion | Epoch interruption (100ms timer thread) | AD-002, REQ-001 |
| Table/instance fork-bomb | `table_elements(1024)`, `instances(4)`, `memories(2)` | REQ-004 |
| Path traversal | `..` rejection in `aegis_fs_read/write`, canonicalize + `starts_with` root check, symlink target `..` rejection | AD-006, REQ-503 |
| NUL byte injection | `trim_end_matches('\0')` on guest memory paths | AD-006 |
| Size exceed | `cap.max_bytes` check after `stat()` for read, before write | REQ-504 |
| Symlink escape | `symlink_metadata` check, `read_link` for non-existent targets, `..` component rejection | AD-006 |
| Receipt gap after rename | Audit log "FAIL-CLOSED VIOLATION" + Trap return | AD-005, REQ-510 |
| Wrong thread handle in epoch interrupter | mpsc channel relay of `Thread` handle | AD-003 |
| Test epoch contamination | `Sandbox::new_with_config(config, false)` for test isolation | AD-004 |
| WASI import injection | WASI imports produce linking error at instantiation (S-4) | `instantiate_with_capabilities` |

## 2. gRPC Boundary Threat Model

| Threat | Mitigation | Evidence |
|--------|-----------|----------|
| Unauthorized client connection | mTLS with `client_auth_mandatory = true` + CN/SAN validation | ADR-012, REQ-713 |
| Client cert identity spoofing | `AegisClientCertVerifier` checks CN (`CN=agent-gateway`) AND SAN (DNS/URI) | REQ-716 |
| Concurrent overload | `Arc<Semaphore>` with `max_concurrent` limit, `RESOURCE_EXHAUSTED` status | REQ-714 |
| Capability escalation | `ExecuteRequest` validated against `PolicyConfig` capabilities — only granted capabilities can be executed | `src/grpc/handlers/mod.rs` |
| Receipt tampering | `VerifyChain` validates Ed25519 signature + BLAKE3 hash chain + timestamp ±5s | `src/receipts/mod.rs` |
| Private key exposure | Ed25519 key pair loaded at startup, never exported (only `public_key()` method), never serialized to network | REQ-720, REQ-811 |
| Unauthorized GetReceiptChain | `GetReceiptChainRequest` has no auth field — relies on mTLS channel security | REQ-711 |
| Result fabrication | Guest memory read at `result_ptr/len` after `execute` export call (AD-009) | AD-009 |
| gRPC error mapping | Violation traps → `FAILED_PRECONDITION`, signing failures → `INTERNAL`, invalid config → `INVALID_ARGUMENT` | `src/grpc/handlers/mod.rs` |

## 3. Receipt Chain Threat Model

| Threat | Mitigation | Evidence |
|--------|-----------|----------|
| Receipt forgery | Ed25519 signature verification in `ReceiptChain::verify_chain()` | REQ-413 |
| Chain manipulation | BLAKE3 hash chain: `prev_hash[i] = blake3(prev_hash[i-1] || canonical[i])` | REQ-410, REQ-411 |
| Timestamp replay | ±5 second tolerance in `verify_chain()` | REQ-412 |
| Wrong public key | `verify_chain()` rejects receipts signed by different key | REQ-413 |
| Receipt gap (S-416) | Explicit documentation + audit log; two-phase emission deferred | AD-005 |
| Forced signing failure | `test-utils` feature flag gates `force_signing_failure()`, compile-time isolated | ADR-011, S-416 |
| Private key in process | `Ed25519KeyPair` in `ReceiptEmitter` struct, never serialized, never in gRPC messages | REQ-720 |

## 4. Known Gaps and Acceptable Risks

| Gap | Risk Level | Mitigation | Why Acceptable |
|-----|-----------|-----------|----------------|
| Epoch imprecision for tight loops | Medium | Host functions provide natural epoch check points | Phase 0 design choice (AD-002); fuel metering deferred |
| S-416 receipt gap after write+emit failure | Medium | Audit log + Trap return | Cannot undo host filesystem write; two-phase emission is deferred |
| TLS not enforced in production config | Medium | Server warns when TLS is `None` | Current deployments use mTLS; no mechanism to mandate TLS |
| `network.http` capability not implemented | Low | N/A | Deferred to future phase (ADR-013) |
| `AegisClientCertVerifier` uses manual X.509 parsing | Low | `x509-parser` library | Well-maintained library; CN/SAN checks are straightforward |
| No rate limiting beyond concurrency semaphore | Low | `max_concurrent` semaphore | Rate limiting is a different concern from concurrency limiting |