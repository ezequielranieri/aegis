## Exploration: Phase 6 Documentation

### Current State

The aegis project has completed Phases 0–5 and merged to master. The system is a Rust/Wasmtime-based secure WASM runtime with a gRPC boundary, 90 tests passing, and a fully functional `aegis-runtime` binary with mTLS.

**Architecture summary (from codebase exploration):**

- **Sandbox layer** (`src/sandbox/mod.rs`): Wasmtime Engine with epoch interruption, `StoreLimitsBuilder` with 4 hard limits (memory, table, instances, memories), `trap_on_grow_failure(true)`, custom host functions (`aegis_fs_read`, `aegis_fs_write`), `EpochInterrupter` with mpsc channel relay (AD-003), `Sandbox::new_with_config(config, enable_epoch)` for test isolation (AD-004)
- **Capability layer** (`src/capabilities/mod.rs`): Typed `Capability` enum (`filesystem.read`, `filesystem.write`, `network.http`), `CapabilityProvider` trait, `CapabilityConfig` per-capability allowed_root + max_bytes
- **Receipt layer** (`src/receipts/mod.rs`): `ExecutionReceipt` (BLAKE3 hash-chain, Ed25519 signing), `ReceiptEmitter` with `Arc<Mutex<...>>` shared across sandboxes, `force_signing_failure()` test-only method gated by `test-utils` feature flag, `ReceiptChain::verify_chain()` with signature + hash-chain + timestamp validation
- **gRPC layer** (`src/grpc/`): Tonic-based server with `Execute`, `VerifyChain`, `GetReceiptChain` RPCs, mTLS with custom `AegisClientCertVerifier` (CN/SAN validation), `Arc<Semaphore>` concurrency limiting, `Health` service, `AegisRuntimeService` with shared `ReceiptEmitter`
- **Config layer** (`src/config/`): TOML-based `PolicyConfig` with capability definitions, `RuntimeConfig` with server/TLS/receipts/execution sections, `load_receipt_keypair()` with 0600 permission enforcement
- **Policy engine** (`src/policy/mod.rs`): Declarative `Policy` with `PolicyRule` (capability + effect + conditions), `PolicyEngine::evaluate()` with first-match-wins and `default_effect = Deny`
- **Observability** (`src/observability/mod.rs`): `tracing` + OpenTelemetry OTLP exporter, `init_tracing()` with env filter
- **Proto** (`proto/aegis/v1/aegis.proto`): `AegisRuntime` service (Execute/VerifyChain/GetReceiptChain), `Health` service, typed messages with `wasm_module` field in `ExecuteRequest`

**Phase 5 archive**: `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/` — contains proposal, design, specs, tasks, verify-report

**Key ADRs in DECISIONS.md**: AD-001 through AD-009, documenting decisions from Phases 0–5 including non-spec behavior, epoch-only interruption, mpsc channel relay, test isolation, receipt gaps, symlink traversal, gRPC boundary debt, Execute RPC invalidation, and happy-path result capture.

**Open items from Phase 5**:
- `hardening/grpc-boundary-tests` PR was blocked by AD-008 (Execute stub) — now resolved per AD-009 (result capture implemented, 90 tests pass)
- AD-007 gRPC boundary failure tests (S-701, S-702, S-704, S-721) are now implemented and passing in `tests/grpc_boundary.rs`
- Phase 5 is ready for archive after this documentation phase

### Affected Areas

- **`DECISIONS.md`** — Needs Phase 6 ADRs documenting: host functions vs WASI, test-utils feature flag rationale, mTLS over plain TLS, scope creep cuts (Q6)
- **`openspec/changes/archive/`** — Phase 6 documentation artifact goes here after completion
- **`openspec/specs/`** — Existing specs (filesystem-read, filesystem-write, grpc-runtime-server, signed-receipts, sandbox-init, aegis-runtime-binary, receipt-verifier) remain valid; no new spec changes needed for Phase 6 (documentation only)
- **`src/`** — No code changes; documentation of existing patterns only
- **`tests/`** — No test changes; documentation of existing test strategies only

### ADRs to Document in Phase 6

#### ADR-010: Host Functions vs WASI

**Context**: Wasmtime supports WASI (WebAssembly System Interface) as a standardized capability set. The aegis project chose to implement custom host functions (`aegis_fs_read`, `aegis_fs_write`) instead of using WASI preview1 or preview2.

