# Filesystem Write Specification (Phase 3)

## Purpose

Custom host function `aegis::fs_write` enabling WASM modules to write files within a single fixed root directory. Atomic write via temp file + rename. Runtime decides access — not WASI. Any violation (path escape, traversal, size exceed, signing failure) = trap, never graceful error.

## Requirements

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-501 | System SHALL provide `aegis::fs_write(path_ptr, path_len, data_ptr, data_len) -> i32` host function via `Linker::func_wrap` when `filesystem.write` capability is present | MUST |
| REQ-502 | System SHALL validate path against `allowed_root` (canonicalized, absolute) and TRAP on any path outside root | MUST |
| REQ-503 | System SHALL reject paths containing `..` segments — TRAP on traversal attempt | MUST |
| REQ-504 | System SHALL enforce `max_write_bytes` (via `max_bytes` in `CapabilityConfig`) — TRAP if `data_len > max_bytes` | MUST |
| REQ-505 | System SHALL perform atomic write: validate destination path completely (canonicalize, root check, symlink escape), THEN write data to temp file (`.aegis_tmp` suffix in `allowed_root`), then `std::fs::rename` to target path | MUST |
| REQ-506 | System SHALL allow overwrite of existing files (matching `std::fs::write` semantics) | MUST |
| REQ-507 | System SHALL accept `&[Capability]` typed enum in `instantiate_with_capabilities()` and register `aegis::fs_write` for `FilesystemWrite` variant | MUST |
| REQ-508 | All violation paths (REQ-502–504) SHALL trap with descriptive message, never return error values | MUST |
| REQ-509 | System SHALL emit signed receipt at all 5 validation points (S-1-W through S-416-W) per Decision 4 | MUST |
| REQ-510 | If receipt emission fails, `aegis_fs_write` SHALL immediately return Trap — no data written to target path (fail-closed, REQ-433) | MUST |
| REQ-511 | Temp file cleanup: on any failure after temp file creation (rename failure, signing failure, emit failure, etc.), system SHALL remove temp file before trapping | MUST |
| REQ-512 | System SHALL validate destination path completely (canonicalize, root check, symlink escape, `..` rejection) BEFORE creating temp file — no filesystem writes occur until all validations pass | MUST |

### REQ-501: Host Function Signature

The host function SHALL be registered via `Linker::func_wrap::<SandboxState, (i32, i32, i32, i32), i32>`:

| Param | Type | Purpose |
|-------|------|---------|
| `path_ptr` | i32 | Guest memory pointer to path string |
| `path_len` | i32 | Length of path string |
| `data_ptr` | i32 | Guest memory pointer to data buffer |
| `data_len` | i32 | Length of data to write |
| Return | i32 | 0 = success, trap on error |

### REQ-502: Path Validation

System SHALL canonicalize both the guest-provided path and `allowed_root`, then verify the resolved path starts with the resolved `allowed_root`.

### REQ-503: Traversal Rejection

System SHALL reject any path containing `..` segments BEFORE canonicalization. Canonicalize must not resolve `..` out of the root.

### REQ-504: Size Enforcement

System SHALL reject writes where `data_len > max_write_bytes`. Default `max_write_bytes` = 1,048,576 (1 MB).

### REQ-505: Atomic Write

System SHALL write data to a temporary file with `.aegis_tmp` suffix within `allowed_root`, then atomically rename to the target path via `std::fs::rename`. This ensures target path is never in a partially-written state.

### REQ-506: Overwrite Allowed

System SHALL allow overwriting existing files. No pre-existence check required — matches `std::fs::write` semantics.

### REQ-511: Temp File Cleanup

On any failure after temp file creation (rename failure, signing failure, etc.), system SHALL remove the `.aegis_tmp` file before trapping.

## Config Schema

### FilesystemWriteParams

