# Decision Records — aegis

## AD-001: trap_on_grow_failure(true) — Non-Spec Fail-Closed Behavior

**Date**: 2026-09-06
**Phase**: 0
**Status**: Accepted

### Context
`StoreLimitsBuilder::trap_on_grow_failure(true)` configures Wasmtime to raise a deterministic `Trap` when `memory.grow` exceeds the configured limit, instead of returning `-1` gracefully.

### Decision
Use `trap_on_grow_failure(true)` in Phase 0 despite it being non-spec-compliant behavior.

### Rationale
- **Fail-closed is the priority**: Phase 0's core guarantee is "any resource exhaustion = trap, never graceful error"
- The Wasm spec allows implementations to trap on `memory.grow` OOM; returning `-1` is a quality-of-implementation choice
- Spike validated: wasmtime 24.0 traps with message "forcing trap when growing memory to N bytes" — deterministic and correct for our security model
- Revisit in Phase 1+ when capability grants may need different semantics for specific hosts

### Consequences
- Modules exceeding memory limit trap immediately (no opportunity to handle OOM gracefully)
- Consistent with REQ-003 and REQ-007 (fail-closed cross-cutting invariant)
- Documented as intentional deviation from strict spec compliance

---

## AD-002: Epoch-Only CPU Interruption — No Fuel Metering

**Date**: 2026-09-06
**Phase**: 0
**Status**: Accepted

### Context
Wasmtime offers two mechanisms for CPU time limiting:
1. **Fuel metering** (`consume_fuel(true)`): instruments every instruction with fuel consumption
2. **Epoch interruption** (`epoch_interruption(true)`): periodic `engine.increment_epoch()` calls from a timer thread

### Decision
Use epoch-only interruption for Phase 0; defer fuel metering to Phase 1+.

### Rationale
- **Simplicity**: Single timer thread vs. per-instruction instrumentation
- **Scope fit**: Phase 0 proves isolation works; fuel is observability/metrics, not security boundary
- **Known limitation**: Epoch-only is imprecise for tight loops without function calls (Wasm only checks epoch at call/loop boundaries). A `loop { br 0 }` without calls may not yield for the full epoch interval.
- Acceptable for Phase 0 because:
  - Hostile integration test (REQ-006) uses 10ms epoch and validates trap occurs
  - Real modules with host calls (Phase 1+) will hit epoch checks at call boundaries
  - Fuel metering adds overhead without strengthening the security property for Phase 0

### Consequences
- **Imprecision documented**: Tight loops without calls may exceed epoch budget before trapping
- **Mitigation**: Phase 1 host functions will provide natural epoch check points
- **Future**: Fuel metering added when receipts need CPU consumption reporting (not as security limit)

---

## AD-003: EpochInterrupter — mpsc::channel for Thread Handle Relay

**Date**: 2026-09-06
**Phase**: 0
**Status**: Accepted

### Context
The original design specified `EpochInterrupter` with a simple `Arc<AtomicBool>` stop flag + `JoinHandle`, using `std::thread::current()` in `new()` to capture the spawned thread's handle.

### Problem
`std::thread::current()` inside `EpochInterrupter::new()` returns the **caller's thread handle**, not the spawned thread's. This causes:
- `unpark()` in `Drop` to unpark the wrong thread
- `join()` to wait on the caller thread (deadlock or panic)

### Decision
Use `std::sync::mpsc::channel` to relay the spawned thread's own `Thread` handle back to the creator:
1. Spawn thread
2. Spawned thread calls `std::thread::current()` and sends via channel
3. Creator receives via `rx.recv()` and stores in `EpochInterrupter.thread`
4. `Drop` calls `thread.unpark()` on the correct handle

### Implementation
```rust
pub fn new(engine: Engine, interval: Duration) -> Self {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let current = thread::current();  // This IS the spawned thread
        let _ = tx.send(current);         // Send its OWN handle back
        loop {
            thread::park_timeout(interval);
            if stop_clone.load(Ordering::Relaxed) { break; }
            engine.increment_epoch();
        }
    });

    let thread = rx.recv().expect("failed to receive thread handle");
    Self { thread, handle: Some(handle), stop }
}
```

### Consequences
- **Correctness**: `unpark()` targets the actual timer thread
- **Complexity cost**: +channel +handle relay (~15 lines)
- **No deadlock**: `rx.recv()` blocks briefly until spawned thread sends handle
- **Documented here**: Prevents future "simplification" that reintroduces the deadlock

---

## AD-004: Sandbox::new_with_config — Epoch Interruption Toggle for Test Isolation

**Date**: 2026-09-06
**Phase**: 0
**Status**: Accepted

### Context
Wasmtime's epoch counter is **process-global** — all `Engine` instances share the same monotonically increasing epoch. This means:
- Test A creates sandbox with 10ms epoch → epoch increments rapidly
- Test B creates sandbox with 10s epoch → epoch is already huge from Test A → immediate trap

This makes parallel test execution non-deterministic and single-threaded execution order-dependent.

### Decision
Add `Sandbox::new_with_config(config: SandboxConfig, enable_epoch: bool) -> Result<Self>` as a second constructor. When `enable_epoch=false`, the `Engine` is created without `epoch_interruption(true)`, and no `EpochInterrupter` is spawned. `Sandbox::new_with_limits(config)` remains the public API for production use (always enables epoch).