**Decision**: Use custom host functions via `Linker::func_wrap` instead of WASI.

**Rationale**:
- **Fine-grained capability control**: WASI provides broad system access (fd_read, fd_write, path_open, etc.) that cannot be easily scoped to a single capability like `filesystem.read` with an `allowed_root`. Custom host functions allow per-capability closures that capture `allowed_root` and `max_bytes` at link time.
- **Security boundary clarity**: Each host function is explicitly named (`aegis_fs_read`, `aegis_fs_write`) and validated against the capability config before any filesystem operation. WASI's flat fd-based model would require additional authorization layers.
- **Path validation integration**: Custom functions embed the full path traversal check (`..` rejection, canonicalize + `starts_with` root check, symlink resolution) directly in the host function closure. WASI would require these checks in a separate policy layer.
- **Receipt emission**: Custom functions emit signed receipts at each validation point (path OOB, traversal, size exceed, success). WASI does not have hooks for receipt emission at the syscall level.
- **Future extensibility**: Custom host functions can be extended to `network.http`, `crypto.sign`, `crypto.verify` without WASI version compatibility constraints.

**Tradeoffs**:
- Not standardized — custom ABI means guest modules must import `aegis` namespace functions
- WASI modules from the ecosystem cannot run without an adapter layer
- More implementation effort per capability vs. using existing WASI imports

**Consequences**:
- Guest WASM modules must be compiled with `aegis` imports, not WASI imports (the `aegis_fs_read`/`aegis_fs_write` pattern)
- WASI modules produce linking errors at instantiation (S-4) — this is documented and expected
- The `Capability` enum maps directly to host function registration, creating a tight coupling between capability grants and host function exports

**Source**: `src/sandbox/mod.rs` `instantiate_with_capabilities()`, `src/capabilities/mod.rs`

---

#### ADR-011: test-utils Feature Flag for Signing Failure

**Context**: The `ReceiptEmitter` needs a mechanism to force signing failures in tests to verify fail-closed behavior (S-416, S-702). This requires a test-only method `force_signing_failure()` that is not available in production builds.

**Decision**: Use a Cargo feature flag `test-utils` to gate `force_signing_failure()` on `ReceiptEmitter`.

**Rationale**:
- **Compile-time isolation**: The `#[cfg(feature = "test-utils")]` attribute ensures the `force_signing_failure` field and method are completely absent from release builds. There is zero runtime overhead or attack surface.
- **Dev-dependency pattern**: The feature is enabled only for `aegis = { path = ".", features = ["test-utils"] }` in `[dev-dependencies]`. Production users who depend on `aegis` without this feature get a clean `ReceiptEmitter` without test hooks.
- **Explicit intent**: The feature name `test-utils` signals that anything behind it is test infrastructure, not production API. This prevents accidental use in production code.
- **Alternative considered**: Environment variable `AEGIS_TEST_MODE` (used in `src/grpc/handlers/mod.rs` for epoch interruption) was considered but rejected for `ReceiptEmitter` because env vars are runtime checks, not compile-time guarantees. The feature flag provides stronger isolation.

**Tradeoffs**:
- Requires `features = ["test-utils"]` on dev-dependency, adding a small cognitive overhead for new contributors
- Two code paths (cfg-gated) increase the surface area of `ReceiptEmitter`
- The `#[cfg(feature = "test-utils")]` pattern must be consistently applied — missed gates could leak test hooks into production

**Consequences**:
- `cargo test` automatically enables `test-utils` via dev-dependency
- `cargo build --release` does not include test hooks
- `force_signing_failure()` is the only test-only method on `ReceiptEmitter`; all other methods are production-ready

**Source**: `Cargo.toml` `[features] test-utils = []`, `src/receipts/mod.rs` `#[cfg(feature = "test-utils")]` blocks

---

#### ADR-012: mTLS over Plain TLS

**Context**: The gRPC boundary requires TLS for transport security. The project chose mutual TLS (mTLS) with client certificate validation over plain TLS (server-only authentication).

**Decision**: Require mTLS with CN/SAN client certificate validation (`AegisClientCertVerifier`) for all gRPC connections.

