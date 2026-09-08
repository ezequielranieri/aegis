# Sandbox Init Specification

## Purpose

Minimal Wasmtime sandbox proving CPU/memory isolation via epoch interruption and StoreLimitsBuilder. Any resource exhaustion MUST produce a deterministic trap — never a graceful error.

## Requirements

### REQ-001: Engine Configuration

The system SHALL create a Wasmtime `Engine` with `epoch_interruption(true)` enabled. Fuel metering SHALL NOT be enabled.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Epoch enabled | Engine is created with config | Engine is constructed | `epoch_interruption` is true on the config |
| No fuel | Engine is created | Any module runs | No fuel consumption occurs |

### REQ-002: Store Limits

The system SHALL enforce four hard resource limits via `StoreLimitsBuilder`:

| Limit | Value | Purpose |
|-------|-------|---------|
| `memory_size` | 1 MB (1 << 20) | Prevents OOM attacks |
| `table_elements` | 1024 | Bounds function pointer tables |
| `instances` | 4 | Prevents fork-bomb instantiation |
| `memories` | 2 | Prevents memory fragmentation abuse |

| Scenario | Given | When | Then |
|----------|-------|------|------|
| All limits enforced | Store created with limits | Module exceeds any single limit | Execution traps |
| Within limits | Store created with limits | Module stays within bounds | Execution succeeds |

### REQ-003: Trap on Grow Failure

The system SHALL set `trap_on_grow_failure(true)` on the `StoreLimitsBuilder`. When `memory.grow` exceeds the limit, the store MUST trap deterministically rather than returning -1.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Memory growth beyond limit | Store with 1MB memory limit | Module calls `memory.grow` past 1MB | Trap is raised (not -1 return) |
| Memory growth within limit | Store with 1MB limit | Module calls `memory.grow` within budget | Growth succeeds |

### REQ-004: Epoch Timer Thread

The system SHALL spawn a single `std::thread` that calls `engine.increment_epoch()` every 100ms. The thread SHALL use `Arc<AtomicBool>` as a stop flag. The `Drop` implementation SHALL set the stop flag and `JoinHandle::join()` to ensure graceful shutdown.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Timer ticks | Epoch timer is running | 100ms elapses | `increment_epoch()` is called |
| Graceful shutdown | Timer is running | `EpochInterrupter` is dropped | Stop flag set, thread joined |
| No resource leak | Epoch interrupter goes out of scope | Drop runs | Thread terminates, no leaked handle |

### REQ-005: Hostile Module Integration Test — Memory Limit

An integration test SHALL compile a hostile `.wat` module that attempts `memory.grow (i32.const 100)`, instantiate it in a sandbox with the configured limits, call its exported `_start` function, and assert that a trap occurs.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Hostile memory growth | Sandbox with 1MB limit | Hostile .wat calls `memory.grow 100` | Trap occurs (not graceful return) |
| Test compiles WAT | `wat = "1.0"` dev-dependency | Test calls `wat::parse_str` | WAT parses to valid WASM bytes |

### REQ-006: Hostile Module Integration Test — Epoch Timeout

An integration test SHALL compile a hostile `.wat` module containing an infinite loop (`loop { br 0 }`), instantiate it in a sandbox with a short test epoch interval (e.g., 10ms), call its exported `_start` function, and assert that a trap occurs due to epoch deadline exceeded — not due to test runner timeout.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Infinite loop trapped by epoch | Sandbox with 10ms epoch interval | Hostile .wat runs infinite loop | Trap occurs (epoch deadline exceeded) |
| Trap is epoch, not timeout | Test runs hostile module | Trap is raised | Trap reason is epoch exhaustion, not test timeout |

### REQ-007: Fail-Closed Property

The system SHALL be fail-closed: any resource exhaustion (memory, table, instance, or epoch timeout) MUST result in a Wasmtime trap. The system SHALL NOT return graceful error values for exhausted resources.

| Scenario | Given | When | Then |
|----------|-------|------|------|
| Resource exhaustion trap | Module hits any limit | Execution continues | Trap raised, not -1 or error enum |
| Epoch timeout trap | Module runs beyond epoch budget | Epoch counter increments | Trap raised on next epoch check |

## Public API

| Type | Signature | Purpose |
|------|-----------|---------|
| `Sandbox` | `Sandbox::new_with_limits(config: SandboxConfig) -> Result<Sandbox>` | Create sandbox with resource limits and epoch timer |
| `SandboxConfig` | Struct with `memory_size`, `table_elements`, `instances`, `memories`, `epoch_interval_ms` fields | Configuration for sandbox limits |
| `EpochInterrupter` | Holds `Arc<AtomicBool>` stop flag + `JoinHandle<()>` | Manages epoch timer lifecycle |

## Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Security | Any resource exhaustion MUST trap, never return a value the caller can ignore |
| Determinism | Same module + same limits = same trap behavior, every run |
| Performance | Epoch timer tick overhead SHALL be negligible (no perceptible work added per tick; `increment_epoch()` is a single atomic increment) |
| Cleanup | No leaked threads or handles after sandbox drop |

## Traceability

| Requirement | Proposal Success Criteria |
|-------------|--------------------------|
| REQ-001 | Epoch interruption fires (sub-second timeout) |
| REQ-002 | All 4 StoreLimitsBuilder limits enforced |
| REQ-003 | Hostile .wat triggers trap (not graceful error) |
| REQ-004 | Epoch timer thread with graceful shutdown |
| REQ-005 | `cargo test` passes with memory-limit integration test |
| REQ-006 | `cargo test` passes with epoch-timeout integration test |
| REQ-007 | Fail-closed: resource exhaustion = trap (cross-cutting) |