### Rationale
- **Test isolation is mandatory**: Cannot rely on test ordering or `--test-threads=1` for correctness
- **Production surface unchanged**: `new_with_limits` still enforces epoch interruption
- **Minimal API addition**: One extra method, one boolean parameter, no new types
- **Security unchanged**: `enable_epoch=false` only used in tests; production code cannot accidentally disable epoch because it's not the default constructor

### Consequences
- **Public API grows by 1 method** — documented in DECISIONS.md to prevent future "simplification" to a single constructor
- **Test-only capability**: `enable_epoch=false` only used in `tests/sandbox.rs` for memory-limit test
- **Documented here**: Prevents misinterpretation as a "security off switch" — it's strictly for test isolation against Wasmtime's global epoch counter

---

## Traceability

| Decision | Spec Req | Design Section | Implementation |
|----------|----------|----------------|----------------|
| AD-001 | REQ-003, REQ-007 | Decision: Store Limits | `StoreLimitsBuilder::trap_on_grow_failure(true)` |
| AD-002 | REQ-001, REQ-006 | Decision: Engine Config | `Config::epoch_interruption(true)`, no fuel |
| AD-003 | REQ-004 | Decision: Epoch Timer | `EpochInterrupter::new()` with mpsc channel |
| AD-004 | REQ-003, REQ-004 | Testing isolation | `Sandbox::new_with_config(config, enable_epoch: bool)` |
| AD-005 | REQ-510 | S-416-W-after-rename gap | `aegis_fs_write` trap + audit log after rename |
| AD-006 | REQ-503, REQ-512 | Symlink traversal + NUL byte fix | `aegis_fs_write` symlink `..` check + NUL trim |
| AD-007 | REQ-701, REQ-702, REQ-704, REQ-714, REQ-715 | gRPC boundary failure tests (explicit debt) | Documented in AD-007; follow-up hardening PR required |

---
## AD-005: S-416-W-after-rename — Receipt Gap on Write After Successful Rename

**Date**: 2026-09-08
**Phase**: 3 (enforcement)
**Status**: Accepted

### Context
`aegis_fs_write` uses atomic write (temp file + `std::fs::rename`). The critical sequence:
1. Validate path, size, guest memory
2. Write data to temp file (`.aegis_tmp` in `allowed_root`)
3. `std::fs::rename(temp, target)` — file is now visible at target path
4. Emit success receipt via `ReceiptEmitter`

If step 4 (emit) fails after step 3 (rename) succeeded, we have a **fundamental asymmetry** vs `aegis_fs_read`:
- In `read`, fail-closed = "no data returned to guest" (guest memory fully controlled)
- In `write`, the host filesystem side effect is already committed and visible to other processes — cannot be retracted

### Decision
If `emit()` fails after successful rename:
1. Log explicit "FAIL-CLOSED VIOLATION" audit record with full context (path, size, error)
2. Return `Trap` at API level (maintain fail-closed API contract)
2. **Do NOT attempt to delete/rollback the file** — it is already committed to host filesystem

This creates a **receipt chain gap**: a real write occurred on the host, but no signed receipt was added to the chain.

### Rationale
- **Cannot undo host filesystem write**: No portable, atomic "un-write" exists after `rename` succeeds
- **Blocking the write entirely** would require two-phase commit (pending receipt → confirm) or external transaction coordinator — out of scope for Phase 3
- **Returning success with no receipt** would silently violate the receipt chain integrity guarantee (Decision 4)
- **Trap + explicit audit log** maintains fail-closed at API level while being honest about the side effect

### Partial Mitigation: Idempotent Retry is Safe
REQ-506 specifies overwrite semantics (matching `std::fs::write`). If a caller receives `Trap` after S-416-W-after-rename and retries:
- Same path, same data → file is overwritten with identical content
- No duplication, no corruption, no state divergence
- Filesystem state is convergent; only the receipt chain has a gap
- This bounds the **operational** blast radius but does NOT close the **audit** gap

### Consequences
- **Receipt chain integrity**: Verifiable gap exists for any write where emit failed after rename
- **Audit trail**: Explicit "FAIL-CLOSED VIOLATION" log entry provides manual traceability
- **Security model**: Write capability has weaker audit guarantee than read capability
- **Future fix**: Two-phase receipt emission (pending before rename, confirm after) or external KMS with atomicity required to fully close this gap
- **Applicability**: This gap applies to ANY capability with persistent host side effects (future `network.http` POST, etc.)

### Traceability
- Design: `openspec/changes/phase3-enforcement/design.md` §6, §7.3
- Spec: `openspec/changes/phase3-enforcement/specs/filesystem-write/spec.md` REQ-510, S-416-W-after-rename
- Tests: `receipt_s416_write_after_rename` validates trap + log + file written

---
## AD-006: Symlink Traversal Bypass + NUL Byte Handling in Path Reading

**Date**: 2026-09-09
**Phase**: 3 (enforcement)
**Status**: Accepted

### Context
Two related security issues were discovered in `aegis_fs_write` after initial implementation:

