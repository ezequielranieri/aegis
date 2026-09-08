# Tasks: Phase 2 — Declarative Policies

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~600-800 |
| 400-line budget risk | High |
| Chained PRs recommended | Yes |
| Suggested split | PR 1 (Unit A) → PR 2 (Unit B) |
| Delivery strategy | ask-on-risk |
| Chain strategy | stacked-to-main |

Decision needed before apply: Yes
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: High

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Typed enum Capability refactor | PR 1 | `cargo test` | 18 Phase 1 tests in `tests/sandbox.rs` | Revert `src/capabilities/mod.rs`, `src/sandbox/mod.rs` to struct Capability |
| 2 | TOML parsing + `Sandbox::from_config` | PR 2 | `cargo test` | New `tests/config_parsing.rs`, `tests/sandbox_config.rs` | Delete `src/config/`, remove `toml` dep, revert lib.rs |

---

## Unit A: Capability Refactor (REQ-201..REQ-206)

- [x] **A.1** Replace `struct Capability` with `enum Capability { FilesystemRead(FilesystemReadParams), FilesystemWrite(FilesystemWriteParams), NetworkHttp(NetworkHttpParams) }` in `src/capabilities/mod.rs` — add `FilesystemReadParams`, `FilesystemWriteParams`, `NetworkHttpParams` structs; derive `Debug, Clone, Serialize, Deserialize`; add `#[serde(rename = "...")]` attrs (REQ-201, REQ-202)
- [x] **A.2** Add `capability_name(&self) -> &'static str` method on `Capability` enum returning variant name strings like `"filesystem.read"` (REQ-203, SA-201)
- [x] **A.3** Update `CapabilityConfig::from(&Capability)` in `src/sandbox/mod.rs` to `match` on enum variants instead of parsing `HashMap<String, Value>` — each arm constructs `CapabilityConfig` with correct fields (REQ-204, SA-202)
- [x] **A.4** Update `Sandbox::instantiate_with_capabilities(&[Capability])` to iterate enum variants and register host functions per variant using `capability_name()` (REQ-205, SA-203)
- [x] **A.5** Update all internal call sites that construct `Capability { name, params }` to use `Capability::FilesystemRead(FilesystemReadParams { ... })` — update `src/sandbox/mod.rs` and any other files referencing the old struct (internal)
- [x] **A.6** Update `fs_read_capability()` helper in `tests/sandbox.rs` to construct `Capability::FilesystemRead(FilesystemReadParams { allowed_root: PathBuf::from(root), max_read_bytes })` instead of `HashMap`-based struct (internal)
- [x] **A.7** [REGRESSION GATE] Run `cargo test` — all 18 Phase 1 tests must pass (REQ-206, SA-203) — BLOCKS: All Unit B tasks — DONE: 18/18 tests pass, zero failures

## Unit B: TOML Parsing (REQ-301..REQ-310)

- [x] **B.1** Add `toml = "0.8"` to `[dependencies]` in `Cargo.toml` (REQ-301)
- [x] **B.2** Create `src/config/mod.rs` with `PolicyConfig` struct, `CapabilityDef` enum (with `#[serde(tag = "name")]`), and `ConfigError` enum with variants `NotFound`, `ParseError`, `MissingField`, `UnknownCapability`, `DuplicateCapability`, `InvalidPath`, `Io` (REQ-302, REQ-308)
- [x] **B.3** Implement `PolicyConfig::load(path: &str) -> Result<Self, ConfigError>` with fallback path resolution: try `.aegis/config.toml` in project root, then `~/.config/aegis/config.toml`; return `ConfigError::NotFound` if neither exists — fail-closed, no empty/default capabilities (REQ-303, SB-308)
- [x] **B.4** Implement validation pass in `PolicyConfig::validate()`: required fields per capability type (`filesystem.read` requires `allowed_root` absolute path and `max_read_bytes > 0`), duplicate detection in `Vec<CapabilityDef>`, invalid path canonicalization — return `ConfigError::MissingField`, `DuplicateCapability`, `InvalidPath` respectively (REQ-305, REQ-309, REQ-310, SB-305, SB-309, SB-310)
- [x] **B.5** Implement `CapabilityDef::try_into() -> Result<Capability, ConfigError>` converting `CapabilityDef` enum to `Capability` enum; unknown capability names via serde tag mismatch produce `ConfigError::UnknownCapability` (REQ-306, SB-306)
- [x] **B.6** Add `Sandbox::from_config(path: &str) -> Result<Self>` constructor: load `PolicyConfig`, convert to `Vec<Capability>`, call `CapabilityConfig::from`, store in `SandboxState`, return sandbox — fail-closed if any validation error (REQ-304, SB-301)
- [x] **B.7** Create test fixtures in `tests/fixtures/config/`: `valid.toml` (single capability), `multi_cap.toml` (filesystem.read + network.http), `missing_field.toml` (no `allowed_root`), `unknown_cap.toml` (invalid name), `duplicate_cap.toml` (two same-name entries), `invalid_path.toml` (relative path) (SB-301..310)
- [x] **B.8** Create `tests/config_parsing.rs` unit tests: `PolicyConfig::load` parses valid TOML, `serde(tag="name")` rejects unknown capability → `UnknownCapability`, missing required field → `MissingField`, `max_read_bytes = 0` → validation error, relative `allowed_root` → `InvalidPath`, duplicate names → `DuplicateCapability`, nonexistent path → `NotFound`, fallback path resolution (SB-301..310)
- [x] **B.9** Create `tests/sandbox_config.rs` integration tests: `Sandbox::from_config` with valid config creates sandbox, invalid config returns `ConfigError` and sandbox never starts, multi-capability config parses both capabilities, `filesystem.read` + `network.http` both parsed (SB-301, SB-302, SB-303, SB-304)
- [x] **B.10** Update `src/lib.rs` re-exports to include `pub mod config` and `pub use config::{PolicyConfig, CapabilityDef, ConfigError}` (B.11)
- [x] **B.11** Run `cargo test` — all new + existing tests pass