**Rationale**:
- **Mutual authentication**: Plain TLS only authenticates the server to the client. mTLS authenticates both parties — the server presents its cert, the client must present a cert signed by the configured CA with CN/SAN matching `expected_identity`. This is essential for the agent-gateway (Go) ↔ aegis-runtime (Rust) boundary where the caller identity must be verified.
- **Fail-closed by default**: `client_auth_mandatory()` returns `true` — connections without valid client certificates are rejected with `UNAUTHENTICATED`. There is no "optional mTLS" mode.
- **CN/SAN validation**: The custom `AegisClientCertVerifier` checks both Common Name (`CN=agent-gateway`) and Subject Alternative Names (DNS/URI) against `RuntimeConfig.tls.expected_identity`. This provides defense-in-depth: even if a CA issues a cert with only CN, SAN must also match; and vice versa.
- **Configurable identity**: The expected identity is not hardcoded — it's `expected_identity` in `TlsConfig`, allowing different environments (dev/staging/prod) to use different caller identities.
- **Plain TLS was rejected**: Plain TLS would allow any client with a valid CA-signed cert to connect, including unauthorized agent-gateway instances or malicious actors who obtain a CA-signed cert.

**Tradeoffs**:
- Certificate management complexity: requires CA, server cert, client cert for every deployment
- mTLS handshake adds latency (~1-2ms) to every gRPC call
- Certificate rotation requires coordination between client and server
- The `AegisClientCertVerifier` parses X.509 certs manually using `x509-parser` — potential fragility if cert formats change

**Consequences**:
- Every `aegis-runtime` deployment requires CA cert, server cert/key, and client cert/key configured in `RuntimeConfig`
- The `grpc_boundary.rs` integration tests generate ephemeral certs via `rcgen` to test mTLS
- If TLS is not configured (`config.server.tls = None`), the server warns but still starts — this is a known gap (no enforcement that mTLS is required in production)

**Source**: `src/grpc/tls.rs` `AegisClientCertVerifier`, `src/config/runtime.rs` `TlsConfig`, `src/grpc/server.rs` `build_tonic_tls_config`

---

#### ADR-013: Scope Creep Cuts (Q6)

**Context**: Phase 6 was originally scoped to include several features beyond documentation. Through the SDD process, these were cut to maintain focus and avoid introducing new implementation risks after Phase 5's close-call with false PASSes.

**Decision**: Phase 6 is documentation-only. All implementation features are deferred to future phases.

**Items cut from Q6 scope**:
1. **`network.http` capability implementation** — Was planned as the next capability after filesystem.read/write. Requires async HTTP client integration, URL validation, rate limiting. Deferred because it introduces new failure modes (network timeouts, DNS resolution) not covered by existing sandbox patterns.
2. **WASI compatibility layer** — Would allow existing WASI modules to run in aegis. Requires WASI host implementation or adapter. Deferred because it conflicts with the custom host function approach (ADR-010) and would dilute the capability-based security model.
3. **Fuel metering** — Was deferred from Phase 0 (AD-002) as "Phase 1+". Still not implemented because epoch-only interruption is sufficient for current security requirements. Fuel metering adds per-instruction overhead and complexity without strengthening the security boundary.
4. **Two-phase receipt emission** — Was proposed as a fix for the S-416-W-after-rename gap (AD-005). Requires a pending/receipt-commit protocol or external KMS with atomicity. Deferred because the current audit log + fail-closed trap approach is adequate for the threat model.
5. **GPU/compute capability** — Not in any spec. Would require Wasmtime host function for compute shaders. Deferred entirely — no spec exists.

**Rationale**:
- Phase 5 had 3 false PASSes (see Retrospective below) — introducing new implementation now risks repeating verification failures
- The documentation phase should solidify the architecture decisions before adding new capabilities
- Each deferred item has a clear reason for deferral and can be explored individually in future phases

**Consequences**:
- Phase 6 produces only documentation artifacts (this explore.md, ADRs, threat model, retrospective)
- `network.http` remains a capability variant in `Capability` enum but has no host function implementation
- Fuel metering remains deferred; epoch-only continues as the CPU limit mechanism
- The S-416 receipt gap remains documented but unfixed (mitigated by audit log)

---

### Threat Model

The aegis threat model covers three security boundaries: the WASM sandbox, the gRPC boundary, and the receipt chain.