1. **Symlink traversal bypass**: When a symlink points to a non-existent target containing `..` (e.g., `../../outside/evil.txt`), the `read_link` branch resolved the target against the parent directory but did NOT normalize the resulting path before the `starts_with` root check. Since `Path::starts_with` compares components literally (not normalized), a path like `/tmp/abc123/../../outside/evil.txt` passed the `starts_with` check against `/tmp/abc123` (first components match, then `..` vs end-of-prefix), allowing a write escape.

2. **NUL byte in path reading**: Guest memory paths include a NUL terminator (e.g., `evil_symlink.txt\0`). `std::str::from_utf8` preserves the NUL byte, causing filesystem operations to fail with "file name contained an unexpected NUL byte" instead of the intended security check.

### Decision
1. **Symlink target `..` rejection**: In the `read_link` branch, explicitly check if the symlink target contains any `..` components and reject immediately with a trap (consistent with REQ-503 traversal rejection for guest paths).

2. **Defense-in-depth path normalization check**: Before the `starts_with` root check, verify that `canonical_path` contains no `ParentDir` (`..`) components. This catches any path construction that leaves `..` unnormalized.

3. **NUL byte stripping**: Trim trailing NUL bytes from guest memory paths after UTF-8 decoding in both `aegis_fs_read` and `aegis_fs_write`.

### Rationale
- **Consistency with REQ-503**: Symlink targets with `..` are traversal attempts regardless of whether the target exists yet.
- **Defense in depth**: The `starts_with` check is not a substitute for proper path normalization. The explicit `..` component check catches any path that slips through.
- **Fail-closed consistency**: NUL byte handling should not change security outcomes — it's a parsing hygiene fix.

### Consequences
- **New test**: `symlink_write_relative_traversal_traps` explicitly exercises the symlink-with-relative-`..`-target-that-doesn't-exist attack vector.
- **NUL byte fix applies to both read and write**: `aegis_fs_read` also had the same latent bug (guest paths with NUL terminators would fail filesystem ops).
- **All 81 tests pass**, including the new symlink traversal test.

### Traceability
- Spec: `openspec/changes/phase3-enforcement/specs/filesystem-write/spec.md` REQ-503, REQ-512
- Design: `openspec/changes/phase3-enforcement/design.md` §5.2, §7
- Tests: `symlink_write_relative_traversal_traps`
- Commit: d2667fe

---
## AD-007: gRPC Boundary Failure Tests — Explicit Technical Debt

**Date**: 2026-09-09
**Phase**: 5 (agent-gateway integration)
**Status**: Accepted (Documented Debt)

### Context
Phase 5 implements the `aegis-runtime` gRPC server exposing `Execute`, `VerifyChain`, and `GetReceiptChain` RPCs over mTLS. The implementation is complete and verified (97 tests pass, clippy clean). However, the verify phase identified **4 gRPC boundary failure-path tests that are not yet implemented**:

| # | Scenario | Spec | Risk |
|---|----------|------|------|
| 1 | S-701: Execute violation trap via gRPC (path traversal, size exceed) | grpc-runtime-server | High — tests gRPC error mapping `FAILED_PRECONDITION` |
| 2 | S-702: Execute signing failure via gRPC | grpc-runtime-server | High — tests gRPC error mapping `INTERNAL` |
| 3 | S-704: VerifyChain tampered receipt via gRPC | grpc-runtime-server | High — tests gRPC error mapping for tampered payloads |
| 4 | S-721: Execute signing failure with corrupt key via gRPC | signed-receipts | High — tests gRPC error mapping with corrupt key |

These scenarios are **tested at the unit level** (sandbox violation traps, receipt signing failures, chain verification tampering) but **not at the gRPC boundary** — the new protocol layer introduced in Phase 5.

### Decision
Document these 4 tests as **explicit technical debt** (AD-007) rather than implementing them in Phase 5. Create a follow-up hardening PR to implement them.

### Rationale
- **Unit tests already cover the underlying logic**: Sandbox violation traps, receipt signing failures, chain verification tampering, and corrupt key handling are all verified at the unit/integration level in `tests/sandbox.rs` and `tests/receipts.rs`.
- **The gap is the gRPC boundary translation layer**: The missing tests verify that internal errors correctly map to gRPC status codes (`FAILED_PRECONDITION`, `INTERNAL`, `UNAUTHENTICATED`/`INVALID_CERT`) and proper error messages — this is the new surface Phase 5 exposes.
- **Implementing these tests requires significant gRPC test infrastructure**: Proper test setup requires generating valid/invalid mTLS certs, creating corrupt keys, tampering with protobuf receipts, and asserting exact gRPC status codes — this is substantial test infrastructure work beyond Phase 5 scope.
- **Explicit debt documentation aligns with project practice**: AD-005 and AD-006 document similar gaps honestly rather than pretending completeness.

### Consequences
- **Gap in gRPC-level verification**: The 4 failure paths are not exercised end-to-end through the gRPC boundary.
- **Mitigated by unit test coverage**: The underlying logic is solid; only the protocol translation layer is unverified.
- **Follow-up hardening PR required**: A dedicated PR to implement these 4 gRPC integration tests.
- **No functional regression risk**: The underlying logic is solid; only the protocol boundary translation is unverified.

### Follow-up
Create a hardening PR (`hardening/grpc-boundary-tests`) with tasks:
1. Add `execute_rpc_violation_trap_via_grpc` test (path traversal, size exceed)
2. Add `execute_rpc_signing_failure_via_grpc` test (corrupt key, signing failure)
3. Add `verify_chain_tampered_receipt_via_grpc` test
4. Add `execute_rpc_signing_failure_corrupt_key_via_grpc` test

