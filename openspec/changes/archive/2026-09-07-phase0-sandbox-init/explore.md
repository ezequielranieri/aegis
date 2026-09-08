## Exploration: Phase 0 Sandbox Initialization

### Current State

The aegis project is a skeleton Rust/Wasmtime codebase at version 0.1.0. The `src/sandbox/mod.rs` contains only a trivial `Sandbox` struct using `Engine::default()` and `Store::new(&engine, ())` with **no** epoch interruption, **no** resource limits, and **no** StoreLimitsBuilder. All other modules (`capabilities`, `receipts`, `policy`, `observability`) are stub trait/struct definitions. There are **zero tests** — integration tests are marked as unavailable in `openspec/testing-capabilities.md`.

Key existing code (`src/sandbox/mod.rs`):
```rust
pub struct Sandbox { engine: Engine, store: Store<()> }
impl Sandbox {
    pub fn new() -> Result<Self> {
        let engine = Engine::default();
        let store = Store::new(&engine, ());
        Ok(Self { engine, store })
    }
}
```

`Cargo.toml` dependencies: `wasmtime = "24.0"`, `tokio = { version = "1.0", features = ["full"] }`, plus serde, tracing, opentelemetry, ring, hex, anyhow, thiserror.

The `rust-wasmtime-sandbox` skill mandates `epoch_interruption(true)` and `consume_fuel(true)`, but Phase 0 explicitly excludes fuel metering — only epoch interruption is used.

### Affected Areas

- **`src/sandbox/mod.rs`** — The entire file needs replacement: `Engine` must be created from a custom `Config` with `epoch_interruption(true)`, `Store` must carry `StoreLimits` via `StoreLimitsBuilder`, and a `ResourceLimiter` must be attached.
- **`Cargo.toml`** — Dev-dependency `wat` crate needed for integration tests to parse WAT text to WASM bytes.
- **`tests/sandbox.rs`** (new) — Integration test file that compiles hostile .wat, instantiates, and expects a trap.
- **`src/lib.rs`** — May need to expose new `Sandbox::new_with_limits()` constructor and `EpochInterrupter` type.

### Approaches

#### Approach 1: Minimal Sandbox with Timer Thread Epoch Ticking (RECOMMENDED)

**Config/Engine:**
```rust
let config = Config::new().epoch_interruption(true);
let engine = Engine::new(&config)?;
```
No `consume_fuel(true)` — fuel metering is explicitly out of scope for Phase 0.

**StoreLimitsBuilder with 4 limits:**
```rust
let limits = StoreLimitsBuilder::new()
    .memory_size(1 << 20)          // 1 MB max linear memory per instance
    .table_elements(1024)          // Max 1024 elements per table
    .instances(4)                  // Max 4 instances per store
    .memories(2)                   // Max 2 linear memories per instance
    .trap_on_grow_failure(true)    // Trap (not return -1) on growth failure
    .build();
```

Rationale for each limit:
- `memory_size(1 << 20)` — 1MB is sufficient for sandboxing proof-of-concept modules; prevents OOM attacks
- `table_elements(1024)` — Tables are only needed for function pointers/callbacks; 1024 is generous for sandbox code
- `instances(4)` — Prevents fork-bomb style instantiation attacks
- `memories(2)` — Most modules need only one linear memory; limiting to 2 prevents memory fragmentation abuse
- `trap_on_grow_failure(true)` — Ensures `memory.grow` beyond limit produces a deterministic trap (not a silent -1 return)

**Store construction with limits:**
```rust
struct SandboxState { limits: StoreLimits }
let mut store = Store::new(&engine, SandboxState { limits });
store.limiter(|state| &mut state.limits);
```

**Epoch interruption — single timer thread:**
```rust
std::thread::spawn(move || {
    loop {
        std::thread::sleep(Duration::from_millis(100));
        engine.increment_epoch();
    }
});
```
The `Engine` is `Clone + Send + Sync`, so it can be moved into a `std::thread`. The docs recommend weak references, but for Phase 0 simplicity, cloning the `Arc`-interned engine is acceptable. The 100ms cadence provides sub-second timeout resolution.

**Pros:**
- Minimal, focused implementation
- Proves isolation works (CPU via epoch timeout, memory via StoreLimits)
- Single test validates the entire sandbox
- Matches the explicit scope: epoch-only, 4 limits, fail-closed

**Cons:**
- `std::thread::spawn` without tokio means the timer thread isn't managed by the async runtime
- No graceful shutdown mechanism for the epoch thread (defer to Phase 1+)
- `trap_on_grow_failure(true)` is technically non-spec-compliant but practical for sandboxing

**Effort:** Low (~200 lines of Rust + 1 .wat file)

#### Approach 2: Using tokio::spawn for Timer Thread

Replace `std::thread::spawn` with `tokio::spawn` to manage the epoch ticker within the async runtime.

**Pros:**
- Consistent with the async runtime already in use (tokio full features)
- Can be cancelled via tokio task handle

**Cons:**
- Requires `#[tokio::main]` context or a tokio runtime handle
- `increment_epoch` is called from async context, adding unnecessary complexity for a simple timer
- `Engine` may not be `Send` in all async contexts (though it is in practice)

**Effort:** Low, but adds unnecessary async complexity for Phase 0

#### Approach 3: Using wasm-tools CLI for .wat Compilation in Tests

Instead of adding `wat` crate as dev-dependency, use `wasm-tools` CLI as a build script or test fixture.

**Pros:**
- No new crate dependency
- Real .wat files as test fixtures

