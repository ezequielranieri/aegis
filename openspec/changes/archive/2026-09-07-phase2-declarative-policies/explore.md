# Exploration: Phase 2 — Declarative Policies

## Current State

The aegis project (Rust + Wasmtime 24.0, edition 2021) is at v0.1.0. Phase 1 (`phase1-filesystem-read-only`) is **complete and archived** with 13/13 tests passing. The codebase has the following key structures relevant to Phase 2:

### `Capability` struct (`src/capabilities/mod.rs`)
```rust
pub struct Capability {
    pub name: String,
    pub params: HashMap<String, serde_json::Value>,
}
```
Already derives `Serialize` + `Deserialize`. The `params` field carries capability-specific configuration (e.g., `allowed_root`, `max_read_bytes` for `filesystem.read`). Built-in constants defined in `capabilities::builtin` module: `FILESYSTEM_READ`, `FILESYSTEM_WRITE`, `NETWORK_HTTP`, `CRYPTO_SIGN`, `CRYPTO_VERIFY`.

### `CapabilityConfig` struct (`src/sandbox/mod.rs`)
```rust
pub struct CapabilityConfig {
    pub name: String,
    pub allowed_root: PathBuf,
    pub max_read_bytes: u64,
}
```
Created via `From<&Capability>` which extracts `allowed_root` and `max_read_bytes` from `cap.params`, with defaults (`/data` for root, 1MB for max_read_bytes). This is the bridge between the generic `Capability` and the sandbox enforcement layer.

### `SandboxState` (`src/sandbox/mod.rs`)
```rust
pub struct SandboxState {
    limits: StoreLimits,
    capabilities: Vec<CapabilityConfig>,
}
```
Stores `Vec<CapabilityConfig>` in the Wasmtime Store for host function closures to access. Populated by `Sandbox::instantiate_with_capabilities()`.

### `Sandbox` (`src/sandbox/mod.rs`)
- `Sandbox::new_with_limits(config)` → creates engine with epoch interruption + `StoreLimitsBuilder`
- `Sandbox::instantiate_with_capabilities(wasm_bytes, capabilities)` → converts `&[Capability]` to `Vec<CapabilityConfig>`, stores in `SandboxState`, registers `aegis::fs_read` host function per `filesystem.read` capability via `Linker::func_wrap`
- `Sandbox::store_mut()` → returns `&mut Store<SandboxState>`

### `PolicyEngine` (`src/policy/mod.rs`)
```rust
pub struct PolicyEngine {
    policy: Policy,  // { version, rules: Vec<PolicyRule>, default_effect: Deny }
}
```
Already implements `evaluate(capability, context) -> PolicyEffect`. **Not yet integrated with `Sandbox`**. This is the natural bridge for Phase 2 declarative policy evaluation.

### `Cargo.toml` Dependencies
```toml
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
thiserror = "1.0"
```
**NO `toml` or `serde_yaml` crate present.** A new parsing dependency is required.

### Test Infrastructure (`tests/sandbox.rs`)
- `wat::parse_str` for WAT→WASM compilation (dev-dep `wat = "1.0"`)
- `Sandbox::new_with_config(SandboxConfig::default(), false)` for test isolation
- `tempfile::tempdir()` for temp directories
- `fs_read_capability(root, max_bytes)` helper creates `Capability` instances
- Mature test patterns: 8+ integration tests covering all 5 scenarios + emit_capability_event verification

---

## Affected Areas

- **`src/sandbox/mod.rs`** — `SandboxState`, `CapabilityConfig`, `Sandbox` need config loading integration. New `ConfigError` type and `Sandbox::from_config()` constructor.
- **`src/capabilities/mod.rs`** — `Capability` struct shape unchanged, but needs a `TryFrom<Config>` or similar path.
- **`src/policy/mod.rs`** — `PolicyEngine` may need to be wired to config loading for policy evaluation.
- **`Cargo.toml`** — New dependency for config format parsing (TOML or YAML).
- **`src/lib.rs`** — May need to expose new public types (`PolicyConfig`, `ConfigError`).
- **`tests/sandbox.rs`** — New tests for config parsing, validation, multi-capability, fail-closed scenarios.
- **New file** — `src/config/` module (or `src/policy/config.rs`) for config file parsing and validation.