**MANDATORY CONSTRAINT**: This hardening PR is the **immediate next task** after Phase 5 closure. It does NOT compete with Phase 6+ (`network.http`, WASM execution, etc.) for priority. It is the explicit next deliverable. Any attempt to start Phase 6 work before this PR is merged constitutes a process violation. Owner: ez (assigned). Target: next PR after Phase 5 closure.
- Verify Report: `verify-report.md` Warnings section (items 1-4)
- Specs: `openspec/changes/phase5-agent-gateway-integration/specs/grpc-runtime-server/spec.md` REQ-701, REQ-702, REQ-714, REQ-715
- Design: `openspec/changes/phase5-agent-gateway-integration/design.md` Testing Strategy §3.4
- Phase: 5 (agent-gateway integration)
- **Mandatory next task**: `hardening/grpc-boundary-tests` PR (blocks Phase 6+)

---
## AD-008: Execute RPC Stub — Phase 5 Closure Invalidated

**Date**: 2026-09-11
**Phase**: 5 (agent-gateway integration)
**Status**: Accepted (Retroactive Invalidation)

### Context
Phase 5 was declared **CLOSED** twice with a verify pass reporting "36/36 requirements, 28/28 scenarios PASS" — including REQ-714 ("Execute RPC SHALL create a sandbox per request from the provided config and run the capability") and REQ-715 ("Execute RPC SHALL trap and return success=false on any violation").

**Subsequent investigation revealed the Execute RPC handler never loads or executes a WASM module.** The implementation in `src/grpc/handlers/mod.rs`:

1. Creates a sandbox ✅
2. Parses config and validates capability grant ✅
3. Sets shared ReceiptEmitter on sandbox ✅
4. **Does NOT call `instantiate_with_capabilities(wasm_bytes, capabilities)`** ❌
5. **Does NOT invoke any exported function from an instantiated module** ❌
6. Returns a hardcoded success string via `execute_filesystem_read` stub ❌

The stub (lines 223-242) explicitly documents this:
```rust
// For now, we return a simple success indicator
// In a full implementation, this would load a WASM module and execute it
// with the filesystem.read capability registered via Linker
let result = format!("filesystem.read executed for capability: {}", cap.capability_name());
```

### Decision
**Phase 5 closure is retroactively invalidated.** The verify PASS did not reflect reality — no verify step exercised actual capability execution via wasmtime. Phase 5 remains **OPEN** until:

1. Execute RPC loads a WASM module (source TBD: embedded, config-specified, or uploaded)
2. Execute RPC calls `sandbox.instantiate_with_capabilities(wasm_bytes, capabilities)`
3. Execute RPC invokes the module's exported entry point (e.g., `run`, `execute`)
4. Execute RPC captures actual result (not hardcoded string) and emits receipt based on real execution
5. All S-700..S-715 scenarios verified against real execution (not stub)

### Consequences
- **`hardening/grpc-boundary-tests` PR is BLOCKED** — its S-701, S-702, S-721 tests are `@ignore` precisely because Execute doesn't execute. They cannot pass until this is fixed.
- **All Phase 5 "CLOSED" claims are retracted** — no archive, no delivery, no Phase 6 start.
- **Verify process reliability in question** — this is the second retroactive invalidation in Phase 5 (build declared closed while broken; verify PASS without real execution). Future verify runs must include manual spot-check of critical paths.

### Root Cause
Verify relied on artifact existence and compile success, not behavioral validation of the core runtime loop (WASM load → instantiate → execute → receipt). The `execute_filesystem_read` stub was written with a TODO comment but never flagged as blocking verification.

### Traceability
| Spec Requirement | Status | Evidence |
|-----------------|--------|----------|
| REQ-714 (Execute runs capability) | **PARTIAL** | WASM loads/instantiates/executes via `instantiate_with_capabilities` + `execute` export; result capture TODO |
| REQ-715 (Execute traps on violation) | **MET** | Path traversal & size exceed → gRPC success=false with error (S-701 PASS) |
| S-700 (Execute happy path) | **UNTESTABLE** | Requires real WASM execution with result capture |
| S-701 (Execute violation trap) | **PASS** | Traversal & size exceed correctly map to gRPC success=false with error message |
| S-702 (Execute signing failure) | **PASS** | Forced signing failure via test-only emitter → gRPC success=false with "receipt signing failure" error |
| S-721 (Execute corrupt key) | **NOT APPLICABLE** | Ed25519 validates at load time; key that "loads but fails to sign" cannot exist. Coverage in S-806/S-807 (startup validation). |

### Progress (2026-09-11)
| Scenario | Status | Notes |
|----------|--------|-------|
| S-701 | ✅ PASS | Path traversal & size exceed via WASM execution; FAILED_PRECONDITION → gRPC success=false |
| S-702 | ✅ PASS | `force_signing_failure()` on shared emitter → gRPC success=false with "receipt signing failure" |
| S-704 | ✅ PASS | VerifyChain tampered receipt → gRPC valid=false |
| S-721 | 📝 DOCUMENTED | Not applicable — Ed25519 validates at load time; see S-806/S-807 |

