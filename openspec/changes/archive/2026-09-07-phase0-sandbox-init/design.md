# Design: Phase 0 Sandbox Initialization

## Technical Approach

Minimal Wasmtime sandbox with epoch-only CPU interruption and `StoreLimitsBuilder` enforcing four hard resource limits plus `trap_on_grow_failure(true)`. A single `std::thread` timer calls `engine.increment_epoch()` every 100ms. Integration tests compile hostile WAT modules via `wat::parse_str` and assert deterministic traps on resource exhaustion. No fuel metering, no capability grants, no WASI.

## Architecture Decisions

### Decision: Engine Configuration

| Option | Tradeoff | Decision |
|--------|----------|----------|
| `Config::new().epoch_interruption(true)` + no fuel | Simpler, epoch-only CPU limits; infinite loops without function calls may not yield | **Chosen** — matches Phase 0 scope; fuel is Phase 1+ |
| `Config::new().epoch_interruption(true).consume_fuel(true)` | More precise CPU accounting; adds fuel consumption overhead | Rejected — explicitly out of scope per proposal |

**Rationale**: Proposal and exploration both specify epoch-only interruption. Fuel metering is deferred to Phase 1.

### Decision: Store Limits via StoreLimitsBuilder

| Limit | Value | Rationale |
|-------|-------|-----------|
| `memory_size` | `1 << 20` (1 MB) | Prevents OOM; 16 pages at 64KB/page |
| `table_elements` | `1024` | Generous for function pointer tables; prevents fork-bomb |
| `instances` | `4` | Limits concurrent module instantiations |
| `memories` | `2` | Most modules need 1; 2 prevents fragmentation abuse |
| `trap_on_grow_failure` | `true` | Fail-closed: `memory.grow` beyond limit traps, doesn't return -1 |

**Rationale**: Direct use of `StoreLimits` (implements `ResourceLimiter`) is simplest and sufficient for Phase 0. Custom `ResourceLimiter` impl is unnecessary complexity.

### Decision: Epoch Timer Thread Implementation

| Option | Tradeoff | Decision |
|--------|----------|----------|
| `std::thread::spawn` with `Arc<AtomicBool>` stop flag + `JoinHandle::join()` in `Drop` | Simple, no async runtime coupling; manual shutdown management | **Chosen** — minimal, proves concept; graceful shutdown via `Drop` |
| `tokio::spawn` with task handle | Async-native cancellation; requires tokio runtime | Rejected — adds async complexity for a simple timer |

**Rationale**: `Engine` is `Clone + Send + Sync`, safe to move into a thread. `Drop` ensures no leaked threads.

### Decision: Epoch Timer Shutdown Mechanism

| Option | Tradeoff | Decision |
|--------|----------|----------|
| `std::thread::sleep(interval)` + `Arc<AtomicBool>` | Simple; shutdown waits up to interval (≤100ms) | Rejected — NFR requires fast cleanup |
| `thread::park_timeout(interval)` + `unpark()` in `Drop` | Immediate shutdown on drop; same complexity | **Chosen** — meets NFR "zero threads/handles leaked" + fast cleanup |

**Rationale**: `park_timeout` + `unpark()` gives deterministic immediate shutdown without waiting for the next tick. Minimal code change, same thread safety guarantees.

### Decision: Test WAT Compilation

| Option | Tradeoff | Decision |
|--------|----------|----------|
| `wat = "1.0"` dev-dependency + `wat::parse_str` in test code | Portable, `cargo test` self-contained; WAT embedded as string literals | **Chosen** — standard approach, no CI dependency |
| `wasm-tools` CLI as build script/fixture | Real .wat files; readable for complex modules | Rejected — breaks `cargo test` portability |

## Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                         Sandbox                                 │
│  ┌──────────────┐    ┌──────────────────────────────────────┐  │
│  │   Engine     │    │              Store                   │  │
│  │ (epoch_int:  │    │  ┌────────────────────────────────┐  │  │
│  │   true)      │    │  │ StoreLimits (ResourceLimiter)  │  │  │
│  │              │    │  │ - memory_size: 1MB             │  │  │
│  │  ┌────────┐  │    │  │ - table_elements: 1024         │  │  │
│  │  │Epoch   │──┼────┼──│ - instances: 4                 │  │  │
│  │  │Timer   │  │    │  │ - memories: 2                  │  │  │
│  │  │Thread  │  │    │  │ - trap_on_grow_failure: true   │  │  │
│  │  └────────┘  │    │  └────────────────────────────────┘  │  │
│  └──────────────┘    └──────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
           │                              │
           │ increment_epoch()            │ limiter(|state| &mut state.limits)
           ▼                              ▼
    Timer thread                   Module execution
    (100ms sleep)                   (traps on limit)