---

## Approaches

### Approach 1: TOML Config File (RECOMMENDED)

**Format**: TOML, aligned with Rust ecosystem conventions (Cargo.toml, rustfmt, etc.)

```toml
[[capabilities]]
name = "filesystem.read"
allowed_root = "/data"
max_read_bytes = 1048576

[[capabilities]]
name = "network.http"
allowed_hosts = ["example.com"]
max_requests_per_second = 100
```

**Pros:**
- Rust ecosystem standard — `toml` crate is idiomatic, first-party support
- Simple flat structure maps 1:1 to `Capability` fields
- `serde` already a dependency; `toml` crate is lightweight and well-maintained
- Human-readable for the simple shape required
- `toml::from_str` + `serde::Deserialize` gives zero-boilerplate parsing

**Cons:**
- TOML is less expressive than YAML for deeply nested structures (not an issue for this flat shape)
- Adding `toml` as a new dependency

**Effort:** Low

### Approach 2: YAML Config File

**Format**: YAML, more human-friendly for complex configs

```yaml
capabilities:
  - name: filesystem.read
    allowed_root: /data
    max_read_bytes: 1048576
  - name: network.http
    allowed_hosts:
      - example.com
    max_requests_per_second: 100
```

**Pros:**
- More human-readable for complex nested structures
- `serde_yaml` crate is mature
- Better for future multi-capability configs with varying fields

**Cons:**
- YAML parsing is heavier and has more edge cases (implicit types, anchors)
- `serde_yaml` adds a new dependency with more transitive deps
- YAML's flexibility can lead to subtle parsing surprises (e.g., `on`/`off`/`yes`/`no` as booleans)
- Not the Rust ecosystem convention

**Effort:** Low-Medium

### Approach 3: JSON Config File

**Format**: JSON, same format as `serde_json` already used

```json
{ "capabilities": [{ "name": "filesystem.read", "allowed_root": "/data", "max_read_bytes": 1048576 }] }
```

**Pros:**
- `serde_json` already a dependency — zero new deps
- Machine-generated configs are common
- No parsing surprises

**Cons:**
- Verbose for human-edited configs
- No comments (critical for security configs)
- Not the Rust ecosystem convention for configuration files
- Lacks TOML/YAML's readability advantages

**Effort:** Lowest (but worst UX)

### Comparison Table

| Approach | Pros | Cons | Complexity | Rust Convention |
|----------|------|------|------------|-----------------|
| **TOML** | Ecosystem standard, simple, well-supported | Less expressive for deeply nested | Low | ✅ Strong |
| **YAML** | Human-friendly, flexible | Heavier, parsing edge cases, not Rust-conventional | Low-Medium | ❌ Not conventional |
| **JSON** | Zero new deps, machine-friendly | No comments, verbose, poor UX | Lowest | ❌ Not conventional |

---

## Config File Location

Three candidate locations:

1. **Project root** (`/home/ez/Projects/aegis/Aegis.toml`) — Simple, discoverable, but mixes config with project root
2. **`.aegis/config.toml`** — Conventional hidden directory, keeps project root clean, follows `.gitignore`-style patterns
3. **`~/.config/aegis/config.toml`** — Global defaults, but doesn't work well for per-project sandbox policies

**Recommendation**: `.aegis/config.toml` in the project root. This follows the convention of hidden config directories (`.git`, `.vscode`, `.github`) and allows per-project sandbox policies. Falls back to `~/.config/aegis/config.toml` for global defaults if project config doesn't exist.

---

## Validation Strategy

### Required Fields per Capability Type

`filesystem.read` **MUST** have:
- `name` = `"filesystem.read"`
- `allowed_root` (string, valid path)
- `max_read_bytes` (integer, > 0)

Future capabilities (e.g., `network.http`) will have their own required fields.

### Unknown Capability Rejection