Execute RPC now:
- Loads WASM from `ExecuteRequest.wasm_module` (new protobuf field 3)
- Calls `instantiate_with_capabilities(wasm_bytes, capabilities)` 
- Invokes exported `execute()` function via wasmtime
- Returns `success=false` with descriptive error for violations (traversal, size, signing failure)
- Result capture from guest memory still TODO (returns placeholder)

Remaining for AD-008 closure:
1. Implement result retrieval from guest memory (uses `result_ptr` from execute export)
2. Re-run full Phase 5 verify with real WASM execution
3. Archive Phase 5

---
## AD-009: Execute RPC Result Capture — Happy Path Receipt Integrity Gap

**Date**: 2026-09-11
**Phase**: 5 (agent-gateway integration)
**Status**: Accepted (Open)

### Context
The Execute RPC now loads, instantiates, and executes WASM modules via wasmtime (AD-008). However, the **happy path result capture is not implemented**:

| Component | Current Behavior | Expected |
|-----------|------------------|----------|
| `execute_wasm_capability` return | `Ok(b"WASM execution completed")` placeholder | Actual guest memory content at `result_ptr` |
| `ReceiptEmitter.emit()` `result` param | `"success"` (hardcoded) | Actual guest execution result |
| `ReceiptEmitter.emit()` `path` param | `""` (empty) | Path/args from guest execution |
| `ReceiptEmitter.emit()` `size` param | `30` (placeholder length) | Actual result byte length |
| `ExecuteResponse.result` field | `"WASM execution completed"` | Actual guest result bytes |

The receipt **records fabricated data** (`result="success"`, `path=""`, `size=30`) instead of the actual guest WASM execution output. This breaks the core guarantee: *"cryptographic proof of what executed."*

### Root Cause
The `execute_wasm_capability` function:
1. Calls the exported `execute` function which returns `result_ptr: i32` (guest memory pointer)
2. Gets the memory export
3. **Does not read guest memory at `result_ptr`** — marked as TODO
4. Returns hardcoded placeholder bytes

### Consequences
- **Receipt integrity gap**: Every successful execution produces a receipt with fabricated `result="success"`, not the actual computation output
- **Verification impossibility**: `agent-gateway` cannot verify what the WASM actually computed
- **Audit trail corrupted**: Receipt chain shows successful executions but with meaningless result data
- **Different from AD-008**: AD-008 fixed *error path* mapping (S-701/702/704/721); this is the *happy path* integrity

### Decision
**This is a separate gap from AD-008** (which fixed gRPC error mapping for S-701/702/704/721). AD-008 closed *error boundary* tests; this is the *happy path* data integrity.

Create `AD-009` to track this. The hardening PR `hardening/grpc-boundary-tests` **can merge** — its scope was the 4 error scenarios (S-701/702/704/721), all now PASS. This AD tracks the happy-path work needed before Phase 5 archive.

### Follow-up (New Branch: `fix/execute-result-capture`) — **COMPLETED 2026-09-11**
1. ✅ Defined guest/host ABI: guest returns `(ptr, len)` tuple from `execute` export (Option B)
2. ✅ In `execute_wasm_capability`: read guest memory at `ptr` with length `len`, validate bounds
3. ✅ Pass actual result hash to `emit(capability, "execute", hash, path, actual_len)`
4. ✅ Return actual result bytes in `ExecuteResponse.result`
5. ✅ WAT fixtures updated: safe_read_module returns `(ptr, len)` via local; traversal/size_exceed use `unreachable`
6. ✅ Test: all 87 tests pass including S-701/S-702/S-704

### Traceability
| Spec Requirement | Status | Evidence |
|-----------------|--------|----------|
| REQ-701 (Execute runs capability) | **MET** | WASM executes, result captured |
| REQ-708 (ExecuteResponse contains result) | **MET** | Returns actual guest output bytes |
| S-700 (Execute happy path) | **MET** | Receipt has real hash, path, size |

### Tests Added (2026-09-11)
| Test | Verifies |
|------|----------|
| `execute_rpc_happy_path_result_capture` | ExecuteResponse.result = guest bytes; receipt result = BLAKE3 hash; path = config; size = bytes |
| `get_receipt_chain_returns_chain` | Chain has 4 receipts (2 read + 2 execute) after 2 Execute calls |
| `verify_chain_valid_via_grpc` | GetReceiptChain chain passes VerifyChain with valid=true |

**AD-009 STATUS: RESOLVED** — Happy path receipt integrity gap closed. All 90 tests pass. Ready for Phase 5 verify + archive.

---

## AD-010: Host Functions vs WASI

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
Wasmtime supports WASI (WebAssembly System Interface) as a standardized capability set. The aegis project chose to implement custom host functions (`aegis_fs_read`, `aegis_fs_write`) instead of using WASI preview1 or preview2.

### Decision
Use custom host functions via `Linker::func_wrap` instead of WASI.