```

**Thread Lifecycle**:
1. `Sandbox::new_with_limits()` → creates `Engine` with epoch interruption
2. Clones `Engine` into `Arc` → spawns timer thread
3. Timer loop: `sleep(100ms)` → `engine.increment_epoch()`
4. `Drop` on `EpochInterrupter`: sets `stop_flag=true` → `join()` → thread exits

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modify | Complete rewrite: `SandboxConfig`, `Sandbox::new_with_limits()`, `EpochInterrupter` |
| `src/lib.rs` | Modify | Export `Sandbox::new_with_limits`, `SandboxConfig`, `EpochInterrupter` |
| `Cargo.toml` | Modify | Add `wat = "1.0"` to `[dev-dependencies]` |
| `tests/sandbox.rs` | Create | Integration tests with hostile WAT modules |

## Interfaces / Contracts

### SandboxConfig

```rust
/// Configuration for sandbox resource limits and epoch interval.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum linear memory size in bytes (default: 1 MB)
    pub memory_size: usize,
    /// Maximum table elements (default: 1024)
    pub table_elements: u32,
    /// Maximum instances per store (default: 4)
    pub instances: u32,
    /// Maximum memories per instance (default: 2)
    pub memories: u32,
    /// Epoch timer interval (default: 100ms)
    pub epoch_interval: Duration,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_size: 1 << 20,
            table_elements: 1024,
            instances: 4,
            memories: 2,
            epoch_interval: Duration::from_millis(100),
        }
    }
}
```

### EpochInterrupter

```rust
/// Manages the epoch timer thread lifecycle.
/// Drop implements immediate shutdown: unparks thread and joins.
pub struct EpochInterrupter {
    thread: std::thread::Thread,
    handle: JoinHandle<()>,
}

impl EpochInterrupter {
    /// Spawns a new epoch timer thread that calls `engine.increment_epoch()`
    /// every `interval`. Uses `park_timeout` for responsive shutdown.
    pub fn new(engine: Engine, interval: Duration) -> Self;
}

impl Drop for EpochInterrupter {
    fn drop(&mut self) {
        self.thread.unpark();
        let _ = self.handle.take().map(|h| h.join());
    }
}
```

### Sandbox

```rust
/// Secure WASM sandbox with epoch interruption and resource limits.
pub struct Sandbox {
    engine: Engine,
    store: Store<SandboxState>,
    epoch_interrupter: EpochInterrupter,
}

#[derive(Default)]
struct SandboxState {
    limits: StoreLimits,
}

impl Sandbox {
    /// Creates a new sandbox with the given configuration.
    /// Configures Engine with epoch_interruption(true), builds StoreLimits,
    /// and spawns the epoch timer thread.
    pub fn new_with_limits(config: SandboxConfig) -> Result<Self>;

    /// Instantiates a WASM module in the sandbox.
    pub fn instantiate(&mut self, wasm_bytes: &[u8]) -> Result<Instance>;
}
```

### Engine Creation

```rust
let config = Config::new()
    .epoch_interruption(true);  // No consume_fuel(true)
let engine = Engine::new(&config)?;
```

### Store Creation with Limits

```rust
let limits = StoreLimitsBuilder::new()
    .memory_size(config.memory_size)
    .table_elements(config.table_elements)
    .instances(config.instances)
    .memories(config.memories)
    .trap_on_grow_failure(true)
    .build();

let mut store = Store::new(&engine, SandboxState { limits });
store.limiter(|state| &mut state.limits);
```

### Epoch Timer Thread Spawning

```rust
let engine_clone = engine.clone();
let interval = config.epoch_interval;
let thread = std::thread::current();

let handle = std::thread::spawn(move || {
    loop {
        std::thread::park_timeout(interval);
        if std::thread::panicking() {
            break;
        }
        engine_clone.increment_epoch();
    }
});