**Cons:**
- Requires `wasm-tools` installed on the system
- Not portable; CI must install it
- Breaks `cargo test` as a standalone command

**Effort:** Medium — adds CI complexity, breaks test portability

### Recommendation

**Approach 1** is recommended. It is the simplest, most focused implementation that directly addresses all three Phase 0 requirements (epoch-only interruption, StoreLimitsBuilder with 4 limits, fail-closed integration test). The `std::thread` timer is sufficient for proving the concept; tokio management can be added later without changing the architecture.

The `wat` crate should be added as a dev-dependency for the integration test. It is the standard approach for compiling WAT text to WASM bytes in Rust test code and keeps `cargo test` fully self-contained.

### Hostile .wat Module Source

The integration test needs a module that:
1. Starts with a small memory (1 page = 64KB)
2. Actively grows memory beyond the 1MB limit
3. With `trap_on_grow_failure(true)`, this traps on `memory.grow`

```wat
;; tests/hostile_memory_growth.wat
(module
  (memory 1)  ;; 1 page = 64KB initial
  (func (export "_start")
    (loop
      ;; Attempt to grow memory by 1 page each iteration
      ;; With limit of 1MB (16 pages), this will exceed after ~16 iterations
      ;; With trap_on_grow_failure(true), this traps instead of returning -1
      memory.grow (i32.const 1)
      drop
      br 0
    )
  )
)
```

Actually, 1MB / 64KB = 16 pages. So starting at 1 page and growing by 1 page each iteration, the 16th `memory.grow` attempt (reaching page 17 = 1.0625MB) exceeds the 1MB limit and traps.

Alternatively, a more aggressive module:
```wat
(module
  (memory 1)
  (func (export "_start")
    (memory.grow (i32.const 100))  ;; Try to grow by 100 pages immediately
    drop
  )
)
```
This immediately exceeds any reasonable limit and traps on the single `memory.grow` call.

### Test Strategy

**Integration test** (`tests/sandbox.rs`):

```rust
use aegis::sandbox::Sandbox;
use wat::parse_str;

#[test]
fn hostile_memory_growth_traps() {
    // 1. Create sandbox with limits
    let mut sandbox = Sandbox::new_with_limits(SandboxLimits {
        memory_size: 1 << 20,   // 1MB
        table_elements: 1024,
        instances: 4,
        memories: 2,
    }).expect("Failed to create sandbox");

    // 2. Compile hostile .wat
    let wat = r#"(module (memory 1) (func (export "_start") (memory.grow (i32.const 100)) drop))"#;
    let wasm_bytes = parse_str(wat).expect("Failed to parse WAT");

    // 3. Instantiate — should succeed (module loads fine)
    let instance = sandbox.instantiate(&wasm_bytes).expect("Module should instantiate");

    // 4. Call _start — should TRAP due to memory.grow exceeding limit
    let func = instance.get_func(&mut sandbox.store, "_start").unwrap();
    let result = func.call(&mut sandbox.store, &[]);
    
    // 5. Assert trap occurred
    assert!(result.is_err(), "Expected trap on memory.grow beyond limit");
}
```

Note: The exact API for calling exported functions depends on the Wasmtime 24.0 version — `get_typed_func` or `get_func` may be used. The key assertion is that calling the function returns an error (trap).

**Test execution:**
```bash
cargo test --test sandbox -- hostile_memory_growth_traps
```

### Open Questions and Tradeoffs

1. **Epoch cadence (100ms)** — The 100ms timer interval is a placeholder. Too aggressive (10ms) wastes CPU; too lax (1s) allows long-running malicious code. Phase 1 should make this configurable via `Policy`.

2. **`trap_on_grow_failure(true)` spec compliance** — Wasmtime docs note this is "not necessarily spec-compliant." For Phase 0 sandboxing, this is acceptable and desirable (deterministic trap). But it should be documented as a Phase 0 deviation.

3. **Engine lifetime with `std::thread`** — Moving `Engine` into a spawned thread works but there's no mechanism to stop the thread when the sandbox is dropped. A `Drop` impl or `JoinHandle` tracking is needed for production use (Phase 1+).

4. **`wat` crate version** — Need to verify compatibility with wasmtime 24.0. The `wat` crate is independent and should work with any Wasmtime version.

5. **StoreLimits vs ResourceLimiter trait** — The docs show `StoreLimits` implements `ResourceLimiter` and is attached via `store.limiter()`. However, the existing `rust-wasmtime-sandbox` skill mentions a custom `ResourceLimiter` impl. For Phase 0, using `StoreLimits` directly (which implements `ResourceLimiter`) is simpler and sufficient.

6. **No fuel metering implications** — Without `consume_fuel(true)`, infinite loops that don't trigger epoch interruption could still run indefinitely. The epoch timer provides CPU limits, but tight loops that don't call imported functions might not check the epoch frequently enough. This is a known limitation of epoch-only interruption vs fuel metering.

7. **WAT test fixture format** — Should we embed .wat as strings in test code (using `wat::parse_str`) or write .wat files as test fixtures? Strings are simpler and more portable; files are more readable for complex modules.

### Implementation Plan

1. Add `wat = "1.0"` as dev-dependency to `Cargo.toml`
2. Rewrite `src/sandbox/mod.rs` with `Sandbox::new_with_limits()`, epoch timer thread, and `StoreLimitsBuilder` configuration
3. Create `tests/sandbox.rs` with the hostile memory growth test
4. Run `cargo test` to verify the trap occurs
5. Update `openspec/changes/phase0-sandbox-init/` with this exploration artifact
