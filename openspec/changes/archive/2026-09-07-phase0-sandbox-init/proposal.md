# Proposal: Phase 0 Sandbox Initialization with Fail-Closed Defaults

## Intent

Establish a minimal Wasmtime sandbox that proves CPU/memory isolation BEFORE adding capabilities. The sandbox must fail-closed: any resource exhaustion (memory growth, instance limits, table limits) triggers a deterministic Wasmtime trap, not a graceful error return. This is the foundation for all future capability grants.

## Scope

### In Scope
- `Engine` with `epoch_interruption(true)` (no fuel metering)
- `StoreLimitsBuilder` with 4 hard limits: memory_size(1MB), table_elements(1024), instances(4), memories(2)
- `trap_on_grow_failure(true)` — non-spec but fail-closed
- Single `std::thread` timer calling `engine.increment_epoch()` every 100ms
- Integration test: hostile `.wat` module attempting `memory.grow 100` → expects trap
- Dev-dependency: `wat = "1.0"` for WAT parsing in tests

### Out of Scope
- WASI preview version selection
- Host function ABI (WIT vs func_wrap)
- Any capability grants (filesystem, network, etc.)
- Receipts, policy engine, observability
- Configurable epoch cadence (hardcoded 100ms for Phase 0)

## Capabilities

### New Capabilities
- `sandbox-init`: Minimal Wasmtime sandbox with epoch interruption and StoreLimitsBuilder enforcing fail-closed resource limits

### Modified Capabilities
- None

## Approach

Follow the recommended approach from exploration (Approach 1):

1. **Engine config**: `Config::new().epoch_interruption(true)` — no `consume_fuel(true)`
2. **Store limits**: `StoreLimitsBuilder` with 4 limits + `trap_on_grow_failure(true)`
3. **Epoch timer**: Single `std::thread::spawn` loop sleeping 100ms, calling `engine.increment_epoch()`, with graceful shutdown via `Arc<AtomicBool>` stop flag + `JoinHandle::join()` in `Drop`
4. **Integration test**: `tests/sandbox.rs` compiles hostile WAT via `wat::parse_str`, instantiates, calls exported `_start`, asserts trap on `memory.grow`
5. **Dev dependency**: Add `wat = "1.0"` to `Cargo.toml` `[dev-dependencies]`

Estimated effort: ~200 lines Rust + 1 test file + 1 hostile WAT module

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modified | Complete rewrite: new `Sandbox::new_with_limits()`, epoch timer thread, StoreLimitsBuilder |
| `Cargo.toml` | Modified | Add `wat = "1.0"` to dev-dependencies |
| `tests/sandbox.rs` | New | Integration test with hostile WAT module expecting trap |
| `src/lib.rs` | Modified | Expose `Sandbox::new_with_limits()` and `EpochInterrupter` type (with shutdown) |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| `trap_on_grow_failure(true)` is non-spec-compliant | High | Document as Phase 0 deviation in DECISIONS.md; revisit in Phase 1 with proper grow failure handling |
| Epoch-only CPU limits imprecise for tight loops | Medium | Document limitation; fuel metering is Phase 1+ consideration |
| `wat` crate version incompatibility with wasmtime 24.0 | Low | `wat` is independent; verify on first `cargo test` |

## Rollback Plan

Revert `src/sandbox/mod.rs` to trivial `Engine::default()` + `Store::new` implementation. Remove `wat` dev-dependency. Delete `tests/sandbox.rs`. Run `cargo test` to confirm zero tests pass (baseline).

## Dependencies

- None for Phase 0 (no external services, no capability grants)

## Success Criteria

- [ ] `cargo test` passes
- [ ] Hostile `.wat` module attempting `memory.grow 100` triggers Wasmtime trap (not graceful error return)
- [ ] Epoch interruption fires (sub-second timeout resolution verified indirectly via test completion)
- [ ] All 4 StoreLimitsBuilder limits are enforced (memory, table, instances, memories)

## Next Phase Trigger

Phase 1: Filesystem read-only capability grant with WIT-defined host functions and receipt emission.