EpochInterrupter { thread, handle }
```

## Testing Strategy

| Layer | What to Test | Approach |
|-------|--------------|----------|
| Unit | `SandboxConfig::default()` values | Assert default field values |
| Unit | `EpochInterrupter::drop()` joins thread | Spawn with short interval, drop, verify no panic |
| Integration | Hostile memory growth (REQ-005) | Compile WAT with `memory.grow 100`, call `_start`, assert `Result::is_err()` |
| Integration | Hostile infinite loop + epoch timeout (REQ-006) | Compile WAT with `loop { br 0 }`, use 10ms epoch, assert trap (not test timeout) |
| Integration | Store limits enforcement (REQ-002, REQ-003) | Test each limit boundary: table > 1024, instances > 4, memories > 2 |

### Test Module Structure: `tests/sandbox.rs`

```rust
use aegis::sandbox::{Sandbox, SandboxConfig};
use std::time::Duration;
use wat::parse_str;

const HOSTILE_MEMORY_GROWTH: &str = r#"
(module
  (memory 1)
  (func (export "_start")
    (memory.grow (i32.const 100))
    drop
  )
)
"#;

const HOSTILE_INFINITE_LOOP: &str = r#"
(module
  (func (export "_start")
    (loop (br 0))
  )
)
"#;

#[test]
fn hostile_memory_growth_traps() {
    let mut sandbox = Sandbox::new_with_limits(SandboxConfig::default())
        .expect("Failed to create sandbox");

    let wasm = parse_str(HOSTILE_MEMORY_GROWTH).expect("WAT parse failed");
    let instance = sandbox.instantiate(&wasm).expect("Module should instantiate");

    let func = instance.get_typed_func::<(), ()>(&mut sandbox.store, "_start")
        .expect("Function not found");
    
    let result = func.call(&mut sandbox.store, ());
    assert!(result.is_err(), "Expected trap on memory.grow beyond 1MB limit");
}

#[test]
fn hostile_infinite_loop_epoch_timeout() {
    // Use short epoch interval (10ms) for faster test
    let config = SandboxConfig {
        epoch_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let mut sandbox = Sandbox::new_with_limits(config)
        .expect("Failed to create sandbox");

    let wasm = parse_str(HOSTILE_INFINITE_LOOP).expect("WAT parse failed");
    let instance = sandbox.instantiate(&wasm).expect("Module should instantiate");

    let func = instance.get_typed_func::<(), ()>(&mut sandbox.store, "_start")
        .expect("Function not found");
    
    let result = func.call(&mut sandbox.store, ());
    assert!(result.is_err(), "Expected epoch deadline trap, not test timeout");
}
```

### Assertion Strategy

- **Memory growth test**: Assert `Result::is_err()` — trap is the expected outcome
- **Infinite loop test**: Assert `Result::is_err()` — trap must occur within test timeout (epoch interruption fires)
- **No graceful returns**: Both tests expect `Err`, never `Ok` with error code

## Error Handling

| Scenario | Behavior |
|----------|----------|
| `memory.grow` exceeds `memory_size` | Wasmtime trap (not -1 return) — `trap_on_grow_failure(true)` |
| Table allocation exceeds `table_elements` | Wasmtime trap |
| Instance creation exceeds `instances` | Wasmtime trap |
| Memory allocation exceeds `memories` | Wasmtime trap |
| Epoch deadline exceeded | Wasmtime trap (epoch interruption) |
| Module instantiation failure | `anyhow::Error` propagated (wasm validation error) |

**Fail-closed invariant (REQ-007)**: All resource exhaustion paths produce a Wasmtime `Trap`. The sandbox never catches and converts traps to graceful errors. Callers see `Result::Err` from `Func::call` and must handle it as fatal.

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `wat` | `1.0` (dev) | Parse WAT text to WASM bytes in integration tests |

Added to `Cargo.toml`:
```toml
[dev-dependencies]
wat = "1.0"
```

## Migration / Rollout

No migration required — this is a greenfield sandbox initialization. The old `Sandbox::new()` remains available (using `Engine::default()`) but is deprecated. All new code uses `Sandbox::new_with_limits()`.

Rollback plan (from proposal):
1. Revert `src/sandbox/mod.rs` to trivial `Engine::default()` + `Store::new`
2. Remove `wat` dev-dependency
3. Delete `tests/sandbox.rs`
4. Run `cargo test` → zero tests pass (baseline)

## Open Questions

- [x] Does `StoreLimitsBuilder::trap_on_grow_failure(true)` work reliably with wasmtime 24.0? **VALIDATED** — spike confirmed: traps with "forcing trap when growing memory to 6619136 bytes"; graceful -1 return when false.

## Threat Matrix

N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary.