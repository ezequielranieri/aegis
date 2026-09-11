# Retrospective: Process Analysis

## 3 False PASSes in Phase 5 (Pattern Started in Phase 3)

The Phase 5 verify process reported "PASS WITH WARNINGS" (36/36 requirements, 28/28 scenarios) despite the Execute RPC never actually loading or executing a WASM module. This is the **third false PASS in Phase 5's history**, but the pattern originated earlier with **AD-005 (Phase 3, S-416)**: the receipt gap after successful write+rename was documented as a known gap but the verify process did not flag it as a blocker — the same "stub/gap not flagged as blocker" pattern that repeated three times in Phase 5.

**This is the third false PASS in Phase 5's history** (fourth including the AD-005 precedent):

| # | False PASS | Root Cause | Lesson |
|---|-----------|------------|--------|
| 1 | Build declared closed while `execute_filesystem_read` was a stub | Verify relied on artifact existence and compile success, not behavioral validation | Verify must include manual spot-check of core runtime loop (WASM load → instantiate → execute → receipt) |
| 2 | Verify PASS reported 36/36 while Execute was a stub returning hardcoded string | The `execute_filesystem_read` stub had a TODO comment but was never flagged as blocking verification | Verify must check that critical paths (Execute → WASM execution → receipt) are tested end-to-end, not just compiled |
| 3 | Verify PASS reported 28/28 scenarios while S-701/702/704 were `@ignore` in gRPC tests | The gRPC boundary tests were marked `#[ignore]` because they couldn't pass without real WASM execution | Ignore-marked tests should be flagged as blockers, not silently excluded from PASS counts |

**Pattern corrected**: Future verify runs must include a "smoke test" that exercises the full runtime loop with a real WASM module, not just artifact existence checks. The `execute_rpc_happy_path_result_capture` test in `tests/grpc_boundary.rs` now serves as this smoke test.

**Pattern that worked**: The AD-008 → AD-009 sequence demonstrated honest documentation of verification gaps. Rather than pretending Phase 5 was complete, the team retroactively invalidated the closure and tracked the gaps as explicit decisions. This practice should be maintained.

## Scope Creep Q6 Cut

Phase 6 was originally scoped to include `network.http` capability implementation, WASI compatibility, fuel metering, and other features. The SDD process identified that adding implementation now would risk repeating the Phase 5 false PASS pattern. The decision to cut Q6 scope to documentation-only was driven by:

1. **Verification fatigue**: Three false PASSes eroded confidence in the verify process. Adding new implementation without a hardened verify process would repeat the pattern.
2. **Architecture stability**: The custom host function approach (ADR-010) and mTLS boundary (ADR-012) are still being validated. New capabilities built on these foundations could expose new failure modes.
3. **Process integrity**: The `hardening/grpc-boundary-tests` PR was the explicit next task after Phase 5 closure. Starting Phase 6 implementation before that PR is merged would violate the process commitment.

## Patterns that Worked

- **Separate binary for gRPC**: Fault isolation aligns with Phase 0 fail-closed principle (aegis crash ≠ gateway crash)
- **Shared `Arc<Mutex<ReceiptEmitter>>`**: Single hash chain across concurrent RPCs without per-sandbox chain fragmentation
- **`mpsc::channel` for thread handle relay**: Solves the `std::thread::current()` trap in `EpochInterrupter::new()` (AD-003)
- **`Sandbox::new_with_config(config, enable_epoch)`**: Clean test isolation without compromising production security (AD-004)
- **`test-utils` feature flag**: Compile-time isolation of test hooks from production code (ADR-011)
- **`AegisClientCertVerifier` with CN/SAN**: Strong client identity binding (ADR-012)
- **BLAKE3 hash chain + Ed25519 signatures**: Proven receipt integrity (90 tests pass)
- **Honest ADR documentation of gaps**: AD-005, AD-007, AD-008, AD-009 demonstrate the practice of documenting known issues rather than hiding them

## Patterns that Need Correction

- **Verify process reliability**: The verify tool must include behavioral smoke tests, not just artifact existence checks. The 3 false PASSes all resulted from verify trusting compilation success over runtime behavior.
  - **Corrective action**: Include a behavioral smoke test in every verify run that exercises the full runtime loop (WASM load → instantiate → execute → receipt) with a real WASM module, rather than artifact existence and compile checks alone.
- **Phase closure discipline**: Phases should not be declared closed until all critical-path tests pass end-to-end. The "stub is good enough" mentality led to the AD-008 invalidation.
  - **Corrective action**: Treat any stub or placeholder on a critical path as an explicit blocking finding; do not declare a phase closed until all critical-path tests pass end-to-end.
- **Ignore-marked tests**: Tests marked `#[ignore]` should not be excluded from PASS counts. They should be tracked as explicit blockers.
  - **Corrective action**: Track `#[ignore]`-marked tests as explicit blockers in the verify report and exclude them from PASS counts only with a documented, justified reason.