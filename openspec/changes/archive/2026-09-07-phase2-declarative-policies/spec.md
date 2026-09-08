# Phase 2 — Declarative Policies Specification

## Purpose

Replace programmatic `Capability` construction with declarative TOML config files. Operators define sandbox policies without recompiling. Two work units: typed enum refactor (Unit A) then TOML parsing (Unit B), with a regression gate between them.

## Requirements

### Unit A: Capability Refactor (REQ-2xx)

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-201 | System SHALL define `Capability` as a Rust enum with variants `FilesystemRead(FilesystemReadParams)`, `FilesystemWrite(FilesystemWriteParams)`, `NetworkHttp(NetworkHttpParams)` | MUST |
| REQ-202 | System SHALL define `FilesystemReadParams` struct with fields `allowed_root: PathBuf` and `max_read_bytes: u64` | MUST |
| REQ-203 | System SHALL provide `capability_name(&self) -> &'static str` method on `Capability` returning the variant name (e.g., `"filesystem.read"`) | MUST |
| REQ-204 | System SHALL update `CapabilityConfig::from(&Capability)` to match on enum variants instead of parsing `HashMap<String, Value>` | MUST |
| REQ-205 | System SHALL update `Sandbox::instantiate_with_capabilities()` to accept `&[Capability]` (enum) and register host functions per variant | MUST |
| REQ-206 | **REGRESSION GATE**: All 18 Phase 1 tests MUST pass after the typed enum refactor — zero test failures permitted | MUST |

### Unit B: TOML Parsing (REQ-3xx)

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-301 | System SHALL add `toml` crate to `Cargo.toml` as a direct dependency | MUST |
| REQ-302 | System SHALL define `PolicyConfig` struct deserializing `[[capabilities]]` TOML array into `Vec<Capability>` via `serde(tag = "name")` | MUST |
| REQ-303 | System SHALL resolve config path: try `.aegis/config.toml` in project root, then `~/.config/aegis/config.toml` as fallback; if neither exists, return `ConfigError::NotFound` — **fail-closed, no empty/default capabilities** | MUST |
| REQ-304 | System SHALL provide `Sandbox::from_config(path: &str) -> Result<Self>` constructor with fail-closed validation | MUST |
| REQ-305 | System SHALL validate required fields per capability type: `filesystem.read` requires `allowed_root` (absolute path) and `max_read_bytes` (> 0) | MUST |
| REQ-306 | System SHALL reject unknown capability names — `Sandbox` startup fails (fail-closed) | MUST |
| REQ-307 | System SHALL support multi-capability config via `[[capabilities]]` TOML table array syntax | MUST |
| REQ-308 | System SHALL define `ConfigError` enum with variants: `NotFound`, `ParseError`, `MissingField`, `UnknownCapability`, `DuplicateCapability`, `InvalidPath`, `Io` | MUST |
| REQ-309 | System SHALL reject duplicate capability names in the same config — if two `[[capabilities]]` entries declare the same `name`, return `ConfigError::DuplicateCapability`, sandbox never starts | MUST |
| REQ-310 | System SHALL reject invalid `allowed_root` paths — not absolute, or canonicalization fails — return `ConfigError::InvalidPath`, sandbox never starts | MUST |

## Scenarios

### Unit A Scenarios (SA-xxx)

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| SA-201 | Enum construction | Code constructs `Capability::FilesystemRead(params)` | Variant is created | `capability_name()` returns `"filesystem.read"` |
| SA-202 | CapabilityConfig from enum | `Capability::FilesystemRead(FilesystemReadParams { allowed_root: "/data", max_read_bytes: 1024 })` | `CapabilityConfig::from(&cap)` called | Config has correct `name`, `allowed_root`, `max_read_bytes` |
| SA-203 | Sandbox instantiation with enum | `instantiate_with_capabilities(wasm, &[cap])` | Sandbox created | Host function `aegis::fs_read` registered, 18 Phase 1 tests pass |

### Unit B Scenarios (SB-xxx)

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| SB-301 | Valid single capability | `.aegis/config.toml` with one `[[capabilities]]` entry | `Sandbox::from_config(path)` called | Sandbox created, capability loaded |
| SB-302 | Valid multi-capability | Config with `filesystem.read` + `network.http` | `Sandbox::from_config(path)` called | Both capabilities parsed and validated |
| SB-303 | Config not found | Path to nonexistent file | `Sandbox::from_config(path)` called | `ConfigError::NotFound`, sandbox never starts |
| SB-304 | Invalid TOML syntax | File with unclosed bracket | `Sandbox::from_config(path)` called | `ConfigError::ParseError`, sandbox never starts |
| SB-305 | Missing required field | `filesystem.read` without `allowed_root` | `Sandbox::from_config(path)` called | `ConfigError::MissingField`, sandbox never starts |
| SB-306 | Unknown capability | Capability name not in builtin constants | `Sandbox::from_config(path)` called | `ConfigError::UnknownCapability`, sandbox never starts |
| SB-307 | Zero max_read_bytes | `max_read_bytes = 0` | `Sandbox::from_config(path)` called | `ConfigError` or validation error, sandbox never starts |
| SB-308 | Fallback path resolution | No `.aegis/config.toml`, `~/.config/aegis/config.toml` exists | `Sandbox::from_config(".aegis/config.toml")` called | Loads from `~/.config/aegis/config.toml` |
| SB-309 | Duplicate capability | Config with two `[[capabilities]]` entries both `name = "filesystem.read"` | `Sandbox::from_config(path)` called | `ConfigError::DuplicateCapability`, sandbox never starts |
| SB-310 | Invalid allowed_root path | Config with `allowed_root = "relative/path"` or non-canonicalizable path | `Sandbox::from_config(path)` called | `ConfigError::InvalidPath`, sandbox never starts |