If `cap.name` is not in `capabilities::builtin` constants, `Sandbox` startup **MUST fail** (fail-closed). This is the same principle as Phase 1 execution failures applied to config loading.

### Fail-Closed on Config Load

Any of the following → `Sandbox` startup fails:
- Config file doesn't exist
- Config file doesn't parse (invalid TOML/YAML)
- Capability declares unknown type
- Required fields missing
- `allowed_root` is not a valid absolute path
- `max_read_bytes` is 0 or negative

---

## Integration with `Sandbox`

### New Constructor: `Sandbox::from_config(path: &str)`

```rust
impl Sandbox {
    /// Creates a sandbox by loading capabilities from a declarative config file.
    ///
    /// Fail-closed: if the config file doesn't parse, declares unknown capability,
    /// or misses required fields, returns an error. No empty/partial policy fallback.
    pub fn from_config(config_path: &str) -> Result<Self> {
        let policy_config = PolicyConfig::load(config_path)?;
        let capabilities = policy_config.validate()?.try_into_capabilities()?;
        let sandbox = Self::new_with_limits(SandboxConfig::default())?;
        // Store capabilities for host function access
        sandbox.store_mut().data_mut().capabilities = capabilities.clone();
        Ok(sandbox)
    }
}
```

### `PolicyConfig` Struct (new module)

```rust
#[derive(Debug, Deserialize)]
pub struct PolicyConfig {
    pub capabilities: Vec<CapabilityDef>,
}

#[derive(Debug, Deserialize)]
pub struct CapabilityDef {
    pub name: String,
    pub allowed_root: Option<String>,
    pub max_read_bytes: Option<u64>,
    // Future capabilities add their own fields
}
```

### Validation Flow

```
Config File → TOML Parse → PolicyConfig → validate() → try_into_capabilities() → Vec<Capability>
                                    │                │                    │
                                    │                │                    └→ Vec<CapabilityConfig>
                                    │                └→ Unknown capability? → Error
                                    └→ Missing required field? → Error
                                    └→ Parse error? → Error
```

---

## Error Types for Config Failures