### Rationale
- **Fine-grained capability control**: WASI provides broad system access (fd_read, fd_write, path_open, etc.) that cannot be easily scoped to a single capability like `filesystem.read` with an `allowed_root`. Custom host functions allow per-capability closures that capture `allowed_root` and `max_bytes` at link time.
- **Security boundary clarity**: Each host function is explicitly named (`aegis_fs_read`, `aegis_fs_write`) and validated against the capability config before any filesystem operation. WASI's flat fd-based model would require additional authorization layers.
- **Path validation integration**: Custom functions embed the full path traversal check (`..` rejection, canonicalize + `starts_with` root check, symlink resolution) directly in the host function closure. WASI would require these checks in a separate policy layer.
- **Receipt emission**: Custom functions emit signed receipts at each validation point (path OOB, traversal, size exceed, success). WASI does not have hooks for receipt emission at the syscall level.
- **Future extensibility**: Custom host functions can be extended to `network.http`, `crypto.sign`, `crypto.verify` without WASI version compatibility constraints.

### Tradeoffs
- Not standardized — custom ABI means guest modules must import `aegis` namespace functions
- WASI modules from the ecosystem cannot run without an adapter layer
- More implementation effort per capability vs. using existing WASI imports

### Consequences
- Guest WASM modules must be compiled with `aegis` imports, not WASI imports (the `aegis_fs_read`/`aegis_fs_write` pattern)
- WASI modules produce linking errors at instantiation (S-4) — this is documented and expected
- The `Capability` enum maps directly to host function registration, creating a tight coupling between capability grants and host function exports

### Traceability
- Design: `src/sandbox/mod.rs` `instantiate_with_capabilities()`, `src/capabilities/mod.rs`
- Spec: All filesystem specs (REQ-101..107, REQ-501..512)

---

## AD-011: test-utils Feature Flag for Signing Failure

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
The `ReceiptEmitter` needs a mechanism to force signing failures in tests to verify fail-closed behavior (S-416, S-702). This requires a test-only method `force_signing_failure()` that is not available in production builds.

### Decision
Use a Cargo feature flag `test-utils` to gate `force_signing_failure()` on `ReceiptEmitter`.

### Rationale
- **Compile-time isolation**: The `#[cfg(feature = "test-utils")]` attribute ensures the `force_signing_failure` field and method are completely absent from release builds. There is zero runtime overhead or attack surface.
- **Dev-dependency pattern**: The feature is enabled only for `aegis = { path = ".", features = ["test-utils"] }` in `[dev-dependencies]`. Production users who depend on `aegis` without this feature get a clean `ReceiptEmitter` without test hooks.
- **Explicit intent**: The feature name `test-utils` signals that anything behind it is test infrastructure, not production API. This prevents accidental use in production code.
- **Alternative considered**: Environment variable `AEGIS_TEST_MODE` (used in `src/grpc/handlers/mod.rs` for epoch interruption) was considered but rejected for `ReceiptEmitter` because env vars are runtime checks, not compile-time guarantees. The feature flag provides stronger isolation.

### Tradeoffs
- Requires `features = ["test-utils"]` on dev-dependency, adding a small cognitive overhead for new contributors
- Two code paths (cfg-gated) increase the surface area of `ReceiptEmitter`
- The `#[cfg(feature = "test-utils")]` pattern must be consistently applied — missed gates could leak test hooks into production

### Consequences
- `cargo test` automatically enables `test-utils` via dev-dependency
- `cargo build --release` does not include test hooks
- `force_signing_failure()` is the only test-only method on `ReceiptEmitter`; all other methods are production-ready

### Traceability
- Source: `Cargo.toml` `[features] test-utils = []`, `src/receipts/mod.rs` `#[cfg(feature = "test-utils")]` blocks
- Tests: `receipt_emitter_signing_failure_forced`, `execute_rpc_signing_failure_via_grpc`

---

## AD-012: mTLS over Plain TLS

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
The gRPC boundary requires TLS for transport security. The project chose mutual TLS (mTLS) with client certificate validation over plain TLS (server-only authentication).

### Decision
Require mTLS with CN/SAN client certificate validation (`AegisClientCertVerifier`) for all gRPC connections.

### Rationale
- **Mutual authentication**: Plain TLS only authenticates the server to the client. mTLS authenticates both parties — the server presents its cert, the client must present a cert signed by the configured CA with CN/SAN matching `expected_identity`. This is essential for the agent-gateway (Go) ↔ aegis-runtime (Rust) boundary where the caller identity must be verified.
- **Fail-closed by default**: `client_auth_mandatory()` returns `true` — connections without valid client certificates are rejected with `UNAUTHENTICATED`. There is no "optional mTLS" mode.
- **CN/SAN validation**: The custom `AegisClientCertVerifier` checks both Common Name (`CN=agent-gateway`) and Subject Alternative Names (DNS/URI) against `RuntimeConfig.tls.expected_identity`. This provides defense-in-depth: even if a CA issues a cert with only CN, SAN must also match; and vice versa.
- **Configurable identity**: The expected identity is not hardcoded — it's `expected_identity` in `TlsConfig`, allowing different environments (dev/staging/prod) to use different caller identities.
- **Plain TLS was rejected**: Plain TLS would allow any client with a valid CA-signed cert to connect, including unauthorized agent-gateway instances or malicious actors who obtain a CA-signed cert.

### Tradeoffs
- Certificate management complexity: requires CA, server cert, client cert for every deployment
- mTLS handshake adds latency (~1-2ms) to every gRPC call
- Certificate rotation requires coordination between client and server
- The `AegisClientCertVerifier` parses X.509 certs manually using `x509-parser` — potential fragility if cert formats change