#### 1. WASM Sandbox Threat Model

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

#### 2. gRPC Boundary Threat Model

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

#### 3. Receipt Chain Threat Model

| Threat | Mitigation | Evidence |
|--------|-----------|----------|
| Receipt forgery | Ed25519 signature verification in `ReceiptChain::verify_chain()` | REQ-413 |
| Chain manipulation | BLAKE3 hash chain: `prev_hash[i] = blake3(prev_hash[i-1] || canonical[i])` | REQ-410, REQ-411 |
| Timestamp replay | ±5 second tolerance in `verify_chain()` | REQ-412 |
| Wrong public key | `verify_chain()` rejects receipts signed by different key | REQ-413 |
| Receipt gap (S-416) | Explicit documentation + audit log; two-phase emission deferred | AD-005 |
| Forced signing failure | `test-utils` feature flag gates `force_signing_failure()`, compile-time isolated | ADR-011, S-416 |
| Private key in process | `Ed25519KeyPair` in `ReceiptEmitter` struct, never serialized, never in gRPC messages | REQ-720 |

#### 4. Known Gaps and Acceptable Risks

| Gap | Risk Level | Mitigation | Why Acceptable |
|-----|-----------|-----------|----------------|
| Epoch imprecision for tight loops | Medium | Host functions provide natural epoch check points | Phase 0 design choice (AD-002); fuel metering deferred |
| S-416 receipt gap after write+emit failure | Medium | Audit log + Trap return | Cannot undo host filesystem write; two-phase emission is deferred |
| TLS not enforced in production config | Medium | Server warns when TLS is `None` | Current deployments use mTLS; no mechanism to mandate TLS |
| `network.http` capability not implemented | Low | N/A | Deferred to future phase (ADR-013) |
| `AegisClientCertVerifier` uses manual X.509 parsing | Low | `x509-parser` library | Well-maintained library; CN/SAN checks are straightforward |
| No rate limiting beyond concurrency semaphore | Low | `max_concurrent` semaphore | Rate limiting is a different concern from concurrency limiting |

---

### Retrospective: Process Analysis

#### 3 False PASSes in Phase 5 (Pattern Started in Phase 3)

The Phase 5 verify process reported "PASS WITH WARNINGS" (36/36 requirements, 28/28 scenarios) despite the Execute RPC never actually loading or executing a WASM module. This is the **third false PASS in Phase 5's history**, but the pattern originated earlier with **AD-005 (Phase 3, S-416)**: the receipt gap after successful write+rename was documented as a known gap but the verify process did not flag it as a blocker — the same "stub/gap not flagged as blocker" pattern that repeated three times in Phase 5.

**This is the third false PASS in Phase 5's history** (fourth including the AD-005 precedent):

| # | False PASS | Root Cause | Lesson |
|---|-----------|------------|--------|
| 1 | Build declared closed while `execute_filesystem_read` was a stub | Verify relied on artifact existence and compile success, not behavioral validation | Verify must include manual spot-check of core runtime loop (WASM load → instantiate → execute → receipt) |
| 2 | Verify PASS reported 36/36 while Execute was a stub returning hardcoded string | The `execute_filesystem_read` stub had a TODO comment but was never flagged as blocking verification | Verify must check that critical paths (Execute → WASM execution → receipt) are tested end-to-end, not just compiled |
| 3 | Verify PASS reported 28/28 scenarios while S-701/702/704 were `@ignore` in gRPC tests | The gRPC boundary tests were marked `#[ignore]` because they couldn't pass without real WASM execution | Ignore-marked tests should be flagged as blockers, not silently excluded from PASS counts |

**Pattern corrected**: Future verify runs must include a "smoke test" that exercises the full runtime loop with a real WASM module, not just artifact existence checks. The `execute_rpc_happy_path_result_capture` test in `tests/grpc_boundary.rs` now serves as this smoke test.

**Pattern that worked**: The AD-008 → AD-009 sequence demonstrated honest documentation of verification gaps. Rather than pretending Phase 5 was complete, the team retroactively invalidated the closure and tracked the gaps as explicit decisions. This practice should be maintained.

#### Scope Creep Q6 Cut

