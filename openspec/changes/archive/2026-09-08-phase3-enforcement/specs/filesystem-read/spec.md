# Delta: filesystem-read — CapabilityConfig Field Rename (Unit A)

## Purpose

Pure refactor: rename `CapabilityConfig.max_read_bytes` → `max_bytes` to generalize the field across all capability variants. No behavior change.

## MODIFIED Requirements

### Requirement: REQ-104 — Size Enforcement

System SHALL call `std::fs::metadata(path)` before reading. If `len > max_bytes`: trap. Default `max_bytes` = 1,048,576 (1 MB).
(Previously: field name was `max_read_bytes`)

#### Scenario: S-5 — Size exceeded

- GIVEN file within `allowed_root` with size > `max_bytes`
- WHEN module calls `aegis::fs_read`
- THEN trap raised with message indicating file size exceeds capability limit

#### Scenario: S-1 — Happy path (unchanged)

- GIVEN file within `allowed_root`, size ≤ `max_bytes`
- WHEN module calls `aegis::fs_read`
- THEN returns file contents, no trap

### Requirement: REQ-204 — CapabilityConfig from Enum

System SHALL update `CapabilityConfig::from(&Capability)` to `match` on enum variants. For `FilesystemRead`, populate `name = "filesystem.read"`, `allowed_root`, `max_bytes`. For `FilesystemWrite`, populate `name = "filesystem.write"`, `allowed_root`, `max_bytes` (from `max_write_bytes`).
(Previously: field was `max_read_bytes`)

#### Scenario: CapabilityConfig from FilesystemRead

- GIVEN `Capability::FilesystemRead(FilesystemReadParams { allowed_root: "/tmp", max_read_bytes: 512 })`
- WHEN `CapabilityConfig::from(&cap)` called
- THEN config has `name = "filesystem.read"`, `max_bytes = 512`

#### Scenario: CapabilityConfig from FilesystemWrite

- GIVEN `Capability::FilesystemWrite(FilesystemWriteParams { allowed_root: "/data", max_write_bytes: 2048 })`
- WHEN `CapabilityConfig::from(&cap)` called
- THEN config has `name = "filesystem.write"`, `max_bytes = 2048`

### Requirement: REQ-464 — Regression Gate (Unit A)

All 71 existing tests SHALL pass after the `max_read_bytes` → `max_bytes` rename. Zero test failures permitted.

#### Scenario: Full regression

- GIVEN codebase with `max_bytes` rename applied
- WHEN `cargo test` executed
- THEN all 71 tests pass, zero failures