### Consequences
- Every `aegis-runtime` deployment requires CA cert, server cert/key, and client cert/key configured in `RuntimeConfig`
- The `grpc_boundary.rs` integration tests generate ephemeral certs via `rcgen` to test mTLS
- If TLS is not configured (`config.server.tls = None`), the server warns but still starts — this is a known gap (no enforcement that mTLS is required in production)

### Traceability
- Source: `src/grpc/tls.rs` `AegisClientCertVerifier`, `src/config/runtime.rs` `TlsConfig`, `src/grpc/server.rs` `build_tonic_tls_config`
- Spec: REQ-713, REQ-716

---

## AD-013: Scope Creep Cuts (Q6)

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
Phase 6 was originally scoped to include several features beyond documentation. Through the SDD process, these were cut to maintain focus and avoid introducing new implementation risks after Phase 5's close-call with false PASSes.

### Decision
Phase 6 is documentation-only. All implementation features are deferred to future phases.

### Items Cut from Q6 Scope
1. **`network.http` capability implementation** — Requires async HTTP client integration, URL validation, rate limiting. Deferred because it introduces new failure modes (network timeouts, DNS resolution) not covered by existing sandbox patterns.
2. **WASI compatibility layer** — Would allow existing WASI modules to run in aegis. Requires WASI host implementation or adapter. Deferred because it conflicts with the custom host function approach (AD-010) and would dilute the capability-based security model.
3. **Fuel metering** — Was deferred from Phase 0 (AD-002) as "Phase 1+". Still not implemented because epoch-only interruption is sufficient for current security requirements. Fuel metering adds per-instruction overhead and complexity without strengthening the security boundary.
4. **Two-phase receipt emission** — Proposed as a fix for the S-416-W-after-rename gap (AD-005). Requires a pending/receipt-commit protocol or external KMS with atomicity. Deferred because the current audit log + fail-closed trap approach is adequate for the threat model.
5. **GPU/compute capability** — Not in any spec. Would require Wasmtime host function for compute shaders. Deferred entirely — no spec exists.

### Rationale
- Phase 5 had 3 false PASSes (see Retrospective below) — introducing new implementation now risks repeating verification failures
- The documentation phase should solidify the architecture decisions before adding new capabilities
- Each deferred item has a clear reason for deferral and can be explored individually in future phases

### Consequences
- Phase 6 produces only documentation artifacts (explore.md, ADRs, threat model, retrospective)
- `network.http` remains a capability variant in `Capability` enum but has no host function implementation
- Fuel metering remains deferred; epoch-only continues as the CPU limit mechanism
- The S-416 receipt gap remains documented but unfixed (mitigated by audit log)

### Traceability
- Related: AD-002 (fuel metering deferred), AD-005 (S-416 receipt gap), AD-010 (host functions vs WASI)

---

## AD-014: Network HTTP Capability Architecture (Phase 7)

**Date**: 2026-09-12
**Phase**: 7 (network.http)
**Status**: Accepted