Phase 6 was originally scoped to include `network.http` capability implementation, WASI compatibility, fuel metering, and other features. The SDD process identified that adding implementation now would risk repeating the Phase 5 false PASS pattern. The decision to cut Q6 scope to documentation-only was driven by:

1. **Verification fatigue**: Three false PASSes eroded confidence in the verify process. Adding new implementation without a hardened verify process would repeat the pattern.
2. **Architecture stability**: The custom host function approach (ADR-010) and mTLS boundary (ADR-012) are still being validated. New capabilities built on these foundations could expose new failure modes.
3. **Process integrity**: The `hardening/grpc-boundary-tests` PR was the explicit next task after Phase 5 closure. Starting Phase 6 implementation before that PR is merged would violate the process commitment.

**Patterns that worked**:
- **Separate binary for gRPC**: Fault isolation aligns with Phase 0 fail-closed principle (aegis crash ≠ gateway crash)
- **Shared `Arc<Mutex<ReceiptEmitter>>`**: Single hash chain across concurrent RPCs without per-sandbox chain fragmentation
- **`mpsc::channel` for thread handle relay**: Solves the `std::thread::current()` trap in `EpochInterrupter::new()` (AD-003)
- **`Sandbox::new_with_config(config, enable_epoch)`**: Clean test isolation without compromising production security (AD-004)
- **`test-utils` feature flag**: Compile-time isolation of test hooks from production code (ADR-011)
- **`AegisClientCertVerifier` with CN/SAN**: Strong client identity binding (ADR-012)
- **BLAKE3 hash chain + Ed25519 signatures**: Proven receipt integrity (90 tests pass)
- **Honest ADR documentation of gaps**: AD-005, AD-007, AD-008, AD-009 demonstrate the practice of documenting known issues rather than hiding them

**Patterns that need correction**:
- **Verify process reliability**: The verify tool must include behavioral smoke tests, not just artifact existence checks. The 3 false PASSes all resulted from verify trusting compilation success over runtime behavior.
- **Phase closure discipline**: Phases should not be declared closed until all critical-path tests pass end-to-end. The "stub is good enough" mentality led to the AD-008 invalidation.
- **Ignore-marked tests**: Tests marked `#[ignore]` should not be excluded from PASS counts. They should be tracked as explicit blockers.

---

### Recommendation

**Phase 6 should produce the following documentation artifacts:**

1. **`openspec/changes/archive/2026-09-11-phase6-documentation/`** — Archive folder with explore.md, ADRs (AD-010 through AD-013), threat model, and retrospective
2. **DECISIONS.md updates** — Add AD-010 (host functions vs WASI), AD-011 (test-utils feature flag), AD-012 (mTLS over plain TLS), AD-013 (scope creep cuts)
3. **Phase 5 archive** — Once this documentation phase is complete, archive Phase 5 to `openspec/changes/archive/2026-09-11-phase5-agent-gateway-integration/` (if not already archived)
4. **`hardening/grpc-boundary-tests` PR** — Should be merged or verified as complete before Phase 7 implementation begins

**Phase 7 (next implementation phase) should be scoped as:**
1. Merge `hardening/grpc-boundary-tests` PR
2. Archive Phase 5
3. Explore `network.http` capability implementation (new explore.md)
4. Consider fuel metering as Phase 8 (when CPU precision is needed)

### Risks

- **Documentation-only phases may be deprioritized**: Without implementation pressure, documentation artifacts may not get completed. The Phase 6 explore.md and ADRs should be treated as first-class artifacts.
- **Phase 5 archive may be delayed**: The Phase 5 archive has been blocked by AD-008 (Execute stub) → AD-009 (result capture) → now this documentation phase. Each dependency adds delay.
- **Threat model may be incomplete**: The threat model documented here covers the current architecture but may not capture all attack vectors (e.g., side-channel attacks on epoch timing, memory scraping).
- **Retrospective findings may not be acted upon**: The 3 false PASSes pattern may repeat if verify process improvements are not implemented as concrete changes (not just documentation).

### Ready for Proposal

**Yes** — This exploration has documented all the key decisions, threat model, and process retrospective needed for the Phase 6 documentation phase. The next step is to create the Phase 6 proposal/spec/design artifacts based on this exploration, then archive Phase 5, and prepare for Phase 7 implementation (starting with the `hardening/grpc-boundary-tests` PR).
