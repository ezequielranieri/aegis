# Filesystem Read-Only Specification (Phase 1)

## Purpose

Custom host function `aegis::fs_read` enabling WASM modules to read files from a single fixed root directory. Runtime decides access — not WASI. Any violation (path escape, traversal, size exceed, fd leak) = trap, never graceful error.

## Delta from Phase 0

`sandbox-init` spec is unchanged. Phase 1 adds new `filesystem-read` domain. `Sandbox::instantiate()` gains `&[Capability]` parameter. No WASI. `wasi` crate removed from `Cargo.toml`.

## Requirements

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-101 | System SHALL provide `aegis::fs_read(path_ptr, path_len, out_ptr, out_len) -> i32` host function via `Linker::func_wrap` when `filesystem.read` capability is present | MUST |
| REQ-102 | System SHALL validate path against `allowed_root` (canonicalized, absolute) and TRAP on any path outside root | MUST |
| REQ-103 | System SHALL reject paths containing `..` segments — TRAP on traversal attempt | MUST |
| REQ-104 | System SHALL enforce `max_read_bytes` via `stat()` before read — TRAP if file size exceeds limit | MUST |
| REQ-105 | System SHALL NOT maintain an fd table — all reads go through `aegis::fs_read`; any raw fd usage = trap | MUST |
| REQ-106 | System SHALL accept `&[Capability]` in `Sandbox::instantiate()` and register host functions per capability | MUST |
| REQ-107 | All violation paths (REQ-102–105) SHALL trap with descriptive message, never return error values | MUST |
| REQ-108 | System SHALL emit receipt stub (capability name, path, size, result) per `fs_read` call | SHOULD |

### REQ-101: Host Function Signature

The host function SHALL be registered via `Linker::func_wrap::<SandboxState, (i32, i32, i32, i32), i32>`:

| Param | Type | Purpose |
|-------|------|---------|
| `path_ptr` | i32 | Guest memory pointer to path string |
| `path_len` | i32 | Length of path string |
| `out_ptr` | i32 | Guest memory pointer for output buffer |
| `out_len` | i32 | Capacity of output buffer |
| Return | i32 | 0 = success, trap on error |

### REQ-102: Path Validation

System SHALL canonicalize both the guest-provided path and `allowed_root`, then verify the resolved path starts with the resolved `allowed_root`.

### REQ-103: Traversal Rejection

System SHALL reject any path containing `..` segments BEFORE canonicalization. Canonicalize must not resolve `..` out of the root.

### REQ-104: Size Enforcement

System SHALL call `std::fs::metadata(path)` before reading. If `len > max_read_bytes`: trap. Default `max_read_bytes` = 1,048,576 (1 MB).

### REQ-105: No FD Table

System SHALL NOT expose file descriptors to WASM modules. The host function reads files internally. There is no fd namespace, no fd passing, no fd inheritance. Attempting to use a raw fd number traps.

### REQ-106: Capability-Driven Registration

`Sandbox::instantiate(wasm_bytes, capabilities)` SHALL iterate capabilities. For each `filesystem.read`, register `aegis::fs_read` with `allowed_root` and `max_read_bytes` captured in the closure.

### REQ-107: Fail-Closed Violations

Every access control and resource violation MUST trap with `Trap::new(message)`. The system SHALL NOT return error codes, errno values, or graceful failure indicators.

### REQ-108: Receipt Stub

System SHALL log `capability: "filesystem.read"`, `path: String`, `size: u64`, `result: Success|Trap|SizeExceeded`. Phase 3 replaces with signed receipts.

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-1 | Allowed read | File within `allowed_root`, size ≤ `max_read_bytes` | Module calls `aegis::fs_read` | Returns file contents, no trap |
| S-2 | Denied read | Path resolves outside `allowed_root` | Module calls `aegis::fs_read` | Trap raised |
| S-3 | Path traversal | Guest path contains `../../etc/passwd` | Module calls `aegis::fs_read` | Trap raised (before canonicalize) |
| S-4 | FD leak | Module attempts raw fd read (no fd table exists) | Module calls raw fd op | Trap raised |
| S-5 | Size exceeded | File size > `max_read_bytes` | Module calls `aegis::fs_read` | Trap raised |

## Public API

| Type | Signature | Purpose |
|------|-----------|---------|
| `Sandbox` | `instantiate(wasm_bytes: &[u8], capabilities: &[Capability]) -> Result<Instance>` | Register host functions per capability, compile + instantiate |
| `Capability` | `{ name: String, params: HashMap<String, Value> }` | Existing struct — Phase 1 uses `name="filesystem.read"`, `params["allowed_root"]`, `params["max_read_bytes"]` |

## Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Security | Fail-closed: all violations trap, never graceful |
| TOCTOU | Race between `stat()` and `read()` documented as known limitation — single-threaded Phase 1 scope; Phase 2/3 addresses with `openat` + fd passing |
| Performance | `stat()` before read adds one syscall overhead — acceptable for Phase 1 |
| Determinism | Same path + same limits = same result, every run |
| Cleanup | No leaked fds or threads after `fs_read` returns |

## Traceability

| Requirement | Proposal Acceptance Criteria |
|-------------|------------------------------|
| REQ-101, REQ-106 | AC-1: Allowed read succeeds |
| REQ-102, REQ-107 | AC-2: Denied read traps |
| REQ-103, REQ-107 | AC-3: Path traversal traps |
| REQ-105, REQ-107 | AC-4: FD leak traps |
| REQ-104, REQ-107 | AC-5: Size exceeded traps |
| REQ-108 | Receipt stub logs per call |