### Context
`Capability::NetworkHttp` was a NO-OP never wired to any host function (ADR-013 cut item #1). Phase 7 reinstates it as a TLS-only HTTPS client host function `aegis_http_fetch` with host allowlist, method allowlist, per-execution token-bucket rate limit, and 1 MiB response cap. Violations trap fail-closed with signed fetch receipts (S-601..S-606, REQ-601..REQ-610).

### Decision
Adopt the six design decisions D1..D6 from the Phase 7 design as the network capability architecture.

| # | Decision | Choice | Alternatives | Rationale |
|---|----------|--------|--------------|-----------|
| D1 | HTTP client | `ureq = { version = "3", default-features = false, features = ["rustls"] }` (rustls 0.23, ring) | `reqwest::blocking` (hyper stack); ureq 2.12 | Blocking I/O matches `Linker::func_wrap`; pure Rust; ring matches existing deps; no gzip → body bytes are wire bytes for BLAKE3 |
| D2 | Policy carrier | `SandboxState.network_http: Option<NetworkHttpParams>` + `network_bucket`/`network_agent`/`network_fetch` | Extend `CapabilityConfig`; closure captures | `Option` is zero-cost for non-network sandboxes; `CapabilityConfig::from(NetworkHttp)` arm kept for match exhaustiveness only |
| D3 | Rate limit | Per-execution `TokenBucket` on `SandboxState` (refill-on-demand, no `Mutex`/timer) | Global bucket (out of scope); closure `Cell` (borrow-fragile) | Fresh `Sandbox` per Execute RPC resets the bucket → per-execution semantics (REQ-606, E-605) |
| D4 | Trap→gRPC | `msg.starts_with("network ")` as the FIRST guard in the Execute trap cascade; dispatch by catalog second word (endpoint/method/connection/timeout/response-size/rate) | Error-code envelope (overkill); positional insertion into the cascade (order-dependent) | S-605's "size"/"exceed" must never hit the generic fs size branch; S-601 embeds a guest-controlled URL (may contain `..`/`size`/`max`) that must never be evaluated against fs branches — only a prefix guard is structurally safe |
| D5 | REQ-610 URL | Host stores `FetchRecord { url, body_blake3, body_len }` on success; Execute handler reads it for the execute receipt (path = fetched URL, result = BLAKE3(body), size = body len) | Guest returns URL via WASM result (untrusted); comma-joined allowlist (rejected at review) | The host is the only party that knows which URL was actually fetched |
| D6 | AD timing | Design records the decision; `AD-014` transcribed at apply | — | decision-log: docs updated in the same work unit as the code |

### Implementation Notes (WU 2 refinements)
- `max_redirects(0)`: redirects could escape the host allowlist — fail-closed hardening over the design default.
- `http_status_as_error(false)`: 4xx/5xx responses return their bodies to the guest instead of trapping.
- URL parsing via `ureq::http::Uri` (no `url` dependency added); covers scheme/authority/port/path validation needs.
- Output-buffer-too-small now emits a trap receipt (path=URL, size=body.len()) before bailing — closes the AD-005-class audit gap (observed remote fetch with no signed receipt).
- Reinstates ADR-013 cut item #1 (read-only reference): `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md`.

### Implementation Notes (archive closure, 2026-09-12)
- **Ring-only TLS provider** (final repo state, commit `8b19898`): `rustls = { version = "0.23", default-features = false, features = ["ring","std","tls12","logging"] }` and `tokio-rustls = { default-features = false, features = ["ring","logging","tls12"] }` (Cargo.toml:39-42). aws-lc-rs is eliminated from the dependency graph — `cargo tree -i aws-lc-rs` matches nothing (exit 101). Consequence: the `prefer-post-quantum` feature is dropped (feature-tied to `aws_lc_rs`); D1's "ring matches existing deps" rationale is now faithfully implemented. Previously documented only in a Cargo.toml comment (:35-38).
- **D4 chain-walk implementation** (`src/grpc/handlers/mod.rs:342-389`): wasmtime 24 wraps host-fn traps in a backtrace frame, so the literal `msg.starts_with("network ")` never fires on the top-level `Display`. The network-first guard walks the error chain via `std::iter::successors(e.source(), ...)` and dispatches on the first frame whose message starts with `network ` (endpoint/method/connection/timeout/response size/rate limit prefixes), structurally isolating all network traps from the fs cascade branches.

### Consequences
- Execute-level receipts for performed fetches report the host-recorded `FetchRecord` (URL / BLAKE3(body) / body length, REQ-610); modules that never fetch fall back to the generic values.
- Network traps surface as gRPC `success=false` (FAILED_PRECONDITION) with clean mapped messages.
- Existing configs remain valid: `allowed_methods` defaults to `["GET"]` via serde default (REQ-601).
- Rollback is scoped: revert the `NetworkHttp` arm to NO-OP + the guard to the fs cascade, delete this AD — no receipt schema change.

### Traceability
- Spec: `openspec/changes/archive/2026-09-12-phase7-network-http/specs/network-http/spec.md` REQ-601..REQ-610, S-601..S-606 (archived 2026-09-12; promoted to `openspec/specs/network-http/spec.md`)
- Design: `openspec/changes/archive/2026-09-12-phase7-network-http/design.md` D1..D6, Data Flow
- Tests: `tests/network_http.rs` + `tests/grpc_boundary.rs` REQ-610 E2E (Phase 4)
- Related: ADR-013 (scope cuts), AD-010 (host functions vs WASI), AD-005 (receipt gap class)

---

## Traceability

| Decision | Spec Req | Design Section | Implementation |
|----------|----------|----------------|----------------|
| AD-001 | REQ-003, REQ-007 | Decision: Store Limits | `StoreLimitsBuilder::trap_on_grow_failure(true)` |
| AD-002 | REQ-001, REQ-006 | Decision: Engine Config | `Config::epoch_interruption(true)`, no fuel |
| AD-003 | REQ-004 | Decision: Epoch Timer | `EpochInterrupter::new()` with mpsc channel |
| AD-004 | REQ-003, REQ-004 | Testing isolation | `Sandbox::new_with_config(config, enable_epoch: bool)` |
| AD-005 | REQ-510 | S-416-W-after-rename gap | `aegis_fs_write` trap + audit log after rename |
| AD-006 | REQ-503, REQ-512 | Symlink traversal + NUL byte fix | `aegis_fs_write` symlink `..` check + NUL trim |
| AD-007 | REQ-701, REQ-702, REQ-704, REQ-714, REQ-715 | gRPC boundary failure tests (explicit debt) | Documented in AD-007; follow-up hardening PR required |
| AD-008 | REQ-714, REQ-715 | Execute RPC stub invalidation | Phase 5 closure retroactively invalidated |
| AD-009 | REQ-701, REQ-708, S-700 | Execute RPC result capture | Guest memory read at `result_ptr/len` |
| AD-010 | REQ-101..107, REQ-501..512 | Host functions vs WASI | `Linker::func_wrap` custom host functions |
| AD-011 | REQ-433, S-416, S-702 | test-utils feature flag | `#[cfg(feature = "test-utils")]` on `ReceiptEmitter` |
| AD-012 | REQ-713, REQ-716 | mTLS over plain TLS | `AegisClientCertVerifier` with CN/SAN |
| AD-013 | Phase 6 scope | Scope creep cuts | `network.http`, WASI, fuel, two-phase emission deferred |
| AD-014 | REQ-601..REQ-610, S-601..S-606 | D1..D6 | `aegis_http_fetch` host fn + `FetchRecord`-backed execute receipt |