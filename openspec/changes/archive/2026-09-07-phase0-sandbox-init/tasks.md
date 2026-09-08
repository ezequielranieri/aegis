# Tasks: Phase 0 Sandbox Initialization

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~280-350 |
| 400-line budget risk | Medium |
| Chained PRs recommended | No |
| Suggested split | Single PR (all phases) |
| Delivery strategy | ask-on-risk |
| Chain strategy | pending |

Decision needed before apply: Yes
Chained PRs recommended: No
Chain strategy: pending
400-line budget risk: Medium

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | SandboxConfig + EpochInterrupter foundation | PR 1 | `cargo test --lib` | Unit tests for config defaults + drop | `src/sandbox/mod.rs` changes only |
| 2 | Sandbox::new_with_limits + instantiate | PR 2 | `cargo test --lib` | Sandbox construction with limits | Same file, separable methods |
| 3 | Integration tests (REQ-005, REQ-006) | PR 3 | `cargo test --test sandbox` | `tests/sandbox.rs` hostile WAT modules | `tests/sandbox.rs` new file, no prod impact |

## Phase 1: Foundation / Infrastructure

- [x] 1.1 Verify `wat = "1.0"` exists in `Cargo.toml` `[dev-dependencies]` and `cargo check` resolves it; add `use std::time::Duration` import to `src/sandbox/mod.rs`
- [x] 1.2 Replace trivial `Sandbox` struct in `src/sandbox/mod.rs` with `SandboxConfig` struct: `memory_size: usize` (default `1 << 20`), `table_elements: u32` (default 1024), `instances: usize` (default 4), `memories: usize` (default 2), `epoch_interval: Duration` (default `Duration::from_millis(100)`), plus `impl Default`
- [x] 1.3 Add `#[derive(Debug, Clone)]` to `SandboxConfig` and document each field with `///` comments

## Phase 2: Core Implementation

- [x] 2.1 Create `EpochInterrupter` struct in `src/sandbox/mod.rs` with `thread: std::thread::Thread`, `handle: Option<JoinHandle<()>>`, and `stop: Arc<AtomicBool>` fields; implement `new(engine: Engine, interval: Duration) -> Self` that spawns a `std::thread` calling `engine.increment_epoch()` every `interval` using `park_timeout(interval)` in the loop, with channel-based thread handle relay
- [x] 2.2 Implement `Drop` for `EpochInterrupter`: set stop flag, call `self.thread.unpark()`, then `self.handle.take().map(|h| h.join())` for immediate graceful shutdown — no sleeping for shutdown
- [x] 2.3 Create `SandboxState` struct with `limits: StoreLimits` field; build `StoreLimits` via `StoreLimitsBuilder::new().memory_size(config.memory_size).table_elements(config.table_elements).instances(config.instances).memories(config.memories).trap_on_grow_failure(true).build()` and attach via `store.limiter(|state| &mut state.limits)`
- [x] 2.4 Implement `Sandbox::new_with_limits(config: SandboxConfig) -> Result<Self>` that creates `Engine` with `Config::new().epoch_interruption(true)` (no fuel), builds `Store` with `SandboxState`, and spawns `EpochInterrupter` — replacing the old `Sandbox::new()`
- [x] 2.5 Implement `Sandbox::instantiate(&mut self, wasm_bytes: &[u8]) -> Result<Instance>` that compiles `Module::new(&self.engine, wasm_bytes)` and creates `Instance::new(&mut self.store, &module, &[])`
- [x] 2.6 Update `src/lib.rs` to add `pub use sandbox::{Sandbox, SandboxConfig, EpochInterrupter};` re-exports alongside existing `pub mod sandbox`

## Phase 3: Testing / Verification

- [x] 3.1 Unit test `SandboxConfig::default()`: assert `memory_size == 1 << 20`, `table_elements == 1024`, `instances == 4`, `memories == 2`, `epoch_interval == Duration::from_millis(100)`
- [x] 3.2 Unit test `EpochInterrupter::drop()` joins thread: spawn with short interval, drop, verify no panic and thread terminates
- [x] 3.3 Create `tests/sandbox.rs` with `HOSTILE_MEMORY_GROWTH` WAT constant (`memory.grow (i32.const 100)`) and `HOSTILE_INFINITE_LOOP` WAT constant (`loop { br 0 }`); add `#[test] fn hostile_memory_growth_traps()` — create sandbox with defaults, parse WAT via `wat::parse_str`, instantiate, call `_start`, assert `result.is_err()`
- [x] 3.4 Add `#[test] fn hostile_infinite_loop_epoch_timeout()` to `tests/sandbox.rs` — use `SandboxConfig { epoch_interval: Duration::from_millis(10), ..Default::default() }`, parse `HOSTILE_INFINITE_LOOP`, instantiate, call `_start`, assert `result.is_err()` confirming epoch trap (not test runner timeout)
- [x] 3.5 Run `cargo test` to verify all unit and integration tests pass; confirm fail-closed invariant: every resource exhaustion produces a Wasmtime `Trap` (never `-1` or graceful return)

## Phase 4: Cleanup / Documentation

- [x] 4.1 Add `/// Fail-closed invariant: all resource exhaustion paths produce Wasmtime Trap, never graceful errors` doc comment to `Sandbox` and `EpochInterrupter`
- [x] 4.2 Add `/// Trap on grow failure: memory.grow beyond limit raises Trap, not -1 return` comment at `StoreLimitsBuilder` construction site
- [x] 4.3 Run `cargo clippy -- -D warnings` and `cargo fmt -- --check` to verify code quality
- [x] 4.4 Remove old `Sandbox::new()` and `Default` impl if fully replaced by `new_with_limits` — verify no callers reference the old API