Need a distinct `ConfigError` type, separate from runtime `Trap` errors. Using `thiserror` (already a dependency):

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Config file not found: {0}")]
    NotFound(String),

    #[error("Failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Missing required field '{field}' for capability '{capability}'")]
    MissingField { capability: String, field: String },

    #[error("Unknown capability type '{name}'. Must be one of: {allowed:?}")]
    UnknownCapability { name: String, allowed: Vec<String> },

    #[error("Invalid path '{path}' for capability '{capability}'")]
    InvalidPath { capability: String, path: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

**Key distinction**: `ConfigError` is returned from `Sandbox::from_config()` and means the sandbox **never starts**. This is different from `wasmtime::Error`/`Trap` which occur at runtime during execution.

---

## Test Strategy

### Unit Tests (config parsing)

| Test | Description | Expected |
|------|-------------|----------|
| `valid_single_capability` | Parse TOML with one `filesystem.read` | Success, `Capability` created correctly |
| `valid_multi_capability` | Parse TOML with `filesystem.read` + `network.http` | Success, both capabilities parsed |
| `invalid_yaml_syntax` | Invalid TOML (e.g., unclosed bracket) | `ConfigError::ParseError` |
| `missing_required_field` | `filesystem.read` without `allowed_root` | `ConfigError::MissingField` |
| `unknown_capability` | Capability name not in `builtin` constants | `ConfigError::UnknownCapability` |
| `config_not_found` | Path to nonexistent file | `ConfigError::NotFound` |
| `zero_max_read_bytes` | `max_read_bytes = 0` | `ConfigError` (or validation error) |
| `invalid_allowed_root` | Relative path or empty string | `ConfigError::InvalidPath` |

### Integration Tests (sandbox startup)

| Test | Description | Expected |
|------|-------------|----------|
| `sandbox_from_config_valid` | `Sandbox::from_config()` with valid file | Sandbox created, capabilities loaded |
| `sandbox_from_config_invalid` | `Sandbox::from_config()` with invalid file | Error, sandbox not created |
| `multi_cap_sandbox_starts` | Config with multiple capabilities | Sandbox starts, all capabilities registered |
| `fail_closed_on_unknown_cap` | Config with unknown capability name | Sandbox startup fails |

### Test File Organization

- `tests/config_parsing.rs` — Unit tests for config parsing and validation
- `tests/sandbox_config.rs` — Integration tests for `Sandbox::from_config()`
- Config fixture files in `tests/fixtures/config/` — `valid.toml`, `invalid.toml`, `missing_fields.toml`, `unknown_capability.toml`

---

## Key Design Decisions

### 1. Config Format: TOML

TOML is the Rust ecosystem convention. The `toml` crate provides `Deserialize` derivation that maps directly to our `PolicyConfig` struct with zero boilerplate. The flat structure of capability definitions is a natural fit for TOML's table array syntax (`[[capabilities]]`).

### 2. Config Loading is Fail-Closed

No fallback to defaults or partial policies. If the config is invalid for **any** reason, the `Sandbox` never starts. This is the same fail-closed principle from Phase 1 (all violations trap) applied to the config layer.

### 3. Multi-Capability from Day One

The config format uses `Vec<CapabilityDef>` from the start. Even though Phase 2 only implements `filesystem.read`, the format and parser must support multiple capabilities. When `network.http` arrives in Phase 3, no format change is needed — just add the new fields to `CapabilityDef` and the validation logic.

### 4. `Capability` Struct Shape is Untouched

The existing `Capability { name: String, params: HashMap<String, serde_json::Value> }` stays the same. Config parsing produces `Vec<Capability>` which feeds into the existing `Sandbox::instantiate_with_capabilities()` pipeline. The `From<&Capability>` → `CapabilityConfig` conversion continues to work unchanged.

### 5. PolicyEngine as Future Bridge

The existing `PolicyEngine` in `src/policy/mod.rs` is not wired to `Sandbox` yet. In Phase 2, the config loading could optionally feed into `PolicyEngine` for runtime policy evaluation. The `PolicyConfig` structure mirrors `Policy` (both have a list of rules/capabilities). This creates a natural extension path: config → `PolicyEngine` → `Sandbox` enforcement.

---

## Risks

1. **TOML crate dependency adds transitive deps**: The `toml` crate pulls in `serde` and `toml_datetime`. Since `serde` is already a direct dependency, this should be minimal. Verify `cargo tree` doesn't introduce conflicts.

2. **Config path resolution**: Relative paths in `allowed_root` need canonicalization. The `CapabilityConfig::from` already handles this via `std::fs::canonicalize`, but config loading should validate that `allowed_root` is an absolute path before passing to `Capability`.

3. **`serde_json::Value` vs TOML types**: The `Capability.params` field uses `serde_json::Value`, but TOML parsing produces different types. Need a conversion layer (e.g., parse TOML → `serde_json::Value` or parse TOML → `PolicyConfig` → `Capability` directly).

4. **Config file permissions**: A config file with insecure permissions (world-readable) could leak capability paths. This is a Phase 2+ concern but should be noted.

5. **Phase 2 `network.http` fields don't exist yet**: The `CapabilityDef` struct needs to be extensible. Using `HashMap<String, serde_json::Value>` for extra fields (like `Capability.params`) would allow future capabilities without struct changes, but loses type safety. A hybrid approach: typed fields for `filesystem.read` + a `params` hashmap for unknown future fields.

---

## Ready for Proposal

**Yes** — The exploration is complete. The path is clear:

1. Add `toml` crate to `Cargo.toml`
2. Create `src/config/` module with `PolicyConfig`, `CapabilityDef`, and `ConfigError`
3. Implement `PolicyConfig::load(path)` → parse TOML → validate → produce `Vec<Capability>`
4. Add `Sandbox::from_config(path)` constructor
5. Write comprehensive tests for parsing, validation, and fail-closed scenarios
6. The `Capability` struct shape, `CapabilityConfig`, enforcement logic, and receipt system all remain unchanged

The orchestrator should tell the user to proceed with `sdd-propose`.
