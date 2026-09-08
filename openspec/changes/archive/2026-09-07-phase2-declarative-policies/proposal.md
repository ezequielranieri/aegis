# Proposal: Phase 2 — Declarative Policies

## Intent

Phase 1 proved runtime-enforced filesystem read isolation via host functions. Phase 2 moves capability configuration from programmatic `Vec<Capability>` to declarative TOML config files. Enables operators to define sandbox policies without recompiling, supports multi-capability composition, and establishes fail-closed config loading for Phase 3.

## Scope

### In Scope
- Add `toml` crate; `src/config/` module (`PolicyConfig`, `CapabilityDef`, `ConfigError`)
- `PolicyConfig::load(path)` → TOML parse → validate → `Vec<Capability>`
- `Sandbox::from_config(path)` with fail-closed startup
- `filesystem.read` (required: `allowed_root`, `max_read_bytes`)
- Multi-capability via `[[capabilities]]` TOML syntax
- Config: `.aegis/config.toml` → `~/.config/aegis/config.toml` fallback
- Unit + integration tests

### Out of Scope
- `network.http`, `crypto.sign`, `crypto.verify` (Phase 3+)
- `PolicyEngine` runtime evaluation
- Config hot-reload, permission auditing, YAML/JSON

## Capabilities

### New Capabilities
- `declarative-config`: TOML policy loading with validation, fail-closed startup

### Modified Capabilities
- `filesystem-read`: Config source shifts to declarative TOML; enforcement unchanged

## Approach

**Two sequential work units with regression gate:**

### Unit A: Capability Refactor (typed enum) — *Regression Gate Required*
```rust
pub enum Capability {
    FilesystemRead(FilesystemReadParams),
    FilesystemWrite(FilesystemWriteParams),
    NetworkHttp(NetworkHttpParams),
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FilesystemReadParams { pub allowed_root: PathBuf, pub max_read_bytes: u64 }
```
- Update `CapabilityConfig::from(&Capability)` to match enum variants
- Update `Sandbox::instantiate_with_capabilities()` for enum
- **REGRESSION GATE**: All 18 Phase 1 tests pass before Unit B

### Unit B: TOML Parsing (built on verified enum)
- Add `toml` crate
- `src/config/` with `PolicyConfig` deserializing via `serde(tag = "name")` into `Vec<Capability>`
- `Sandbox::from_config(path)` with fail-closed validation
- Config parsing + sandbox integration tests

**Receipt Naming: Option A** — `fn capability_name(&self) -> &'static str` on enum. Zero duplication, compile-time exhaustive, `emit_capability_event` calls `capability.capability_name()`.

## Affected Areas

- `Cargo.toml`: add `toml` crate
- `src/capabilities/mod.rs`: `Capability` → typed enum, `capability_name()`
- `src/sandbox/mod.rs`: `CapabilityConfig::from`, `instantiate_with_capabilities`, `from_config`
- `src/config/mod.rs` (new): `PolicyConfig`, `CapabilityDef`, `ConfigError`, load/validate
- `src/lib.rs`: re-export new types
- `tests/config_parsing.rs` (new): unit tests for parsing/validation
- `tests/sandbox_config.rs` (new): integration tests for `from_config()`
- `tests/fixtures/config/` (new): valid, invalid, missing fields, unknown capability

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Typed enum breaks existing `Capability` construction | Medium | Regression gate: 18 Phase 1 tests pass before Unit B |
| `serde(tag = "name")` doesn't map to enum | Low | Prototype deserialization in isolation |
| Config path resolution differs | Low | Reuse existing `canonicalize` logic |
| `toml` crate transitive conflicts | Low | `serde` already direct dep; verify `cargo tree` |

## Rollback Plan

1. Revert `Cargo.toml` — remove `toml` crate
2. Revert `src/capabilities/mod.rs` — restore `struct Capability`
3. Revert `src/sandbox/mod.rs` — restore `CapabilityConfig::from`, remove `from_config`
4. Delete `src/config/` and test files
5. `cargo test` — all 18 Phase 1 tests pass

## Dependencies

- Phase 1 complete (18/18 tests)
- `toml` crate (new, lightweight)
- Existing: `serde`, `serde_json`, `thiserror`, `anyhow`

## Success Criteria

- [ ] Unit A: 18 Phase 1 tests pass after typed enum refactor (regression gate)
- [ ] Unit B: `Sandbox::from_config(".aegis/config.toml")` starts sandbox with `filesystem.read`
- [ ] Unit B: Invalid config → `ConfigError`, sandbox never starts
- [ ] Unit B: Multi-capability config parses and validates
- [ ] All new unit + integration tests pass
- [ ] `cargo test` passes end-to-end

## Ready for Spec

**Yes** — Exploration complete, two design decisions resolved, clear two-unit breakdown with regression gate.