```toml
[[capabilities]]
name = "filesystem.write"
allowed_root = "/path/to/write/root"
max_write_bytes = 1048576
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `allowed_root` | String | Yes | Absolute path boundary for write operations |
| `max_write_bytes` | u64 | Yes | Maximum bytes per write call (must be > 0) |

## API

| Type | Signature | Purpose |
|------|-----------|---------|
| `Sandbox` | `instantiate_with_capabilities(wasm, &[Capability])` | Register `aegis::fs_write` for `FilesystemWrite` variant |
| Host fn | `aegis::fs_write(path_ptr, path_len, data_ptr, data_len) -> i32` | Atomic file write with capability validation |

## Receipt Emission (Decision 4)

6 emission points in `aegis_fs_write`:

| Point | Scenario | Action | Result | Trigger |
|-------|----------|--------|--------|---------|
| 1 | S-1-W happy | `write` | `success` | Data written successfully |
| 2 | S-2-W outside root | `write` | `trap` | Path resolves outside `allowed_root` |
| 3 | S-3-W traversal | `write` | `trap` | `..` segment detected |
| 4 | S-5-W size | `write` | `trap` | Data exceeds `max_write_bytes` |
| 5 | S-416-W signing | `write` | `trap` | Receipt emission fails (before temp file or after) |
| 6 | S-416-W-after-write | `write` | `trap` | Temp file written, then emit fails — temp file cleaned up |

**Fail-closed rule**: If `emit()` returns `Err` at any point, the host function MUST immediately return Trap. No data is written to the target path. Temp file is cleaned up if it was created.

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-1-W | Happy write | File within `allowed_root`, data ≤ `max_write_bytes` | Module calls `aegis::fs_write` | File written atomically, no trap |
| S-2-W | Outside root | Path resolves outside `allowed_root` | Module calls `aegis::fs_write` | Trap raised |
| S-3-W | Path traversal | Guest path contains `../../etc/passwd` | Module calls `aegis::fs_write` | Trap raised (before canonicalize) |
| S-5-W | Size exceeded | Data length > `max_write_bytes` | Module calls `aegis::fs_write` | Trap raised |
| S-416-W | Signing failure | Force signing error on write call | `aegis_fs_write` called | Trap raised, no data written to target |
| S-416-W-after-write | Signing failure after temp write | Temp file created, then force signing error | `aegis_fs_write` called | Trap raised, NO target file written, NO `.aegis_tmp` file left behind |
| T-6-W | Guest memory OOB | `data_ptr` beyond memory bounds | Module calls `aegis::fs_write` | Trap raised |
| Symlink-W | Symlink escape | Symlink inside root points outside | Module calls `aegis::fs_write` via symlink path | Trap raised (canonical path outside root) |

### Scenario Details

#### S-1-W: Happy Write

- GIVEN temp directory as `allowed_root`, data ≤ limit
- WHEN module writes "hello world" to "output.txt"
- THEN file exists at `allowed_root/output.txt` with correct contents, receipt emitted with `result = "success"`

#### S-2-W: Outside Root

- GIVEN temp directory as `allowed_root`
- WHEN module writes to "../secret.txt"
- THEN trap raised, receipt emitted with `result = "trap"`

#### S-3-W: Path Traversal

- GIVEN temp directory as `allowed_root`
- WHEN module writes to "../../etc/passwd"
- THEN trap raised (before canonicalize), receipt emitted with `result = "trap"`

#### S-5-W: Size Exceeded

- GIVEN `max_write_bytes = 1024`
- WHEN module writes 2048 bytes
- THEN trap raised, receipt emitted with `result = "trap"`

#### S-416-W: Signing Failure

- GIVEN receipt emitter with forced signing failure
- WHEN module calls `aegis_fs_write` for happy path
- THEN trap raised, NO file written to target path, NO additional receipt added

#### S-416-W-after-write: Signing Failure After Temp File Written

- GIVEN receipt emitter with forced signing failure AFTER temp file creation
- WHEN module calls `aegis_fs_write` for happy path
- THEN trap raised, NO target file written to `allowed_root`, NO `.aegis_tmp` file left behind (cleaned up before trap)

#### T-6-W: Guest Memory OOB

- GIVEN `data_ptr` set to address beyond memory bounds
- WHEN module calls `aegis_fs_write`
- THEN trap raised, receipt emitted with `result = "trap"`, path = ""

#### Symlink-W: Symlink Escape

- GIVEN symlink inside `allowed_root` pointing to file outside root
- WHEN module writes to symlink path
- THEN trap raised (canonical path outside `allowed_root`), receipt emitted with `result = "trap"`

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-501, REQ-507 | S-1-W |
| REQ-502, REQ-508, REQ-509, REQ-510 | S-2-W |
| REQ-503, REQ-508, REQ-509, REQ-510 | S-3-W |
| REQ-504, REQ-508, REQ-509, REQ-510 | S-5-W |
| REQ-505, REQ-506, REQ-511 | S-1-W |
| REQ-509, REQ-510 | S-416-W |
| REQ-510, REQ-511 | S-416-W-after-write |
| REQ-512 | S-1-W, S-2-W, S-3-W, S-5-W, Symlink-W (validation order) |
| REQ-502, REQ-509 | Symlink-W |
| — | T-6-W (guest memory bounds, from REQ-433) |

## Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| Atomicity | Target file never in partially-written state (temp + rename) |
| Security | Fail-closed: all violations trap, never graceful |
| Cleanup | Temp file removed on any failure path |
| Determinism | Same path + same data = same result, every run |
| Overwrite | Existing files overwritten without error |
